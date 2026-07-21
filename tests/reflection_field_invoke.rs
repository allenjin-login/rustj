//! 集成闸门(Phase B.5.2):**字段 DirectMethodHandle 调用** —— 解释器对
//! `invokevirtual MethodHandle.{invoke,invokeExact}` 的签名多态钩子:receiver 为字段 DMH 时
//! 直读 `member`(MemberName)按 refKind 做 getfield/putfield/getstatic/putstatic(设计 §2
//! shortcut:rustj 不解释 LambdaForm)。
//!
//! 前置:B.5.1(`unreflectField` 返非 null DMH)。本闸门驱动 `mh.invoke/invokeExact(...)` 读出
//! /写入字段真值。RED = `MethodHandle.invoke` 走 ACC_NATIVE 分派 → UnsatisfiedLinkError;
//! GREEN = 字段值正确。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{
    bootstrap_java_lang_invoke, bootstrap_module_system, initialize_system_class,
};
use rustj::runtime::{VmError, VmThread, Value};
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.lang.reflect.Field;
import java.lang.invoke.MethodHandle;
import jdk.internal.access.SharedSecrets;
import jdk.internal.access.JavaLangInvokeAccess;
public class Probe {
    public int x = 7;
    // 非最终静态字段:经 <clinit> putstatic 置值(非 ConstantValue 属性——该属性仅 final 常量字段有,
    // 其处理为独立后续层)。用于验证 getStatic DMH 钩子读静态槽。
    public static int statField = 123;

    // 实例字段 getter via invoke:Integer.value(int)。
    public static int instanceInvoke() throws Throwable {
        Field f = Integer.class.getDeclaredField("value");
        MethodHandle mh = SharedSecrets.getJavaLangInvokeAccess().unreflectField(f, false);
        return (int) mh.invoke(Integer.valueOf(42));
    }

    // 实例字段 getter via invokeExact:Integer.value(int)。
    public static int instanceInvokeExact() throws Throwable {
        Field f = Integer.class.getDeclaredField("value");
        MethodHandle mh = SharedSecrets.getJavaLangInvokeAccess().unreflectField(f, false);
        return (int) mh.invokeExact(Integer.valueOf(42));
    }

    // 静态字段 getter via invoke:Probe.statField(int 静态,非 final)。
    public static int staticInvoke() throws Throwable {
        Field f = Probe.class.getDeclaredField("statField");
        MethodHandle mh = SharedSecrets.getJavaLangInvokeAccess().unreflectField(f, false);
        return (int) mh.invoke();
    }

    // 实例字段 setter via invoke:Probe.x(int,写 99 后读回)。
    public static int instanceSetter() throws Throwable {
        Field f = Probe.class.getDeclaredField("x");
        MethodHandle mh = SharedSecrets.getJavaLangInvokeAccess().unreflectField(f, true);
        Probe p = new Probe();
        mh.invoke(p, 99);
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
        "java/lang/reflect/Field",
        "java/lang/invoke/MethodHandles",
        "java/lang/invoke/MethodHandleImpl",
        "java/lang/invoke/MethodHandle",
        "java/lang/invoke/DirectMethodHandle",
        "java/lang/invoke/MemberName",
        "java/lang/invoke/MethodHandleNatives",
        "jdk/internal/access/SharedSecrets",
        "jdk/internal/misc/VM",
        "java/lang/Object",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }
    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 应成功");
    bootstrap_java_lang_invoke(&mut vm).expect("Phase 3 lite 应成功");
    Some(vm)
}

fn run_case(vm: &mut VmThread, name: &str) -> i32 {
    match run_static_in(vm, "Probe", name, "()I") {
        Ok(Value::Int(v)) => v,
        Ok(other) => panic!("{name} 期望 Int,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("{name} 抛异常:\n{trace}");
        }
        Err(e) => panic!("{name} 内部错误:{e:?}"),
    }
}

/// **RED→GREEN**(Phase B.5.2):字段 DMH 的 invoke/invokeExact 经解释器钩子读出/写入字段真值。
#[test]
fn method_handle_field_invoke_reads_and_writes() {
    let Some(mut vm) = setup_vm() else { return };

    // 实例 getter(invoke / invokeExact):Integer(42).value == 42。
    assert_eq!(run_case(&mut vm, "instanceInvoke"), 42);
    assert_eq!(run_case(&mut vm, "instanceInvokeExact"), 42);
    // 静态 getter:Probe.statField == 123(非 final → <clinit> putstatic 置值;ConstantValue 属性
    // 处理为独立后续层,故暂不用 Integer.MIN_VALUE 这种 static final 常量)。
    assert_eq!(run_case(&mut vm, "staticInvoke"), 123);
    // 实例 setter:Probe.x 经 putField 写 99 后读回。
    assert_eq!(run_case(&mut vm, "instanceSetter"), 99);
}
