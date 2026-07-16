//! 集成闸门(Layer 4.4):javac 编译含 `== null`、引用相等、`switch(int)` 的真实 Java,
//! 解析 `.class` 由 rustj 执行,验证 ifnull/if_acmp*/tableswitch/lookupswitch 与 JVM 一致。
//!
//! 这是"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//!
//! 方法均为无参 static、内部自带数据,使 javac 仍编出目标指令并返回可断言的 int。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, VmThread};

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
    let dir = std::env::temp_dir().join(format!("rustj-cf-{}-{s}-{public_name}", std::process::id()));
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
            let n = match cf.constant_pool.get(m.name_index).unwrap() {
                ConstantPoolEntry::Utf8(s) => s == name,
                _ => false,
            };
            let d = match cf.constant_pool.get(m.descriptor_index).unwrap() {
                ConstantPoolEntry::Utf8(s) => s == desc,
                _ => false,
            };
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

fn run(reg: &std::sync::Arc<ClassRegistry>, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = VmThread::new(std::sync::Arc::clone(reg));
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
public class ControlFlow {
    // ifnull:new int[]{1,2,3} 非 null → 返回 a[0] = 1
    public static int nullCheck() {
        int[] a = new int[] { 1, 2, 3 };
        if (a == null) return 0;
        return a[0];
    }
    // if_acmpeq:同一引用比较 → 1
    public static int sameRef() {
        int[] a = new int[] { 5 };
        int[] b = a;
        if (a == b) return 1;
        return 0;
    }
    // tableswitch:密集,x=2 → 102
    public static int denseSwitch() {
        int x = 2;
        switch (x) {
            case 0: return 100;
            case 1: return 101;
            case 2: return 102;
            default: return -1;
        }
    }
    // lookupswitch:稀疏,x=100 → 2
    public static int sparseSwitch() {
        int x = 100;
        switch (x) {
            case 10: return 1;
            case 100: return 2;
            case 1000: return 3;
            default: return -1;
        }
    }
}
"#;

#[test]
fn ifnull_returns_element_when_nonnull() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "nullCheck", "()I"), Value::Int(1));
}

#[test]
fn if_acmpeq_same_reference() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "sameRef", "()I"), Value::Int(1));
}

#[test]
fn dense_switch_hits_case_2() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "denseSwitch", "()I"), Value::Int(102));
}

#[test]
fn sparse_switch_hits_case_100() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "sparseSwitch", "()I"), Value::Int(2));
}
