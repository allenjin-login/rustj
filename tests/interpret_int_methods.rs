//! 集成测试(执行闸门):用 `javac` 编译真实 Java 类,解析其 `.class`,
//! 再用 rustj 解释器**真正执行**其中的 `static int` 方法,断言结果与 JVM 一致。
//!
//! 这是 Layer 3.1 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//! 方法刻意只用 int 子集(算术 / iload-istore / iinc / if* / if_icmp* / goto / ireturn)。

use std::path::PathBuf;
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::runtime::{Frame, Interpreter, Value};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn compile(source: &str, class_name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rustj-exec-{}-{class_name}", std::process::id()));
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

/// 执行静态 int 方法 `name{desc}`,实参按顺序写入局部变量 0..,返回 int 结果。
fn run_static_int(cf: &ClassFile, name: &str, desc: &str, args: &[i32]) -> i32 {
    let method = find_method(cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    for (i, &arg) in args.iter().enumerate() {
        frame.locals.set_int(i as u16, arg).unwrap();
    }
    let interp = Interpreter::new(&code.code, &cf.constant_pool);
    match interp.interpret(&mut frame) {
        Ok(Value::Int(v)) => v,
        Ok(other) => panic!("{name} 返回非 int:{other:?}"),
        Err(e) => panic!("{name} 执行失败:{e}"),
    }
}

/// 实参:支持 int/long/float/double,按 JVM 调用约定(long/double 占两槽)写入局部变量。
enum Arg {
    I(i32),
    L(i64),
    F(f32),
    D(f64),
}

/// 执行静态方法,按实参类型与槽位约定写入局部变量,返回结果值。
fn run_static_value(cf: &ClassFile, name: &str, desc: &str, args: &[Arg]) -> Value {
    let method = find_method(cf, name, desc);
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
    let interp = Interpreter::new(&code.code, &cf.constant_pool);
    interp
        .interpret(&mut frame)
        .unwrap_or_else(|e| panic!("{name} 执行失败:{e}"))
}

#[test]
fn executes_real_static_int_methods() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }

    let source = r#"
public class IntMath {
    public static int add(int a, int b) {
        return a + b;
    }
    public static int factorial(int n) {
        int r = 1;
        for (int i = 2; i <= n; i++) r *= i;
        return r;
    }
    public static int fib(int n) {
        if (n < 2) return n;
        int a = 0, b = 1;
        for (int i = 2; i <= n; i++) {
            int t = a + b;
            a = b;
            b = t;
        }
        return b;
    }
    public static int gcd(int a, int b) {
        while (b != 0) {
            int t = b;
            b = a % b;
            a = t;
        }
        return a;
    }
}
"#;
    let class_path = compile(source, "IntMath");
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");

    // add
    assert_eq!(run_static_int(&cf, "add", "(II)I", &[3, 4]), 7);
    assert_eq!(run_static_int(&cf, "add", "(II)I", &[-5, 10]), 5);

    // factorial
    assert_eq!(run_static_int(&cf, "factorial", "(I)I", &[0]), 1);
    assert_eq!(run_static_int(&cf, "factorial", "(I)I", &[1]), 1);
    assert_eq!(run_static_int(&cf, "factorial", "(I)I", &[5]), 120);
    assert_eq!(run_static_int(&cf, "factorial", "(I)I", &[6]), 720);

    // fib
    assert_eq!(run_static_int(&cf, "fib", "(I)I", &[0]), 0);
    assert_eq!(run_static_int(&cf, "fib", "(I)I", &[1]), 1);
    assert_eq!(run_static_int(&cf, "fib", "(I)I", &[10]), 55);
    assert_eq!(run_static_int(&cf, "fib", "(I)I", &[20]), 6765);

    // gcd
    assert_eq!(run_static_int(&cf, "gcd", "(II)I", &[48, 36]), 12);
    assert_eq!(run_static_int(&cf, "gcd", "(II)I", &[17, 5]), 1);
    assert_eq!(run_static_int(&cf, "gcd", "(II)I", &[100, 0]), 100);
}

#[test]
fn executes_real_numeric_methods() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }

    let source = r#"
public class NumMath {
    public static long longAdd(long a, long b) {
        return a + b;
    }
    public static long factorialLong(int n) {
        long r = 1;
        for (long i = 2; i <= n; i++) r *= i;
        return r;
    }
    public static double avg(int a, int b) {
        return (a + b) / 2.0;
    }
    public static double distanceSquared(double dx, double dy) {
        return dx * dx + dy * dy;
    }
    public static float sumFloat(float a, float b) {
        return a + b;
    }
}
"#;
    let class_path = compile(source, "NumMath");
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");

    // longAdd:超过 int 范围,证明是 long
    assert_eq!(
        run_static_value(&cf, "longAdd", "(JJ)J", &[Arg::L(1_000_000_000), Arg::L(2_000_000_000)]),
        Value::Long(3_000_000_000)
    );

    // factorialLong(20) = 2432902008176640000(long 累乘 + i2l 比较)
    assert_eq!(
        run_static_value(&cf, "factorialLong", "(I)J", &[Arg::I(20)]),
        Value::Long(2_432_902_008_176_640_000)
    );

    // avg(3, 4) = 3.5(int 算术 + i2d 提升 + ddiv)
    match run_static_value(&cf, "avg", "(II)D", &[Arg::I(3), Arg::I(4)]) {
        Value::Double(v) => assert!((v - 3.5).abs() < 1e-9),
        other => panic!("avg 返回非 double:{other:?}"),
    }

    // distanceSquared(3.0, 4.0) = 25.0
    match run_static_value(&cf, "distanceSquared", "(DD)D", &[Arg::D(3.0), Arg::D(4.0)]) {
        Value::Double(v) => assert!((v - 25.0).abs() < 1e-9),
        other => panic!("distanceSquared 返回非 double:{other:?}"),
    }

    // sumFloat(1.5, 2.5) = 4.0
    match run_static_value(&cf, "sumFloat", "(FF)F", &[Arg::F(1.5), Arg::F(2.5)]) {
        Value::Float(v) => assert!((v - 4.0).abs() < 1e-6),
        other => panic!("sumFloat 返回非 float:{other:?}"),
    }
}
