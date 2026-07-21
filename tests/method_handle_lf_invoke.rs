//! 集成闸门(Phase G.2):**LambdaForm 解释器** —— `invokevirtual MethodHandle.invokeExact`
//! 在 receiver 非「字段 DirectMethodHandle」时,读 `mh.form`(LambdaForm)遍历 `Name[]` 拓扑序执行
//! (设计 §3)。B.5.2 字段 DMH 短路在前;本法处理 BMH/方法 DMH/转换 adapter 等任意非字段 MH。
//!
//! **G.2.1**(本闸门首例):`MethodHandles.identity(int.class)` 的 LF = `[MH param, arg param]`,
//! arity=2、result=1、**无计算节点**(LambdaForm.java:1683 `Name[]{argument(0,L),argument(1,type)}`)→
//! 解释 = 绑定入口参数 + 返 `names[result]`(=arg)。验证 LF 读取 + 参数绑定 + 结果返回的最小骨架。
//!
//! RED:identity MH 非 DMH(或非字段 DMH)→ B.5.2 钩子抛「仅支持字段 refKind(1-4)」或正常虚分派 ULE。
//! GREEN:`(int) identity(int.class).invokeExact(42)` == 42(经 LF 解释 names[result] 返 arg)。
//!
//! 需 javac + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{
    bootstrap_java_lang_invoke, bootstrap_module_system, initialize_system_class,
};
use rustj::runtime::{Value, VmError, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
public class Probe {
    // identity(int.class) 的 LF 无计算节点(仅 MH param + arg param,result=arg)→ 验证 LF 解释最小骨架。
    public static int identityInvokeExact() throws Throwable {
        MethodHandle mh = MethodHandles.identity(int.class);
        return (int) mh.invokeExact(42);
    }
    // constant(int.class, 42) 的 LF = [carrier(BMH), Name(species.getterFunction(0), carrier)](读 BMH 绑定的
    // 物种字段);且构造本身经 factory().invokeBasic → 物种工厂 LF(含 newInvokeSpecial 构造节点)。
    // 验证 NamedFunction 计算节点分派(字段读 + 构造器调用)。
    public static int constantInvokeExact() throws Throwable {
        MethodHandle mh = MethodHandles.constant(int.class, 42);
        return (int) mh.invokeExact();
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
        .load(
            rustj::classfile::parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for c in [
        "java/lang/Class",
        "java/lang/Integer",
        "java/lang/invoke/MethodHandles",
        "java/lang/invoke/MethodHandleImpl",
        "java/lang/invoke/MethodHandle",
        "java/lang/invoke/BoundMethodHandle",
        "java/lang/invoke/ClassSpecializer",
        "java/lang/invoke/DirectMethodHandle",
        "java/lang/invoke/MemberName",
        "java/lang/invoke/MethodHandleNatives",
        "java/lang/invoke/LambdaForm",
        "java/lang/invoke/MethodType",
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

/// **RED→GREEN**(Phase G.2.1):`MethodHandles.identity(int.class).invokeExact(42)` 经 LF 解释返 42。
/// identity 的 LF = `[MH, arg]`、arity=2、result=1、无计算节点 → 绑参数 + 返 names[1]。
#[test]
fn identity_invoke_exact_via_lambda_form() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static_in(&mut vm, "Probe", "identityInvokeExact", "()I") {
        Ok(Value::Int(v)) => assert_eq!(v, 42, "identity(42) 经 LF 解释须返 42"),
        Ok(other) => panic!("期望 Int,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("identityInvokeExact 抛异常:\n{trace}");
        }
        Err(e) => panic!("identityInvokeExact 内部错误:{e:?}"),
    }
}

/// **RED→GREEN**(Phase G.2.2):`MethodHandles.constant(int.class, 42).invokeExact()` == 42。
/// constant 的构造经物种工厂 invokeBasic(构造器 NamedFunction 节点),LF = [carrier, 字段读] →
/// 验证 NamedFunction 计算节点分派(字段读 + 构造器调用)。
#[test]
fn constant_invoke_exact_via_lambda_form() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static_in(&mut vm, "Probe", "constantInvokeExact", "()I") {
        Ok(Value::Int(v)) => assert_eq!(v, 42, "constant(42) 经 LF 解释须返 42"),
        Ok(other) => panic!("期望 Int,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("constantInvokeExact 抛异常:\n{trace}");
        }
        Err(e) => panic!("constantInvokeExact 内部错误:{e:?}"),
    }
}
