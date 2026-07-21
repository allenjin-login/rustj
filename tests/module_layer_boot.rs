//! 集成闸门(Layer 4.14b):**`ModuleLayer.boot()` + Phase 2 模块系统初始化**。
//!
//! 真 JVM 的 Phase 2(`System.initPhase2`,`System.java:1930`):`bootLayer = ModuleBootstrap.boot();`
//! 然后 `VM.initLevel(2)`(`MODULE_SYSTEM_INITED`)。本层把该引导收编为 VM 原生能力:
//! 分配真 `java/lang/ModuleLayer` Instance、置 `System.bootLayer` 静态字段、`VM.initLevel(2)`。
//!
//! `ModuleLayer.boot()` 仅 `return System.bootLayer;`(ModuleLayer.java:923,getstatic)。
//! `Module.getLayer()`(Module.java:232)对 java.base 特判——`loader==null && name=="java.base"`
//! 时返 `ModuleLayer.boot()`(java.base Module 在引导层之前创建,故 `layer` 字段保持 null,
//! 与真 JVM 同序)。故 `Integer.class.getModule().getLayer() == ModuleLayer.boot()` 成立。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class LayerGate {
    // ModuleLayer.boot():Phase2 后 System.bootLayer 已置 → 非 null。
    public static int bootNonNull() {
        return java.lang.ModuleLayer.boot() != null ? 1 : -1;
    }
    // Integer.class.getModule().getLayer():java.base 的 getLayer() 特判 → ModuleLayer.boot()。
    // 两侧均读 System.bootLayer → 同引用 → 相等。
    public static int baseModuleLayerIsBoot() {
        java.lang.ModuleLayer bl = java.lang.ModuleLayer.boot();
        java.lang.Module m = Integer.class.getModule();
        return (m != null && m.getLayer() == bl) ? 2 : -2;
    }
    // boot() 幂等:两次 boot() 返同一引导层引用。
    public static int bootStable() {
        return java.lang.ModuleLayer.boot() == java.lang.ModuleLayer.boot() ? 3 : -3;
    }
}
"#;

/// **集成闸门**:Layer 4.14b ModuleLayer.boot() + Phase 2。
#[test]
fn module_layer_boot_returns_boot_layer() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编 LayerGate;载入注册表。
    let mut registry = compile_and_load(SOURCE, "LayerGate");

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 ModuleLayer + System(boot 读 bootLayer 静态字段)
    //    + Integer/getLayer 链路真类。容器→模块由 load_closure 从 module-info 推导。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ModuleLayer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();

    let mut vm = VmThread::new(registry);
    // Phase 1(savedProps 引导,4.13)+ Phase 2(模块系统引导,4.14b)。
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // 3) ModuleLayer.boot() 非 null(System.bootLayer 已置)。
    assert_eq!(run_static_int(&mut vm, "LayerGate", "bootNonNull"), Ok(1), "boot() 须非 null");
    // 4) Integer 的模块所属层 == boot()层(java.base getLayer 特判 → boot())。
    assert_eq!(
        run_static_int(&mut vm, "LayerGate", "baseModuleLayerIsBoot"),
        Ok(2),
        "java.base Module.getLayer() 须 == ModuleLayer.boot()"
    );
    // 5) boot() 幂等(同 System.bootLayer 引用)。
    assert_eq!(run_static_int(&mut vm, "LayerGate", "bootStable"), Ok(3), "boot() 须幂等");
}
