//! 集成闸门(Layer 4.15b):**反射调用 — `Method.invoke` 端到端**。
//!
//! `Method.invoke(obj, args)` 经真 java.base 字节码路径 → `MethodAccessor`(惰性)→
//! `ReflectionFactory.newMethodAccessor` → `MethodHandleAccessorFactory.newMethodAccessor` →
//! `useNativeAccessor`(`!VM.isJavaLangInvokeInited()` → true,rustj 不跑 initPhase3)→
//! `DirectMethodHandleAccessor$NativeAccessor` → **native `invoke0`**(= HotSpot `JVM_InvokeMethod`)。
//! 绕过「MethodHandle 直接调用」墙。详见 spec `2026-07-11-layer-4.15b-reflection-invocation-design.md`。
//!
//! `Method.invoke` 的 `checkAccess` → `Reflection.verifyPublicMemberAccess` → `Module.isExported`
//! → `implIsExportedOrOpen` 读 java.base `Module.exportedPackages` 实例 Map——由 Layer 4.14c
//! `populate_module_exports`(bootstrap_module_system 末尾)据 `module-info` 的非限定 exports 填充。
//!
//! 覆盖:静态法(parseInt(String)→42)、实例法(String.length()→5)、重载+拆箱
//! (parseInt(String,int)→255,Integer 参拆箱)。需 `javac` + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.lang.reflect.Method;
public class Probe {
    // 静态法:Integer.parseInt(String) → 42(public static;不改 accessibility)。
    public static int invokeStatic() throws Exception {
        Method m = Integer.class.getDeclaredMethod("parseInt", String.class);
        return (int) m.invoke(null, "42");
    }
    // 实例法:String.length() → 5(虚分派 + 非空 receiver)。
    public static int invokeInstance() throws Exception {
        Method m = String.class.getDeclaredMethod("length");
        return (int) m.invoke("hello", new Object[0]);
    }
    // 重载 + 拆箱:parseInt(String, int) → parseInt("ff", 16) == 255(Integer 参拆箱)。
    public static int invokeOverload() throws Exception {
        Method m = Integer.class.getDeclaredMethod("parseInt", String.class, int.class);
        return (int) m.invoke(null, "ff", 16);
    }
}
"#;

/// **RED→GREEN**(Layer 4.15b):`Method.invoke` 经真字节码路径 + native `invoke0`(JVM_InvokeMethod)。
///
/// Layer 4.14c(`populate_module_exports` 填 java.base `Module.exportedPackages`)解锁了
/// `Method.invoke` 的 `checkAccess` → `Module.isExported` 访问检查,使本端到端闸门转绿。
#[test]
fn method_invoke_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "Probe", &[]);
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap())
        .unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for c in [
        "java/lang/Class",
        "java/lang/Integer",
        "java/lang/String",
        "java/lang/reflect/Method",
        "java/lang/reflect/AccessibleObject",
        "java/lang/reflect/InvocationTargetException",
        "java/lang/NoSuchMethodException",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/reflect/MethodHandleAccessorFactory",
        "jdk/internal/reflect/DirectMethodHandleAccessor",
        "jdk/internal/access/SharedSecrets",
        "jdk/internal/misc/Unsafe",
        "jdk/internal/misc/VM",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    assert_eq!(
        run_static_int(&mut vm, "Probe", "invokeStatic"),
        Ok(42),
        "Method.invoke 静态法:Integer.parseInt(\"42\") 须返 42"
    );
    assert_eq!(
        run_static_int(&mut vm, "Probe", "invokeInstance"),
        Ok(5),
        "Method.invoke 实例法:\"hello\".length() 须返 5"
    );
    assert_eq!(
        run_static_int(&mut vm, "Probe", "invokeOverload"),
        Ok(255),
        "Method.invoke 重载+拆箱:Integer.parseInt(\"ff\", 16) 须返 255"
    );
}
