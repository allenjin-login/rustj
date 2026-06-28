//! 集成闸门(Layer 4.8):javac 编含字符串字面量的真实 Java,由 rustj 执行,
//! 验证 `ldc`/`ldc_w` 取 `CONSTANT_String` → intern → 引用,且**同一字面量恒同引用**
//! (故 `"x" == "x"` 成立)。需 `javac`(无则跳过)。
//!
//! 不调用任何 String 方法(`.length()`/`.equals()` 等)——纯验证 intern 身份 + 文本落堆;
//! String 方法调用顺延到"加载真实 String 类"层。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::{ClassRegistry, Oop};
use rustj::runtime::{Frame, Interpreter, Value, Vm};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-sl-{pid}-{s}-{public_name}",
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

/// 运行 `class_name.name(desc)`,带其异常表(本闸门方法无 try/catch,表为空)。
fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
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
        Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

/// 运行返回 `String` 的方法,读回 intern 字符串的解码文本(查堆 `Oop::String`)。
fn run_string_text(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> String {
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
        Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let mut vm = Vm::new(reg);
    let v = interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"));
    let Value::Reference(r) = v else {
        panic!("期望 Value::Reference(String),得 {v:?}");
    };
    match vm.heap().get(r).unwrap() {
        Oop::String(s) => s.text().to_string(),
        other => panic!("期望 Oop::String,得 {other:?}"),
    }
}

fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

const SOURCE: &str = r#"
public class StringGate {
    // 1. 返回字符串字面量:ldc "hello" + areturn → intern 引用
    public static String greet() {
        return "hello";
    }

    // 2. 同字面量 == :ldc + ldc + if_acmpeq;JLS 不把引用 == 视为常量,javac 不折叠
    public static boolean sameLiteral() {
        return "x" == "x";
    }

    // 3. 经局部变量承载同一字面量:确保走 ldc + if_acmpeq(锁定 intern 语义)
    public static boolean sameViaLocal() {
        String a = "x";
        String b = "x";
        return a == b;
    }

    // 4. 不同字面量 != :intern 给出不同引用
    public static boolean diffLiteral() {
        return "a" == "b";
    }
}
"#;

#[test]
fn greet_returns_interned_string_with_text() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "StringGate");
    assert_eq!(
        run_string_text(&reg, "StringGate", "greet", "()Ljava/lang/String;"),
        "hello"
    );
}

#[test]
fn same_literal_is_equal() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "StringGate");
    assert_eq!(as_int(run(&reg, "StringGate", "sameLiteral", "()Z")), 1);
}

#[test]
fn same_literal_via_local_is_equal() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "StringGate");
    assert_eq!(as_int(run(&reg, "StringGate", "sameViaLocal", "()Z")), 1);
}

#[test]
fn different_literals_are_not_equal() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "StringGate");
    assert_eq!(as_int(run(&reg, "StringGate", "diffLiteral", "()Z")), 0);
}
