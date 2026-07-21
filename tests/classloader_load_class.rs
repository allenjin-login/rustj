//! 集成闸门(Layer 4.31):**`ClassLoader.findLoadedClass0(String)Class` native**。
//!
//! 4.30 安装 `SharedSecrets.javaLangAccess` 后 `getSystemClassLoader()` 返非 null,但
//! `cl.loadClass(name)` 链阻塞于:`ClassLoader.loadClass:490` → `BuiltinClassLoader.loadClass:578`
//! → `loadClassOrNull:592` → `findLoadedClass:1264` → `findLoadedClass0`(native,ULE)。
//! `findLoadedClass0`(`ClassLoader.java:1270 private final native`)移植 `JVM_FindLoadedClass`:
//! 按 binary name 查"本 loader 已加载"集 → 返 Class 镜像或 null。rustj 单注册表模型:`name →
//! registry.get(intern) → intern_class_mirror 或 null`(忽略 per-loader 隔离;已加载类即返)。
//! 解锁 `loadClass` 的**已加载类快速路径**(`findLoadedClass` 命中即返,不进 module/parent 委派)。

use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::VmThread;
use rustj::testkit::*;

// cl.loadClass("java.lang.String"):触发 loadClass→findLoadedClass→findLoadedClass0。
// String 已在注册表(闭包预载)→ findLoadedClass0 命中返 String Class → 与 String.class 同一。
const LOAD_PROBE_SOURCE: &str = r#"
public class LoadProbe {
    public static int loadStringIdentity() throws Exception {
        Class<?> c = ClassLoader.getSystemClassLoader().loadClass("java.lang.String");
        return (c == String.class) ? 1 : 0;
    }
}
"#;

/// **集成闸门**(Layer 4.31):`findLoadedClass0` 命中注册表 → `loadClass("java.lang.String")`
/// 返 String Class(与 `String.class` 同一)。修前抛 UnsatisfiedLinkError(findLoadedClass0 未登记)。
#[test]
fn find_loaded_class0_supports_loadclass_already_loaded() {
    require_javac!();
    require_javabase!(jmod);

    let mut registry = compile_and_load(LOAD_PROBE_SOURCE, "LoadProbe");

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassLoader").unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    assert_eq!(
        run_static_int(&mut vm, "LoadProbe", "loadStringIdentity"),
        Ok(1),
        "loadClass(\"java.lang.String\") 须返与 String.class 同一的 Class(findLoadedClass0 命中注册表)"
    );
}
