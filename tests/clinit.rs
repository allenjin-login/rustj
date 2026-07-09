//! 集成闸门(Layer 4.9):javac 编带静态初始化器(`<clinit>`)的真实 Java,由 rustj 执行,
//! 验证首次 active use 触发超类→本类的 `<clinit>` 执行、只跑一次、失败 →
//! `ExceptionInInitializerError` / 再访问 → `NoClassDefFoundError`。需 `javac`(无则跳过)。
//!
//! 关键:**非 final** 静态字段(javac 必发 `<clinit>` putstatic,而非 `ConstantValue` 折叠),
//! 故 `static int v = 42` 的 42 经 `<clinit>` 写入,`getstatic` 方能读到。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::{ClassRegistry, Oop};
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 编译源(可含多个顶层类)并加载全部 `.class` 进注册表。
fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-cl-{pid}-{s}-{public_name}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功"))
                .expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(
                cf.constant_pool.get(m.name_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == name
            );
            let d = matches!(
                cf.constant_pool.get(m.descriptor_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == desc
            );
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 运行 `class_name.name(desc)` 至返回;异常则 panic。各调用自带新 `Vm`(同一 `reg`,
/// 故 `<clinit>` 状态跨调用保留),`<clinit>` 仅在首次 active use 触发。
fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let (result, _vm) = run_result(reg, class_name, name, desc);
    result.unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

/// 同 [`run`] 但保留结果(含 `Err`)+ 产出 `Vm`(供读堆上异常对象)。
fn run_result<'a>(
    reg: &'a ClassRegistry,
    class_name: &str,
    name: &str,
    desc: &str,
) -> (Result<Value, VmError>, Vm<'a>) {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp =
        Interpreter::new(&code.code, &lc.cf.constant_pool)
            .with_exception_table(&code.exception_table);
    let mut vm = Vm::new(reg);
    let result = interp.interpret_with(&mut frame, &mut vm);
    (result, vm)
}

fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

/// 断言结果为 `ThrownException`,其堆对象类名 == `expected`。
fn assert_throws_class(result: Result<Value, VmError>, vm: &Vm<'_>, expected: &str) {
    let Err(VmError::ThrownException(exc)) = result else {
        panic!("期望抛 ThrownException({expected}),得 {result:?}");
    };
    let heap = vm.heap();
    let Some(Oop::Instance(i)) = heap.get(exc) else {
        panic!("异常应为引导桩实例,引用 {exc:?}");
    };
    assert_eq!(i.class_name(), expected, "异常类名不符");
}

const SOURCE: &str = r#"
public class ClinitGate {
    static int v = 42;        // 非 final → <clinit> putstatic
    static int base = 10;
    static int derived;       // 由 static 块写入
    static int counter;       // 由 static 块自增
    static {
        counter++;
        derived = base + 5;   // <clinit> 内字段间引用
    }
    public static int getV() { return v; }
    public static int getCounter() { return counter; }
    public static int getDerived() { return derived; }
}

class Base {
    static int b = 100;
}

class Sub extends Base {
    static int s = b + 1;     // 依赖超类静态 b(须先初始化 Base)
    public static int getS() { return s; }
}

class Bad {
    static int x = boom();    // <clinit> 调用抛异常的方法
    static int boom() { return 1 / 0; }
    public static int getX() { return x; }
}
"#;

#[test]
fn static_initializer_runs_on_first_use() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ClinitGate");
    // <clinit> 执行前 getstatic 会读默认 0;此处经 invokestatic getV 触发初始化 → 42。
    assert_eq!(as_int(run(&reg, "ClinitGate", "getV", "()I")), 42);
}

#[test]
fn static_block_runs_and_field_refs_resolve() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ClinitGate");
    // derived = base + 5 = 15(<clinit> 内 getstatic base + 常量 + putstatic)。
    assert_eq!(as_int(run(&reg, "ClinitGate", "getDerived", "()I")), 15);
}

#[test]
fn clinit_runs_exactly_once() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ClinitGate");
    // 多次 active use:<clinit> 仍只跑一次(counter 仅自增一次)。
    assert_eq!(as_int(run(&reg, "ClinitGate", "getV", "()I")), 42);
    assert_eq!(as_int(run(&reg, "ClinitGate", "getCounter", "()I")), 1);
    assert_eq!(as_int(run(&reg, "ClinitGate", "getCounter", "()I")), 1);
}

#[test]
fn superclass_clinit_runs_before_subclass() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ClinitGate");
    // Sub.s = b + 1;须先初始化 Base(b=100)→ s = 101。
    assert_eq!(as_int(run(&reg, "Sub", "getS", "()I")), 101);
}

#[test]
fn failing_clinit_throws_exception_in_initializer_error() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ClinitGate");
    // Bad.<clinit> → boom() 抛 ArithmeticException → 包成 ExceptionInInitializerError。
    {
        let (r, vm) = run_result(&reg, "Bad", "getX", "()I");
        assert_throws_class(r, &vm, "java/lang/ExceptionInInitializerError");
    } // vm 释放,reg 借用归还
    // 失败后(Bad 已 Failed)再访问 → NoClassDefFoundError。
    let (r2, vm2) = run_result(&reg, "Bad", "getX", "()I");
    assert_throws_class(r2, &vm2, "java/lang/NoClassDefFoundError");
}
