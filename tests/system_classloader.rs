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

use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::VmThread;
use rustj::testkit::*;

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

/// **集成闸门**(Layer 4.30):`initialize_system_class` 安装 `SharedSecrets.javaLangAccess`(经
/// `System.setJavaLangAccess()`)→ `AbstractClassLoaderValue.map` 不再 NPE → `ClassLoaders.<clinit>`
/// 全链通 → `getSystemClassLoader()` 返非 null。修前抛 ExceptionInInitializerError。
#[test]
fn system_classloader_non_null_after_jla_installed() {
    require_javac!();
    require_javabase!(jmod);

    let mut registry = compile_and_load(SYSCL_PROBE_SOURCE, "SysClProbe");

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassLoader").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    assert_eq!(
        run_static_int(&mut vm, "SysClProbe", "loaderNonNull"),
        Ok(1),
        "getSystemClassLoader 须返非 null(JLA 已安装)"
    );
}
