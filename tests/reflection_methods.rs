//! 集成闸门(Layer 4.15a-3):**反射可执行成员 native — `getDeclaredMethods0`/`getDeclaredConstructors0`**。
//!
//! `Class.getDeclaredMethod(name, params...)`/`getDeclaredConstructor(params...)` 经
//! `privateGetDeclaredMethods`/`privateGetDeclaredConstructors`(同 fields 的 reflectionData CAS
//! 缓存 + Reflection.filterMethods/Constructors)调私有 native
//! `getDeclaredMethods0(Z)[Ljava/lang/reflect/Method;` / `getDeclaredConstructors0(Z)[...]Constructor;`,
//! 构造真 Method[]/Constructor[](`slot`=本类方法序,`parameterTypes`/`exceptionTypes` 为 Class[]
//! 经方法描述符解析)。searchMethods/searchConstructors 按 name+parameterTypes 匹配,故参数类型数组
//! 须精确。复用 part2 的 AccessibleObject clinit 引导 + ReflectionFactory.copyMethod/copyConstructor。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.lang.reflect.Method;
import java.lang.reflect.Constructor;
public class ReflectMc {
    // parseInt(String) public static → PUBLIC|STATIC == 9。searchMethods 按 name+parameterTypes 匹配。
    public static int parseIntMethodModifiers() {
        try {
            Method m = Integer.class.getDeclaredMethod("parseInt", String.class);
            return m.getModifiers();
        } catch (NoSuchMethodException e) {
            return -100;
        }
    }
    // getParameterCount() 真字节码读 parameterTypes.length → 1。
    public static int parseIntParamCount() {
        try {
            Method m = Integer.class.getDeclaredMethod("parseInt", String.class);
            return m.getParameterCount();
        } catch (NoSuchMethodException e) {
            return -100;
        }
    }
    // Integer(int) 构造器 → getParameterCount() == 1。
    public static int intValueCtorParamCount() {
        try {
            Constructor<?> c = Integer.class.getDeclaredConstructor(int.class);
            return c.getParameterCount();
        } catch (NoSuchMethodException e) {
            return -100;
        }
    }
}
"#;

/// **集成闸门**:Layer 4.15a-3 getDeclaredMethods0/Constructors0。
#[test]
fn class_declared_methods0_constructs_method_array() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "ReflectMc", &[]);
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("ReflectMc.class")).unwrap()).unwrap())
        .unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for c in [
        "java/lang/Class",
        "java/lang/Integer",
        "java/lang/String",
        "java/lang/reflect/Method",
        "java/lang/reflect/Constructor",
        "java/lang/reflect/AccessibleObject",
        "java/lang/NoSuchMethodException",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/access/SharedSecrets",
        "jdk/internal/misc/Unsafe",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // getDeclaredMethod("parseInt", String.class) 命中,modifiers 9 = PUBLIC|STATIC。
    assert_eq!(
        run_static_int(&mut vm, "ReflectMc", "parseIntMethodModifiers"),
        Ok(9),
        "parseInt(String) modifiers 须为 PUBLIC|STATIC == 9"
    );
    // getParameterCount() == 1(参数类型数组长度)。
    assert_eq!(
        run_static_int(&mut vm, "ReflectMc", "parseIntParamCount"),
        Ok(1),
        "parseInt(String) 参数个数须为 1"
    );
    // getDeclaredConstructor(int.class).getParameterCount() == 1。
    assert_eq!(
        run_static_int(&mut vm, "ReflectMc", "intValueCtorParamCount"),
        Ok(1),
        "Integer(int) 构造器参数个数须为 1"
    );
}
