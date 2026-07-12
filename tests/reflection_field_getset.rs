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

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{
    bootstrap_java_lang_invoke, bootstrap_module_system, initialize_system_class,
};
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
        "rustj-fieldgetset-{n}-{}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .args(["--add-exports", "java.base/jdk.internal.access=ALL-UNNAMED"])
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

/// 经解释器在 `Probe` 上跑静态法 `name()I`,失败返异常类名(便于诊断)。
fn run_static_int(vm: &mut Vm, name: &str) -> Result<i32, String> {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表缺失"));
    let lc = reg
        .get("Probe")
        .unwrap_or_else(|| panic!("Probe 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 Probe.{name}()I"));
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let trace = vm.format_trace(r);
            Err(trace)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

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

fn setup_vm() -> Option<Vm> {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return None;
    }
    let jmod = find_javabase_jmod()?;
    let dir = compile_dir(SOURCE, "Probe");
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
    let mut vm = Vm::new(registry);
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
        run_static_int(&mut vm, "staticFinalGet"),
        Ok(-2147483648),
        "Field.get(Integer.MIN_VALUE) 须经 accessor→DMH→ConstantValue 返 -2147483648"
    );
    assert_eq!(
        run_static_int(&mut vm, "staticGet"),
        Ok(123),
        "Field.get(Probe.stat) 须返 123"
    );
    assert_eq!(
        run_static_int(&mut vm, "staticSet"),
        Ok(999),
        "Field.set(null,999) 写 Probe.stat 后读回 999"
    );
}

/// **RED(顺延候选 g)**:实例 `Field.get`/`Field.set`。accessor 对 getter/setter 做非恒等
/// `asType((LObject;)I)` / `((LObject;I)V)` → `MethodHandleImpl.makePairwiseConvert` 包成
/// `BoundMethodHandle`(非 DMH);asType 路径触发 `BoundMethodHandle.<clinit>`→`ClassSpecializer`
/// →`ConstantUtils.referenceClassDesc`→`Class.descriptorString`→`Class.isHidden`(native 缺)→
/// `ExceptionInInitializerError`;且即使越过,`invokeExact` 于 BMH 须解释 LambdaForm
/// (rustj 不解释)→ 阻塞于「MethodHandle 直接调用」(CLAUDE.md §9.4 候选 g)。待该层解锁后去 ignore。
#[test]
#[ignore = "实例 Field.get/set 阻塞于 MethodHandle 直接调用(顺延候选 g);静态已通见 field_get_set_static_end_to_end"]
fn field_get_set_instance_end_to_end() {
    let Some(mut vm) = setup_vm() else { return };
    assert_eq!(run_static_int(&mut vm, "instanceGet"), Ok(7), "Field.get(p) 读 Probe.x==7");
    assert_eq!(run_static_int(&mut vm, "instanceSet"), Ok(99), "Field.set(p, 99) 写后读回 99");
}
