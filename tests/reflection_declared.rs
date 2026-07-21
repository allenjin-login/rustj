//! 集成闸门(Layer 4.15a-2):**反射成员元数据 native — `getDeclaredFields0`**。
//!
//! `Class.getDeclaredFields()`(Class.java:2249)= `copyFields(privateGetDeclaredFields(false))`。
//! `privateGetDeclaredFields`(Class.java:2914)经 `reflectionData()` 缓存(首次 `SoftReference`→
//! null → `newReflectionData` 的 `Atomic.casReflectionData`/Unsafe CAS)后调私有 native
//! `getDeclaredFields0(Z)[Ljava/lang/reflect/Field;`(jmod 实测描述符),再 `Reflection.filterFields`
//! (java.base 类不在 fieldFilterMap → 透传),最后 `copyFields`/`ReflectionFactory.copyField`。
//!
//! 本闸门驱动整条 `java.lang.reflect` 引导链:AccessibleObject `<clinit>`(置 SharedSecrets)→
//! ReflectionFactory 单例 → ReflectAccess.copyField → Field.copy + Unsafe 引用 CAS。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.lang.reflect.Field;
public class ReflectDecl {
    // getDeclaredFields() → 整条 copyFields/privateGetDeclaredFields/getDeclaredFields0 链。
    public static int declaredFieldsNonEmpty() {
        Field[] fs = Integer.class.getDeclaredFields();
        return fs.length > 0 ? 1 : -1;
    }
    // getDeclaredField("value") → searchFields + 同链;Integer.value = private final int
    // (ACC_PRIVATE|ACC_FINAL == 18);Field.getName() 真字节码读 name 字段。
    public static int valueFieldModifiers() {
        try {
            Field f = Integer.class.getDeclaredField("value");
            return f.getName().equals("value") ? f.getModifiers() : -2;
        } catch (NoSuchFieldException e) {
            return -100;
        }
    }
}
"#;

/// **集成闸门**:Layer 4.15a-2 getDeclaredFields0 反射成员元数据。
#[test]
fn class_declared_fields0_constructs_field_array() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编 ReflectDecl;载入注册表。
    let dir = compile_dir(SOURCE, "ReflectDecl", &[]);
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("ReflectDecl.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载反射链所需的真 java.base 类。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for c in [
        "java/lang/Class",
        "java/lang/Integer",
        "java/lang/String",
        "java/lang/reflect/Field",
        "java/lang/reflect/AccessibleObject",
        "java/lang/NoSuchFieldException",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/access/SharedSecrets",
        "java/lang/ref/SoftReference",
        "jdk/internal/misc/Unsafe",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // 3) getDeclaredFields() 经整条链返非空 Field[]。
    assert_eq!(
        run_static_int(&mut vm, "ReflectDecl", "declaredFieldsNonEmpty"),
        Ok(1),
        "getDeclaredFields0 须返非空 Field[]"
    );
    // 4) getDeclaredField("value") 命中并读 modifiers(18 = PRIVATE|FINAL)。
    assert_eq!(
        run_static_int(&mut vm, "ReflectDecl", "valueFieldModifiers"),
        Ok(18),
        "Integer.value modifiers 须为 PRIVATE|FINAL == 18"
    );
}
