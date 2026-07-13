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
        "rustj-mhlf-{n}-{}-{public_name}",
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

/// 经解释器在 `Probe` 上跑静态法 `name()I`,返 owned Value。
fn run_static_int(vm: &mut Vm, name: &str) -> Result<Value, VmError> {
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
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 Probe.{name}()I"));
    let code = method.code.as_ref().expect("应有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

const SOURCE: &str = r#"
import java.lang.invoke.MethodHandle;
import java.lang.invoke.MethodHandles;
public class Probe {
    // identity(int.class) 的 LF 无计算节点(仅 MH param + arg param,result=arg)→ 验证 LF 解释最小骨架。
    public static int identityInvokeExact() throws Throwable {
        MethodHandle mh = MethodHandles.identity(int.class);
        return (int) mh.invokeExact(42);
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
    let mut vm = Vm::new(registry);
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
    match run_static_int(&mut vm, "identityInvokeExact") {
        Ok(Value::Int(v)) => assert_eq!(v, 42, "identity(42) 经 LF 解释须返 42"),
        Ok(other) => panic!("期望 Int,得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("identityInvokeExact 抛异常:\n{trace}");
        }
        Err(e) => panic!("identityInvokeExact 内部错误:{e:?}"),
    }
}
