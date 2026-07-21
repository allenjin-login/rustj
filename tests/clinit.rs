//! 集成闸门(Layer 4.9):javac 编带静态初始化器(`<clinit>`)的真实 Java,由 rustj 执行,
//! 验证首次 active use 触发超类→本类的 `<clinit>` 执行、只跑一次、失败 →
//! `ExceptionInInitializerError` / 再访问 → `NoClassDefFoundError`。需 `javac`(无则跳过)。
//!
//! 关键:**非 final** 静态字段(javac 必发 `<clinit>` putstatic,而非 `ConstantValue` 折叠),
//! 故 `static int v = 42` 的 42 经 `<clinit>` 写入,`getstatic` 方能读到。

use rustj::testkit::*;

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
    require_javac!();
    let reg = compile_and_load(SOURCE, "ClinitGate");
    let reg = std::sync::Arc::new(reg);
    // <clinit> 执行前 getstatic 会读默认 0;此处经 invokestatic getV 触发初始化 → 42。
    assert_eq!(as_int(run(&reg, "ClinitGate", "getV", "()I")), 42);
}

#[test]
fn static_block_runs_and_field_refs_resolve() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ClinitGate");
    let reg = std::sync::Arc::new(reg);
    // derived = base + 5 = 15(<clinit> 内 getstatic base + 常量 + putstatic)。
    assert_eq!(as_int(run(&reg, "ClinitGate", "getDerived", "()I")), 15);
}

#[test]
fn clinit_runs_exactly_once() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ClinitGate");
    let reg = std::sync::Arc::new(reg);
    // 多次 active use:<clinit> 仍只跑一次(counter 仅自增一次)。
    assert_eq!(as_int(run(&reg, "ClinitGate", "getV", "()I")), 42);
    assert_eq!(as_int(run(&reg, "ClinitGate", "getCounter", "()I")), 1);
    assert_eq!(as_int(run(&reg, "ClinitGate", "getCounter", "()I")), 1);
}

#[test]
fn superclass_clinit_runs_before_subclass() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ClinitGate");
    let reg = std::sync::Arc::new(reg);
    // Sub.s = b + 1;须先初始化 Base(b=100)→ s = 101。
    assert_eq!(as_int(run(&reg, "Sub", "getS", "()I")), 101);
}

#[test]
fn failing_clinit_throws_exception_in_initializer_error() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ClinitGate");
    let reg = std::sync::Arc::new(reg);
    // Bad.<clinit> → boom() 抛 ArithmeticException → 包成 ExceptionInInitializerError。
    {
        let (r, vm) = run_result(&reg, "Bad", "getX", "()I");
        assert_throws!(r, &vm, "java/lang/ExceptionInInitializerError");
    } // vm 释放,reg 借用归还
    // 失败后(Bad 已 Failed)再访问 → NoClassDefFoundError。
    let (r2, vm2) = run_result(&reg, "Bad", "getX", "()I");
    assert_throws!(r2, &vm2, "java/lang/NoClassDefFoundError");
}
