//! 集成闸门(Layer 4.30):**`SharedSecrets.javaLangAccess` 在 Phase 1 引导期安装**。
//!
//! 4.29 越过 `Unsafe.getLong` 后,`ClassLoader.getSystemClassLoader()` 链阻塞于 `ClassLoaders.<clinit>`
//! → `ArchivedClassLoaders.archive` → `ServicesCatalog.getServicesCatalog` →
//! `AbstractClassLoaderValue.get/map` → `JLA.createOrGetClassLoaderValueMap(cl)`,抛
//! `ExceptionInInitializerError`(cause = NPE,因 `JLA == null`)。`JLA = SharedSecrets.getJavaLangAccess()`
//! 在真 JDK 由 `System.initPhase1`(`System.java:1774`)首步 `setJavaLangAccess()`(`System.java:1995`)
//! 安装:分配 `System$1` 匿名 `JavaLangAccess` 实例(~80 方法,**安装期不跑方法体**,仅惰性按需调用)
//! → `SharedSecrets.setJavaLangAccess(jla)`。rustj 的 Phase 1 引导(`initialize_system_class`)未跑此步,
//! 故 `javaLangAccess` 恒 null。
//!
//! 修法:在 `initialize_system_class` 增一步 `install_java_lang_access` —— 直接 `invokestatic
//! java/lang/System.setJavaLangAccess()V`(私有静态,rustj 不查访问控制,等同真实启动序列)。
//! 解锁 `getSystemClassLoader()` 整链 → 返非 null AppClassLoader。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
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
    None
}

const SYSCL_PROBE_SOURCE: &str = r#"
public class SysClProbe {
    // ClassLoader.getSystemClassLoader():触发 ClassLoaders.<clinit> → ArchivedClassLoaders.archive
    // → ServicesCatalog → AbstractClassLoaderValue.map → JLA.createOrGetClassLoaderValueMap。
    // JLA 已安装 → 返非 null AppClassLoader(1);修前 JLA null → ExceptionInInitializerError。
    public static int loaderNonNull() {
        ClassLoader cl = ClassLoader.getSystemClassLoader();
        return cl == null ? 0 : 1;
    }
}
"#;

fn run_static_int(vm: &mut Vm<'_>, class: &str, name: &str) -> Result<i32, String> {
    let lc = vm
        .registry()
        .and_then(|r| r.get(class))
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {class}.{name}()I"));
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("{o:?}"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

/// **集成闸门**(Layer 4.30):`initialize_system_class` 安装 `SharedSecrets.javaLangAccess`(经
/// `System.setJavaLangAccess()`)→ `AbstractClassLoaderValue.map` 不再 NPE → `ClassLoaders.<clinit>`
/// 全链通 → `getSystemClassLoader()` 返非 null。修前抛 ExceptionInInitializerError。
#[test]
fn system_classloader_non_null_after_jla_installed() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "rustj-syscl-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::new(0)
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SysClProbe.java"), SYSCL_PROBE_SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(dir.join("SysClProbe.java"))
        .output()
        .expect("javac 失败");
    assert!(
        out.status.success(),
        "javac 失败:{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut registry = ClassRegistry::new();
    registry
        .load(
            rustj::classfile::parse(&std::fs::read(dir.join("SysClProbe.class")).unwrap()).unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassLoader").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = Vm::new(&registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    assert_eq!(
        run_static_int(&mut vm, "SysClProbe", "loaderNonNull"),
        Ok(1),
        "getSystemClassLoader 须返非 null(JLA 已安装)"
    );
}
