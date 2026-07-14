//! 集成闸门(Phase G.4 / G.4.1):真 `java.util.stream` 流水线端到端。
//!
//! **进度(G.4 全完成)**:`Stream.of(1,2,3).count()` == 3L ✅;`Stream.of(1,2,3,4)
//! .map(x->x*2).filter(x->x>4).count()` == 2L ✅;`Stream.of(1,2,3,4).reduce(0, Integer::sum)`
//! == 10 ✅(G.4.1 lambda 签名适配:原语方法引用的拆箱/装箱)。真 Stream 中间+终端操作经
//! invokedynamic lambda + 方法引用端到端通。
//!
//! 解锁墙(已修):(1) G.4 `Class.modifiers` 字段未由 VM 置 → `isEnum()` false →
//! `getEnumConstantsShared()` null → `EnumMap.<init>` NPE(StreamOpFlag.<clinit>)→ `populate_class_mirror_fields`
//! 置 `modifiers = cf.access_flags.bits()`;(2) G.4.1 `dispatch_lambda` 对 `Integer::sum` 等
//! 原语方法引用做 SAM 实参拆箱 + 原语返回装箱(对应 LambdaForm box/unbox 节点)。

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
        "rustj-stream-{n}-{}-{public_name}",
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

/// 经解释器跑 `Probe.<name>` 静态法,返 owned Value(支持 long 返回)。
fn run_static(vm: &mut Vm, name: &str, desc: &str) -> Result<Value, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("类注册表缺失");
    let lc = reg
        .get("Probe")
        .unwrap_or_else(|| panic!("Probe 未加载"));
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 Probe.{name}{desc}"));
    let code = method.code.as_ref().expect("应有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    interp.interpret_with(&mut frame, vm)
}

const SOURCE: &str = r#"
import java.util.stream.Stream;
public class Probe {
    public static long countThree() {
        return Stream.of(1, 2, 3).count();
    }
    public static long mapFilterCount() {
        return Stream.of(1, 2, 3, 4).map(x -> x * 2).filter(x -> x > 4).count();
    }
    public static int sumReduce() {
        return Stream.of(1, 2, 3, 4).reduce(0, Integer::sum);
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
        .load(
            rustj::classfile::parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // 预载 Stream 流水线骨架类(闭包传递性拉入其余)。
    for c in [
        "java/util/stream/Stream",
        "java/util/stream/AbstractPipeline",
        "java/util/stream/ReferencePipeline",
        "java/util/stream/StreamSupport",
        "java/util/Spliterators",
        "java/util/Arrays",
        "java/lang/invoke/MethodHandles",
        "java/lang/invoke/LambdaMetafactory",
        "java/lang/Integer",
        "java/util/HashMap",
        "jdk/internal/misc/VM",
        "java/lang/Object",
        "java/lang/System",
    ] {
        if let Err(e) = load_closure(&mut registry, &cp, c) {
            eprintln!("预载 {c} 失败:{e:?}");
        }
    }
    let mut vm = Vm::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 应成功");
    bootstrap_java_lang_invoke(&mut vm).expect("Phase 3 lite 应成功");
    Some(vm)
}

/// **GREEN(Phase G.4)**:`Stream.of(1,2,3).count()` == 3L。Stream 构造 + 终端操作端到端。
/// 解锁墙原为 `EnumMap.<init>` NPE:`Class.getEnumConstantsShared()`(Class.java:3434 字节码
/// 调 `isEnum()`+`getMethod("values")`+`Method.invoke`)返 null,因 `Class.modifiers` 字段
/// 未由 VM 置(默认 0)→ `isEnum()`(3365 `getModifiers()&ENUM`)返 false。修复:`populate_class_mirror_fields`
/// 置 `Class.modifiers = cf.access_flags.bits()`(含 ACC_ENUM)→ isEnum 真 → values() 经反射
/// 调用返枚举常量数组 → EnumMap keyUniverse 非空 → StreamOpFlag.<clinit> 通。
#[test]
fn stream_of_count_end_to_end() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static(&mut vm, "countThree", "()J") {
        Ok(Value::Long(v)) => assert_eq!(v, 3, "Stream.of(1,2,3).count() 须 3"),
        Ok(other) => panic!("期望 Long,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("countThree 抛异常:\n{trace}");
        }
        Err(e) => panic!("countThree 内部错误:{e:?}"),
    }
}

/// **GREEN(Phase G.4)**:中间操作 map/filter + 终端 count,经 invokedynamic lambda(`x->x*2`、`x->x>4`)。
/// `Stream.of(1,2,3,4).map(x->x*2).filter(x->x>4).count()` == 2L(2,4,6,8 → >4: 6,8)。
/// javac 为 lambda 生成装箱签名 synthetic,内部自行拆箱 → `dispatch_lambda` 直传 Integer 引用即通。
#[test]
fn stream_map_filter_count_end_to_end() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static(&mut vm, "mapFilterCount", "()J") {
        Ok(Value::Long(v)) => assert_eq!(v, 2, "Stream.of(1,2,3,4).map(x*2).filter(>4).count() 须 2"),
        Ok(other) => panic!("期望 Long,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("mapFilterCount 抛异常:\n{trace}");
        }
        Err(e) => panic!("mapFilterCount 内部错误:{e:?}"),
    }
}

/// **GREEN(Phase G.4.1)**:`reduce(0, Integer::sum)`(方法引用 + 归约)。
/// `Stream.of(1,2,3,4).reduce(0, Integer::sum)` == 10。
///
/// **解锁**:G.4.1 lambda 适配器签名适配。`Integer::sum` 为真 `(II)I` 原语方法;SAM
/// `BiFunction.apply` 传两个装箱 `Integer` 引用。`dispatch_lambda` 现按 impl_desc 形参类型对
/// SAM 实参拆箱(读 `value` 字段 → int)、按 SAM 返回类型对原语返回装箱(int → Integer),
/// 对应 LambdaForm 的 unbox/box 节点。原直传致 `iload` 在 Reference 槽 `get_int` →
/// `Frame(TypeMismatch)`。
#[test]
fn stream_reduce_with_method_ref_end_to_end() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static(&mut vm, "sumReduce", "()I") {
        Ok(Value::Int(v)) => assert_eq!(v, 10, "Stream.of(1,2,3,4).reduce(0, Integer::sum) 须 10"),
        Ok(other) => panic!("期望 Int,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("sumReduce 抛异常:\n{trace}");
        }
        Err(e) => panic!("sumReduce 内部错误:{e:?}"),
    }
}
