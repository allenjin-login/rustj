//! 集成闸门(Layer 4.10w 候选):**真 `java.lang.String` 的 `substring` / `charAt` / `length`
//! 端到端**(经 javac 编的真字节码)。
//!
//! 与仅测 `equals`/`hashCode`/`intern`(4.10i)不同:本闸门驱动 `substring`(经
//! `Arrays.copyOfRange` → `System.arraycopy` 分配新 String + Latin1 字节复制)与
//! `charAt`(Latin1 单字节读取)真字节码。预载真 String 闭包(String + Arrays + System 等)。
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
public class StrOps {
    // substring(0,5) of "hello world" → "hello" → 长度 5。
    public static int subLen() {
        String s = "hello world";
        String t = s.substring(0, 5);
        return t.length();
    }
    // charAt 累加:'j'(106) + 'a'(97) = 203。
    public static int charCode() {
        String s = "java";
        return s.charAt(0) + s.charAt(1);
    }
}
"#;

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-strops-{n}-{}-{public_name}", std::process::id()));
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
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn run_int(vm: &mut Vm<'_>, name: &str) -> Result<i32, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let lc = vm.registry().and_then(|r| r.get("StrOps")).expect("StrOps 须已加载");
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
        other => panic!("StrOps.{name} 应返 int,得 {other:?}"),
    }
}

/// **集成闸门**:真 String 的 substring/charAt/length。
#[test]
fn real_string_substring_and_charat() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_dir(SOURCE, "StrOps");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("StrOps.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载真 String 闭包(substring/charAt 跑真字节码;substring 经 Arrays.copyOfRange
    // → System.arraycopy,故闭包含 String/Arrays/System 等真类)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    let mut vm = Vm::new(&registry);
    let char_code = run_int(&mut vm, "charCode").unwrap_or_else(|e| {
        panic!("charCode 运行失败(真 String.charAt 链缺口):{e:?}")
    });
    assert_eq!(char_code, 203, "charAt(0)+charAt(1) = 'j'+'a' = 106+97");
    let sub_len = run_int(&mut vm, "subLen").unwrap_or_else(|e| {
        panic!("subLen 运行失败(真 String.substring 链缺口):{e:?}")
    });
    assert_eq!(sub_len, 5, "substring(0,5) of \"hello world\" 长度 5");
}
