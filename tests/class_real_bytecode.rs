//! 集成闸门(Layer 4.12):**退役 `Oop::Class` → 真 `java/lang/Class` Instance**。
//!
//! 探针发现(见 roadmap spec 修订):JDK 25 的 `Class.getName/getClassLoader/getModule/
//! isArray/isPrimitive/getComponentType` 全为真字节码字段读,但旧 `Oop::Class` 镜像把所有
//! 方法调用路由到固定 native 表(invoke.rs:867/985)→ 非 native 表方法(如 `getName`)抛
//! `UnsatisfiedLinkError`。本闸门验证退役后:Class 镜像是真 `java/lang/Class` Instance,
//! 其真字节码字段读 + 新增 native(`getSuperclass`/`isAssignableFrom`/`isInstance`/…)经正常
//! 类链分派全部跑通。
//!
//! 每法成功返唯一正数,失配返负诊断(带实际值,便于定位)。需 `javac` + 本机 `java.base.jmod`;
//! 缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
public class Cb {
    // getName():真字节码字段读(name 预置,不落 initClassName)→ "java.lang.Integer"。
    public static int nameOk() {
        return Integer.class.getName().equals("java.lang.Integer") ? 1 : -1;
    }
    // getClassLoader():真字节码字段读(classLoader=null=Bootstrap)→ null。
    public static int loaderNull() {
        return Integer.class.getClassLoader() == null ? 2 : -2;
    }
    // getModule():真字节码字段读(4.14a:module=java.base Module 镜像)→ 非 null 且名 "java.base"。
    public static int moduleBase() {
        java.lang.Module m = Integer.class.getModule();
        return (m != null && m.getName().equals("java.base")) ? 3 : -3;
    }
    // getSuperclass():新增 native → Number 镜像;== Number.class(同 intern)。
    public static int superIsNumber() {
        return Integer.class.getSuperclass() == Number.class ? 4 : -4;
    }
    // isAssignableFrom(Class):新增 native → Number.isAssignableFrom(Integer)=true。
    public static int assignable() {
        return Number.class.isAssignableFrom(Integer.class) ? 5 : -5;
    }
    // isInstance(Object):新增 native。正例 `Object.isInstance(new Cb())`=true(Cb 是 Object);
    // 负例 `Cb.isInstance("x")`=false(String 非 Cb)。两例均真,返 6。避用 Integer.valueOf
    //(其 <clinit> 需 VM.savedProps 引导,与 Class 镜像核验无关)。
    public static int isInstanceOk() {
        boolean pos = Object.class.isInstance(new Cb());
        boolean neg = Cb.class.isInstance("x");
        return (pos && !neg) ? 6 : -6;
    }
    // isArray():真字节码(componentType!=null)→ int[] 为 true。
    public static int arrayIsArray() {
        return int[].class.isArray() ? 7 : -7;
    }
    // isPrimitive():真字节码(primitive 字段)→ int 为 true。
    public static int intIsPrimitive() {
        return int.class.isPrimitive() ? 8 : -8;
    }
    // getComponentType():真字节码字段读 → int[].class.getComponentType()==int.class。
    public static int componentIsInt() {
        return int[].class.getComponentType() == int.class ? 9 : -9;
    }
}
"#;

/// **集成闸门**:真 `java/lang/Class` 字节码字段读 + 新增 native。
#[test]
fn real_class_bytecode_on_real_mirror() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编 Cb;载入注册表。
    let dir = compile_dir(SOURCE, "Cb", &[]);
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("Cb.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载用到的真类(Integer→Number→Object→Class…)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in ["java/lang/Integer", "java/lang/Object"] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    assert!(
        !registry.get("java/lang/Class").unwrap().is_synthetic_stub(),
        "java/lang/Class 须为真类(退役前提)"
    );

    let mut vm = VmThread::new(registry);

    // 3) 每法断言:正数=成功。
    assert_eq!(run_static_in(&mut vm, "Cb", "nameOk", "()I"), Ok(Value::Int(1)), "getName 真字节码");
    assert_eq!(run_static_in(&mut vm, "Cb", "loaderNull", "()I"), Ok(Value::Int(2)), "getClassLoader 真字节码→null");
    assert_eq!(run_static_in(&mut vm, "Cb", "moduleBase", "()I"), Ok(Value::Int(3)), "getModule 真字节码→java.base");
    assert_eq!(run_static_in(&mut vm, "Cb", "superIsNumber", "()I"), Ok(Value::Int(4)), "getSuperclass native");
    assert_eq!(run_static_in(&mut vm, "Cb", "assignable", "()I"), Ok(Value::Int(5)), "isAssignableFrom native");
    assert_eq!(run_static_in(&mut vm, "Cb", "isInstanceOk", "()I"), Ok(Value::Int(6)), "isInstance native");
    assert_eq!(run_static_in(&mut vm, "Cb", "arrayIsArray", "()I"), Ok(Value::Int(7)), "isArray 真字节码");
    assert_eq!(run_static_in(&mut vm, "Cb", "intIsPrimitive", "()I"), Ok(Value::Int(8)), "isPrimitive 真字节码");
    assert_eq!(run_static_in(&mut vm, "Cb", "componentIsInt", "()I"), Ok(Value::Int(9)), "getComponentType 真字节码");
}
