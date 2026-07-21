//! 集成闸门(Phase B.5.3 / Layer 4.15b-field 收尾):**`Field.get`/`Field.set` 端到端** —— 经真
//! java.base 字节码路径(`Field.get`→`getFieldAccessor`→`ReflectionFactory.newFieldAccessor`→
//! `MethodHandleAccessorFactory.newFieldAccessor`→`JLIA.unreflectField`→DMH→`getter.invokeExact`)
//! 验证字段反射。前置:B.5.1(DMH 创建)+ B.5.2(MH invoke 钩子)+ ConstantValue 属性(B.5.3 前置,
//! `7c21d07`)。需 `javac` + 本机 jmod;缺一跳过。
//!
//! **关键路径分歧**:`MethodHandleIntegerFieldAccessorImpl.fieldAccessor` 对 getter 做
//! `asType`——**静态**字段 getter 类型 `()I`,`asType(()I)` 命中 `newType==type` 快路径返 `this`
//! (DMH 不变)→ B.5.2 钩子直读 member getStatic(**ConstantValue 经此可见**)。**实例**字段 getter
//! 类型 `(DeclaringClass)I`,`asType((LObject;)I)` 非恒等 → `MethodHandleImpl.makePairwiseConvert`
//! 包一层(非 DMH)→ 钩子不命中 → 落「MethodHandle 直接调用」墙(顺延候选 g)。故本闸门静态全通、
//! 实例暂顺延(除非/直到钩子扩展解包 pairwiseConvert 包裹)。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{
    bootstrap_java_lang_invoke, bootstrap_module_system, initialize_system_class,
};
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.lang.reflect.Field;
public class Probe {
    public int x = 7;
    // 非最终静态字段:<clinit> putstatic 置值(settable)。
    public static int stat = 123;

    // 静态 final 常量(ConstantValue 属性):跨类读 Integer.MIN_VALUE,经 accessor asType(()I)
    // 恒等快路径返 DMH → B.5.2 钩子 getStatic → ConstantValue 经此可见。
    public static int staticFinalGet() throws Exception {
        Field f = Integer.class.getDeclaredField("MIN_VALUE");
        return (int) f.get(null);
    }

    // 非最终静态 getter:Probe.stat == 123(asType(()I) 恒等 → DMH getStatic)。
    public static int staticGet() throws Exception {
        Field f = Probe.class.getDeclaredField("stat");
        return (int) f.get(null);
    }

    // 非最终静态 setter:Field.set(Probe.stat, 999)(asType((I)V) 恒等 → DMH putStatic)。
    public static int staticSet() throws Exception {
        Field f = Probe.class.getDeclaredField("stat");
        f.set(null, 999);
        return Probe.stat;
    }

    // 实例字段 getter/setter:accessor 对 getter/setter 做 asType((LObject;)I)/((LObject;I)V)
    // 非恒等 → pairwiseConvert 包成 BoundMethodHandle(非 DMH)→ 钩子不命中,且 asType 路径触发
    // BoundMethodHandle.<clinit>→Class.isHidden 等 native。阻塞于「MethodHandle 直接调用」(顺延候选 g)。
    public static int instanceGet() throws Exception {
        Field f = Probe.class.getDeclaredField("x");
        Probe p = new Probe();
        return (int) f.get(p);
    }
    public static int instanceSet() throws Exception {
        Field f = Probe.class.getDeclaredField("x");
        Probe p = new Probe();
        f.set(p, 99);
        return p.x;
    }
}
"#;

fn setup_vm() -> Option<VmThread> {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return None;
    }
    let jmod = find_javabase_jmod()?;
    let dir = compile_dir(SOURCE, "Probe", &["--add-exports", "java.base/jdk.internal.access=ALL-UNNAMED"]);
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
        "java/lang/Object",
        "java/lang/reflect/Field",
        "java/lang/reflect/AccessibleObject",
        "java/lang/reflect/Modifier",
        "java/lang/invoke/MethodHandles",
        "java/lang/invoke/MethodHandleImpl",
        "java/lang/invoke/MethodHandle",
        "java/lang/invoke/DirectMethodHandle",
        "java/lang/invoke/MemberName",
        "java/lang/invoke/MethodHandleNatives",
        "jdk/internal/reflect/FieldAccessor",
        "jdk/internal/reflect/FieldAccessorImpl",
        "jdk/internal/reflect/MethodHandleFieldAccessorImpl",
        "jdk/internal/reflect/MethodHandleIntegerFieldAccessorImpl",
        "jdk/internal/reflect/MethodHandleAccessorFactory",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/LangReflectAccess",
        "jdk/internal/access/SharedSecrets",
        "jdk/internal/misc/Unsafe",
        "jdk/internal/misc/VM",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }
    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 应成功");
    bootstrap_java_lang_invoke(&mut vm).expect("Phase 3 lite 应成功");
    Some(vm)
}

/// **RED→GREEN**(Phase B.5.3):静态 `Field.get`/`Field.set` 经真 java.base 字节码路径
/// (Field→accessor→`asType` 恒等返 DMH→B.5.2 钩子 getStatic/putStatic)。覆盖:
/// (1) 跨类 `static final` 常量 `Integer.MIN_VALUE`(ConstantValue 属性)→ -2147483648;
/// (2) 本类非最终静态 `Probe.stat` get → 123;
/// (3) 本类非最终静态 `Field.set(null, 999)` putStatic → 999。
#[test]
fn field_get_set_static_end_to_end() {
    let Some(mut vm) = setup_vm() else { return };
    assert_eq!(
        run_static_int(&mut vm, "Probe", "staticFinalGet"),
        Ok(-2147483648),
        "Field.get(Integer.MIN_VALUE) 须经 accessor→DMH→ConstantValue 返 -2147483648"
    );
    assert_eq!(
        run_static_int(&mut vm, "Probe", "staticGet"),
        Ok(123),
        "Field.get(Probe.stat) 须返 123"
    );
    assert_eq!(
        run_static_int(&mut vm, "Probe", "staticSet"),
        Ok(999),
        "Field.set(null,999) 写 Probe.stat 后读回 999"
    );
}

/// **GREEN(Phase G.2.3/G.3)**:实例 `Field.get`/`Field.set` 端到端经真 java.base 字节码 + 完整
/// LambdaForm 解释。accessor 对实例 getter/setter 做非恒等 `asType((LObject;)I)` /
/// `((LObject;I)V)` → `MethodHandleImpl.makePairwiseConvert` 包成转换 BMH(Species_LL);其 LF
/// 读绑定底层 DMH(argL1)再 `invokeBasic(DMH, receiver)`;DMH 的 prepared 字段 LF 为
/// `fieldOffset → checkBase → UNSAFE → Unsafe.getInt(base, ord)`(`putInt` 同构)。解锁链:
/// `objectFieldOffset` native 返实例字段 ord → DMH `Accessor.fieldOffset` 存之 →
/// `invoke_method_ref` 路由 native(原抛 AME)→ `Unsafe.getInt` Instance 分支按 ord 直读实例槽。
#[test]
fn field_get_set_instance_end_to_end() {
    let Some(mut vm) = setup_vm() else { return };
    assert_eq!(run_static_int(&mut vm, "Probe", "instanceGet"), Ok(7), "Field.get(p) 读 Probe.x==7");
    assert_eq!(run_static_int(&mut vm, "Probe", "instanceSet"), Ok(99), "Field.set(p, 99) 写后读回 99");
}
