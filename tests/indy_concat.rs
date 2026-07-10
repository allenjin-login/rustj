//! 集成闸门(Layer 4.10u):**真 `invokedynamic`** 的字符串拼接(JDK 9+ 默认风格)。
//!
//! 与 `string_concat.rs`(用 `-XDstringConcat=inline` 退回 StringBuilder)不同:本闸门用
//! **默认 javac**,`s + s` / `"n=" + 7` 编为 `invokedynamic makeConcatWithConstants`,引导
//! 方法 `java/lang/invoke/StringConcatFactory.makeConcatWithConstants`,recipe 经
//! `` 占位 + 字面量。rustj 按引导方法 (类,名) 特判 → 按 recipe 拼接。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

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
public class IndyConcat {
    // 默认 javac(JDK9+)→ invokedynamic makeConcatWithConstants。
    public static int selfConcatLength() {
        String s = "abc";
        return (s + s).length();          // recipe  ;两动态 String → "abcabc" → 6
    }
    public static int mixedConcat() {
        return ("n=" + 7).length();       // recipe "n=";字面量 + int 占位 → "n=7" → 3
    }
}
"#;

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-indy-{n}-{}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    // 默认 javac:动态拼接 → invokedynamic(不传 -XDstringConcat=inline)。
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn run_int(vm: &mut Vm, name: &str) -> i32 {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("IndyConcat 须已加载");
    let lc = reg.get("IndyConcat").expect("IndyConcat 须已加载");
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
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => n,
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            panic!("IndyConcat.{name} 抛 Java 异常:{cls}(invokedynamic 拼接链有缺口)")
        }
        other => panic!("IndyConcat.{name} 应返 int,得 {other:?}"),
    }
}

/// **集成闸门**:真 invokedynamic 字符串拼接。
#[test]
fn invokedynamic_make_concat_with_constants() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_dir(SOURCE, "IndyConcat");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("IndyConcat.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载真 String 闭包(intern 结果须为真 String 实例)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    let mut vm = Vm::new(registry);
    assert_eq!(run_int(&mut vm, "selfConcatLength"), 6, "(s+s).length() 须为 6");
    assert_eq!(run_int(&mut vm, "mixedConcat"), 3, "(\"n=\"+7).length() 须为 3");
}
