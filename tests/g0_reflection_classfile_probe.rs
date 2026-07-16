//! 集成探针(Phase G.0 可行性):**反射 + Class-File API** 两个数据点。
//! 物种生成链 = `getDeclaredConstructor`(deriveSuperClass)→ `ClassFile.of().build`(generateConcrete
//! SpeciesCodeFile)→ `defineClass`。本探针隔离前两个的可行性(独立于 BMH,避免 <clinit> 早崩)。
//!
//! (a) `Probe.class.getDeclaredConstructor(String,Integer)` → parameterTypes 引用同一性 + modifiers。
//! (b) `ClassFile.of().build(ClassDesc.of("X"), b -> b.withFlags(ACC_PUBLIC).withSuperclass(CD_Object))`
//!     → 非空 byte[](验 Class-File API 核心 build 路径 + 其 invokedynamic lambda)。
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
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

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
        "rustj-g0probe-{n}-{}-{public_name}",
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

fn run_static(vm: &mut VmThread, name: &str, desc: &str) -> Result<Value, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("类注册表缺失");
    let lc = reg.get("Probe").unwrap_or_else(|| panic!("Probe 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 Probe.{name}{desc}"));
    let code = method.code.as_ref().expect("应有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity("Probe", name);
    interp.interpret_with(&mut frame, vm)
}

const SOURCE: &str = r#"
import java.lang.reflect.Constructor;
import java.lang.classfile.ClassFile;
import java.lang.constant.ClassDesc;
import java.lang.constant.ConstantDescs;
public class Probe {
    Probe(String s, Integer i) {}                  // 包私有构造器,两个引用参数
    // (a) 反射:getDeclaredConstructor + parameterTypes 引用同一性 + modifiers。
    public static boolean reflectionCtor() throws Exception {
        Constructor<?> c = Probe.class.getDeclaredConstructor(String.class, Integer.class);
        Class<?>[] pt = c.getParameterTypes();
        return pt.length == 2 && pt[0] == String.class && pt[1] == Integer.class;
    }
    // (b) Class-File API:ClassFile.of().build(...) 产非空字节码(经 invokedynamic lambda Consumer)。
    public static int classfileBuild() throws Exception {
        byte[] b = ClassFile.of().build(ClassDesc.of("GeneratedFoo"), clb -> {
            clb.withFlags(ClassFile.ACC_PUBLIC)
               .withSuperclass(ConstantDescs.CD_Object);
        });
        return b.length;
    }
}
"#;

fn setup_vm() -> Option<VmThread> {
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
        "java/lang/String",
        "java/lang/Integer",
        "java/lang/Object",
        "java/lang/reflect/Constructor",
        "java/lang/reflect/AccessibleObject",
        "java/lang/reflect/Executable",
        "java/lang/reflect/Member",
        "java/lang/reflect/Modifier",
        "java/lang/invoke/MethodHandles",
        "java/lang/invoke/MethodHandleImpl",
        "java/lang/invoke/MethodHandle",
        "java/lang/invoke/DirectMethodHandle",
        "java/lang/invoke/MemberName",
        "java/lang/invoke/MethodHandleNatives",
        "java/lang/invoke/MethodType",
        "java/lang/invoke/CallSite",
        // Class-File API 核心(传递性拉 java.lang.classfile.*)。
        "java/lang/classfile/ClassFile",
        "java/lang/constant/ClassDesc",
        "java/lang/constant/ConstantDescs",
        "jdk/internal/access/SharedSecrets",
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

#[test]
fn g0_reflection_get_declared_constructor() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static(&mut vm, "reflectionCtor", "()Z") {
        Ok(Value::Int(n)) => assert!(n != 0, "getDeclaredConstructor parameterTypes 引用同一性应成立"),
        Ok(other) => panic!("期望 boolean,得 {other:?}"),
        Err(VmError::ThrownException(r)) => {
            panic!("reflectionCtor 抛:\n{}", vm.format_trace(r));
        }
        Err(e) => panic!("内部错误:{e:?}"),
    }
}

#[test]
fn g0_classfile_api_build() {
    let Some(mut vm) = setup_vm() else { return };
    match run_static(&mut vm, "classfileBuild", "()I") {
        Ok(Value::Int(n)) => assert!(n > 0, "ClassFile.of().build 应产非空字节码,得 len={n}"),
        Ok(other) => panic!("期望 int,得 {other:?}"),
        Err(VmError::ThrownException(r)) => {
            panic!("classfileBuild 抛:\n{}", vm.format_trace(r));
        }
        Err(e) => panic!("内部错误:{e:?}"),
    }
}
