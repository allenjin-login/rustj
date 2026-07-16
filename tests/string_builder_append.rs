//! 探测闸门(下一个真实 java.base 缺口):`java.lang.StringBuilder`。
//!
//! 若绿:证明真实 StringBuilder(String append + 长度)可端到端跑;若红:首个失败即下一实现层。
//! 需 `javac` + 本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java")
            .join(ver)
            .join("jmods/java.base.jmod");
        if p.exists() {
            return Some(p);
        }
    }
    std::env::var("JAVA_HOME")
        .ok()
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

const SOURCE: &str = r#"
public class SbOps {
    // StringBuilder.append(String) ×2 → length 6。
    public static int appendLen() {
        StringBuilder sb = new StringBuilder();
        sb.append("foo");
        sb.append("bar");
        return sb.length();
    }
    // StringBuilder.append(int) → "42" → length 2。
    public static int appendIntLen() {
        StringBuilder sb = new StringBuilder();
        sb.append(42);
        return sb.length();
    }
}
"#;

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-sbops-{n}-{}-{public_name}", std::process::id()));
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
    assert!(out.status.success(), "javac 失败:\n{}", String::from_utf8_lossy(&out.stderr));
    dir
}

fn run_int(vm: &mut VmThread, name: &str) -> Result<i32, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("SbOps").expect("SbOps 须已加载");
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .unwrap();
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    match interp.interpret_with(&mut frame, vm)? {
        Value::Int(n) => Ok(n),
        other => panic!("SbOps.{name} 应返 int,得 {other:?}"),
    }
}

/// **探测**:真 StringBuilder append/length。
#[test]
fn real_string_builder() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_dir(SOURCE, "SbOps");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("SbOps.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载 StringBuilder 闭包(连带 AbstractStringBuilder/Arrays/System/Integer 等)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/StringBuilder").unwrap();

    let mut vm = VmThread::new(registry);
    let len = run_int(&mut vm, "appendLen").unwrap_or_else(|e| panic!("appendLen 失败:{e:?}"));
    assert_eq!(len, 6, "append(\"foo\")+append(\"bar\") → length 6");
    let ilen = run_int(&mut vm, "appendIntLen").unwrap_or_else(|e| panic!("appendIntLen 失败:{e:?}"));
    assert_eq!(ilen, 2, "append(42) → \"42\" → length 2");
}
