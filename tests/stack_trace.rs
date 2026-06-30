//! 集成闸门(Java 栈轨迹捕获):证抛出的异常携带调用链,`Vm::format_trace` 可回读。
//!
//! `class Trace { deep(){1/0} mid(){deep()} top(){mid()} }` —— `top` 经两层调用到 `deep`
//! 抛 `ArithmeticException`。捕获的栈轨迹须含 `deep`/`mid`/`top`,且**抛出帧(deep)在前**
//! (Java 惯例:最内帧首)。修前 `format_trace` 返空 → 红;捕获接通后 → 绿。
//! 需 `javac`(PATH);缺则跳过。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load() -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-trace-{}-{s}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("Trace.java");
    std::fs::write(&src, SOURCE).unwrap();
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
    reg.load(parse(&std::fs::read(dir.join("Trace.class")).unwrap()).expect("解析应成功"))
        .expect("加载应成功");
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

const SOURCE: &str = r#"
public class Trace {
    public static int deep() { return 1 / 0; }   // idiv 除零 → ArithmeticException
    public static int mid() { return deep(); }
    public static int top() { return mid(); }
}
"#;

/// 跑 `Trace.top` → 期望 `ThrownException`;回读 `format_trace` 验调用链。
#[test]
fn thrown_exception_carries_call_chain() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let reg = compile_and_load();
    let lc = reg.get("Trace").unwrap();
    let m = find_method(&lc.cf, "top", "()I");
    let code = m.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), "top");
    let mut vm = Vm::new(&reg);

    let err = interp
        .interpret_with(&mut frame, &mut vm)
        .expect_err("top() 应抛 ArithmeticException");
    let VmError::ThrownException(r) = err else {
        panic!("期望 ThrownException,得 {err:?}");
    };

    // 异常类正确。
    use rustj::oops::Oop;
    let cls = match vm.heap().get(r) {
        Some(Oop::Instance(i)) => i.class_name().to_string(),
        o => panic!("异常应为 Instance,得 {o:?}"),
    };
    assert_eq!(cls, "java/lang/ArithmeticException");

    // 栈轨迹含调用链;抛出帧(deep)须在 top 之前(最内帧首)。
    let trace = vm.format_trace(r);
    eprintln!("捕获栈轨迹:\n{trace}");
    let d = trace.find("deep").expect("轨迹应含 deep");
    let mi = trace.find("mid").expect("轨迹应含 mid");
    let t = trace.find("top").expect("轨迹应含 top");
    assert!(d < mi && mi < t, "调用链顺序应 deep→mid→top(最内帧首),得:\n{trace}");
}
