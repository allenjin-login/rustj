//! 集成探针(Phase G.0 可行性):**反射 + Class-File API** 两个数据点。
//! 物种生成链 = `getDeclaredConstructor`(deriveSuperClass)→ `ClassFile.of().build`(generateConcrete
//! SpeciesCodeFile)→ `defineClass`。本探针隔离前两个的可行性(独立于 BMH,避免 <clinit> 早崩)。
//!
//! (a) `Probe.class.getDeclaredConstructor(String,Integer)` → parameterTypes 引用同一性 + modifiers。
//! (b) `ClassFile.of().build(ClassDesc.of("X"), b -> b.withFlags(ACC_PUBLIC).withSuperclass(CD_Object))`
//!     → 非空 byte[](验 Class-File API 核心 build 路径 + 其 invokedynamic lambda)。
//!
//! 需 javac + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{
    bootstrap_java_lang_invoke, bootstrap_module_system, initialize_system_class,
};
use rustj::runtime::{Frame, Interpreter, Value, VmError, VmThread};
use rustj::testkit::*;

/// 执行 `Probe.{name}{desc}` 静态法。`classfileBuild` 经 invokedynamic lambda,须方法身份 →
/// `with_identity`(testkit `run_static_in` 无此变体,故保留最小本地 runner,同 D 组 indy_concat)。
fn run_static(vm: &mut VmThread, name: &str, desc: &str) -> Result<Value, VmError> {
    let reg = vm.registry().expect("类注册表缺失");
    let lc = reg.get("Probe").expect("Probe 未加载");
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().expect("应有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
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
