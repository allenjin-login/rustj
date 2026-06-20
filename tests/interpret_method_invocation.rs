//! 集成测试(执行闸门):用 `javac` 编译含**方法调用**的真实 Java 类,解析其 `.class`,
//! 再用 rustj 解释器**真正执行**其中的方法,验证 `invokestatic` 的解析、实参传递、
//! 递归与跨栈帧 `*return` 与 JVM 一致。
//!
//! 这是 Layer 3.3 的"能否跑通真实字节码"判据(4.1 起改走 `ClassRegistry` + `Vm`)。
//! 需要 PATH 中有 `javac`(无则跳过)。方法刻意只用数值 + `invokestatic`(同类自调/递归/互调)。

use std::path::PathBuf;
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn compile(source: &str, class_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustj-invoke-{}-{class_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join(format!("{class_name}.java"));
    std::fs::write(&src, source).unwrap();

    let output = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        output.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    dir.join(format!("{class_name}.class"))
}

/// 编译 + 解析 + 加载进注册表(字段布局随之解析)。返回注册表。
fn compile_and_load(source: &str, class_name: &str) -> ClassRegistry {
    let class_path = compile(source, class_name);
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");
    let mut registry = ClassRegistry::new();
    registry.load(cf).expect("加载应成功");
    registry
}

fn utf8(cf: &ClassFile, index: u16) -> String {
    match cf.constant_pool.get(index).unwrap() {
        ConstantPoolEntry::Utf8(s) => s.clone(),
        e => panic!("expected Utf8 at {index}, got {e:?}"),
    }
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| utf8(cf, m.name_index) == name && utf8(cf, m.descriptor_index) == desc)
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 实参:支持 int/long/float/double,按 JVM 调用约定(long/double 占两槽)写入局部变量。
enum Arg {
    I(i32),
    L(i64),
    F(f32),
    D(f64),
}

/// 执行静态方法(支持 `invokestatic`):在注册表中定位方法,按实参类型与槽位约定
/// 写入局部变量,返回结果值。`Vm` 持同一注册表,递归调用共享之。
fn run(registry: &ClassRegistry, class_name: &str, name: &str, desc: &str, args: &[Arg]) -> Value {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));

    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let mut slot: u16 = 0;
    for a in args {
        match a {
            Arg::I(v) => {
                frame.locals.set_int(slot, *v).unwrap();
                slot += 1;
            }
            Arg::L(v) => {
                frame.locals.set_long(slot, *v).unwrap();
                slot += 2;
            }
            Arg::F(v) => {
                frame.locals.set_float(slot, *v).unwrap();
                slot += 1;
            }
            Arg::D(v) => {
                frame.locals.set_double(slot, *v).unwrap();
                slot += 2;
            }
        }
    }

    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(registry);
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

#[test]
fn executes_recursive_int_methods() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }

    let source = r#"
public class Recursion {
    public static int factorial(int n) {
        if (n <= 1) return 1;
        return n * factorial(n - 1);
    }
    public static int fib(int n) {
        if (n < 2) return n;
        return fib(n - 1) + fib(n - 2);
    }
    public static int square(int x) { return x * x; }
    public static int sumOfSquares(int a, int b) {
        return square(a) + square(b);
    }
    public static int ackermann(int m, int n) {
        if (m == 0) return n + 1;
        if (n == 0) return ackermann(m - 1, 1);
        return ackermann(m - 1, ackermann(m, n - 1));
    }
}
"#;
    let registry = compile_and_load(source, "Recursion");

    // 递归 invokestatic(单 int 实参/返回)
    assert_eq!(run(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(0)]), Value::Int(1));
    assert_eq!(run(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(1)]), Value::Int(1));
    assert_eq!(run(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(5)]), Value::Int(120));
    assert_eq!(run(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(10)]), Value::Int(3_628_800));

    // 双递归(同一方法自调两次后相加)
    assert_eq!(run(&registry, "Recursion", "fib", "(I)I", &[Arg::I(0)]), Value::Int(0));
    assert_eq!(run(&registry, "Recursion", "fib", "(I)I", &[Arg::I(1)]), Value::Int(1));
    assert_eq!(run(&registry, "Recursion", "fib", "(I)I", &[Arg::I(10)]), Value::Int(55));
    assert_eq!(run(&registry, "Recursion", "fib", "(I)I", &[Arg::I(15)]), Value::Int(610));

    // 互调:sumOfSquares 调 square(两个 int 实参)
    assert_eq!(run(&registry, "Recursion", "sumOfSquares", "(II)I", &[Arg::I(3), Arg::I(4)]), Value::Int(25));
    assert_eq!(run(&registry, "Recursion", "sumOfSquares", "(II)I", &[Arg::I(5), Arg::I(12)]), Value::Int(169));

    // 嵌套调用作为实参 + 多层递归(Ackermann:验证调用栈深度与返回值回填)
    assert_eq!(run(&registry, "Recursion", "ackermann", "(II)I", &[Arg::I(2), Arg::I(3)]), Value::Int(9));
    assert_eq!(run(&registry, "Recursion", "ackermann", "(II)I", &[Arg::I(3), Arg::I(3)]), Value::Int(61));
}

#[test]
fn executes_methods_with_mixed_numeric_args() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }

    let source = r#"
public class Mixed {
    // long 基 + int 指数:验证 cat-1/cat-2 实参的槽位布局。
    public static long pow(long base, int exp) {
        if (exp <= 0) return 1;
        return base * pow(base, exp - 1);
    }
    // int + long:实参类型顺序不同,槽位布局相反。
    public static long shift(int n, long amount) {
        if (n <= 0) return amount;
        return amount + shift(n - 1, amount);
    }
    // double 实参与返回:cat-2 实参传递 + cat-2 返回值压栈。
    public static double powDouble(double base, int exp) {
        if (exp <= 0) return 1;
        return base * powDouble(base, exp - 1);
    }
    // float 实参与返回。
    public static float powFloat(float base, int exp) {
        if (exp <= 0) return 1f;
        return base * powFloat(base, exp - 1);
    }
}
"#;
    let registry = compile_and_load(source, "Mixed");

    // pow(2, 10) = 1024:long 在 slot 0-1,int 在 slot 2
    assert_eq!(run(&registry, "Mixed", "pow", "(JI)J", &[Arg::L(2), Arg::I(10)]), Value::Long(1024));
    assert_eq!(run(&registry, "Mixed", "pow", "(JI)J", &[Arg::L(3), Arg::I(5)]), Value::Long(243));
    assert_eq!(run(&registry, "Mixed", "pow", "(JI)J", &[Arg::L(5), Arg::I(0)]), Value::Long(1));

    // shift(3, 100) = 100 + 100 + 100 + 100 = 400:int 在 slot 0,long 在 slot 1-2
    assert_eq!(run(&registry, "Mixed", "shift", "(IJ)J", &[Arg::I(3), Arg::L(100)]), Value::Long(400));

    // powDouble(2.0, 10) = 1024.0:double(cat-2)实参 + cat-2 返回
    match run(&registry, "Mixed", "powDouble", "(DI)D", &[Arg::D(2.0), Arg::I(10)]) {
        Value::Double(v) => assert!((v - 1024.0).abs() < 1e-9),
        other => panic!("powDouble 返回非 double:{other:?}"),
    }

    // powFloat(2.0, 5) = 32.0:float(cat-1)实参 + cat-1 返回
    match run(&registry, "Mixed", "powFloat", "(FI)F", &[Arg::F(2.0), Arg::I(5)]) {
        Value::Float(v) => assert!((v - 32.0).abs() < 1e-5),
        other => panic!("powFloat 返回非 float:{other:?}"),
    }
}
