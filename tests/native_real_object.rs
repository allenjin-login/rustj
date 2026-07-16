//! 集成闸门(4.10 capstone):用 `javac` 编译一个调用 `new Object().hashCode()` 的最小真
//! Java 程序,`Object` 从真实 `java.base.jmod` 经 `ClassPath` 加载并**覆盖合成桩**,
//! 再由 rustj 解释器执行——端到端验证「真容器 → 真类 → `<clinit>` → native 分派 → 身份哈希」
//! 全链(北极星:加载真实 java.base 的最小可运行证据)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 找本机首个 `java.base.jmod`;无则 `None`。
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

/// javac 编译单个 public 类到临时目录,返回该目录。
fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-native-{n}-{}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac").arg("-d").arg(&dir).arg(&src).output().expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// 按名+描述符在类中找方法。
fn find_method<'a>(cf: &'a rustj::metadata::ClassFile, cp: &rustj::constant_pool::ConstantPool, name: &str, desc: &str) -> &'a rustj::metadata::MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(cp.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(cp.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 解释执行一个静态方法。
fn run_static(registry: &std::sync::Arc<ClassRegistry>, class: &str, name: &str, desc: &str) -> Result<Value, VmError> {
    let lc = registry.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, &lc.cf.constant_pool, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = VmThread::new(std::sync::Arc::clone(registry));
    interp.interpret_with(&mut frame, &mut vm)
}

const SOURCE: &str = r#"
public class NativeGate {
    // 真 Object(从 jmod 载入覆盖桩)经 new → <clinit>(registerNatives 空)→ hashCode()。
    // 同一对象两次 hashCode 必相等(native 身份哈希 = 句柄 idx)→ 返回 1。
    public static int run() {
        Object o = new Object();
        int a = o.hashCode();
        int b = o.hashCode();
        return a == b ? 1 : 0;
    }
}
"#;

/// **capstone**:真 java.base 的 Object 经容器加载 + native 分派端到端跑通。
#[test]
fn real_object_hashcode_runs_via_native_dispatch() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 NativeGate;载入注册表(连同合成桩)。
    let dir = compile_dir(SOURCE, "NativeGate");
    let mut registry = ClassRegistry::new();
    let ng = parse(&std::fs::read(dir.join("NativeGate.class")).unwrap()).unwrap();
    registry.load(ng).unwrap();

    // 2) 真 Object 从 jmod 载入,**覆盖**合成桩。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let real_obj = cp
        .load_class("java/lang/Object")
        .unwrap()
        .expect("Object 须在 jmod 内")
        .0;
    registry.load_or_replace(real_obj).unwrap();
    let registry = std::sync::Arc::new(registry);

    // 3) 真 Object.hashCode 须为 ACC_NATIVE(桩无 hashCode → 证覆盖成功)。
    let obj_lc = registry.get("java/lang/Object").unwrap();
    let hash = find_method(&obj_lc.cf, &obj_lc.cf.constant_pool, "hashCode", "()I");
    assert!(hash.access_flags.is_native(), "真 Object.hashCode 须 native");

    // 4) 跑 NativeGate.run():new Object → <clinit> registerNatives(native 空操作)→
    //    hashCode()×2 → 同句柄同 idx → 相等 → 返回 1。
    assert_eq!(run_static(&registry, "NativeGate", "run", "()I").unwrap(), Value::Int(1));
}
