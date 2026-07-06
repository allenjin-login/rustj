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

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
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
        .map(|jh| PathBuf::from(jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-layer-{n}-{}-{public_name}",
        std::process::id()
    ));
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

/// 解释执行一个无参静态 int 方法(共用传入 Vm)。抛 Java 异常时把类名带出。
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
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance Oop:{o:?})"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

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
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编 LayerGate;载入注册表。
    let dir = compile_dir(SOURCE, "LayerGate");
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("LayerGate.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 ModuleLayer + System(boot 读 bootLayer 静态字段)
    //    + Integer/getLayer 链路真类。容器→模块由 load_closure 从 module-info 推导。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ModuleLayer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();

    let mut vm = Vm::new(&registry);
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
