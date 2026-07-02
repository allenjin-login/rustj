//! 探测闸门:int↔string 全往返(`StringBuilder.append(int).toString()`、`String.valueOf(int)`、
//! `Integer.parseInt`)。验证 4.10w putByte/getByte + DecimalDigits 链端到端。绿则证明 int↔string
//! 真实可用;红则首个失败即下一层。需 javac + java.base.jmod;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}
fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java").join(ver).join("jmods/java.base.jmod");
        if p.exists() { return Some(p); }
    }
    std::env::var("JAVA_HOME").ok().map(|jh| Path::new(&jh).join("jmods/java.base.jmod")).filter(|p| p.exists())
}

const SOURCE: &str = r#"
public class IntStr {
    // StringBuilder.append(int).toString() → "123" → 长度 3。
    public static int sbToStringLen() {
        return new StringBuilder().append(123).toString().length();
    }
    // String.valueOf(int) → "-5" → 长度 2。
    public static int valueOfLen() {
        return String.valueOf(-5).length();
    }
    // Integer.parseInt(String) → 42。
    public static int parse() {
        return Integer.parseInt("42");
    }
}
"#;

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-intstr-{n}-{}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac").arg("-d").arg(&dir).arg(&src).output().expect("javac 执行失败");
    assert!(out.status.success(), "javac 失败:\n{}", String::from_utf8_lossy(&out.stderr));
    dir
}

fn run_int(vm: &mut Vm<'_>, name: &str) -> Result<i32, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let lc = vm.registry().and_then(|r| r.get("IntStr")).expect("IntStr 须已加载");
    let method = lc.cf.methods.iter().find(|m| {
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap();
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    match interp.interpret_with(&mut frame, vm)? {
        Value::Int(n) => Ok(n),
        other => panic!("IntStr.{name} 应返 int,得 {other:?}"),
    }
}

/// **探测**:int↔string 全往返。
#[test]
fn int_string_round_trip() {
    if !javac_available() { eprintln!("跳过:无 javac"); return; }
    let Some(jmod) = find_javabase_jmod() else { eprintln!("跳过:无 java.base.jmod"); return; };

    let dir = compile_dir(SOURCE, "IntStr");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("IntStr.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // 预载 StringBuilder + Integer 闭包(连带 DecimalDigits/Arrays/System 等)。
    load_closure(&mut registry, &cp, "java/lang/StringBuilder").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

    let mut vm = Vm::new(&registry);
    let n = run_int(&mut vm, "sbToStringLen").unwrap_or_else(|e| panic!("sbToStringLen 失败:{e:?}"));
    assert_eq!(n, 3, "append(123).toString() → \"123\" 长度 3");
    let v = run_int(&mut vm, "valueOfLen").unwrap_or_else(|e| panic!("valueOfLen 失败:{e:?}"));
    assert_eq!(v, 2, "String.valueOf(-5) → \"-5\" 长度 2");
    let p = run_int(&mut vm, "parse").unwrap_or_else(|e| panic!("parse 失败:{e:?}"));
    assert_eq!(p, 42, "Integer.parseInt(\"42\") → 42");
}
