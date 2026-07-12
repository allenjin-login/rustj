//! 集成闸门(Phase B.5.1):**字段 DirectMethodHandle 创建** —— `MethodHandles.lookup().unreflectGetter(field)`
//! 返非 null 且 member 已解析。
//!
//! `Field.get` 的 DMH 链:`JLIA.unreflectField`(MethodHandleImpl.java:1610)→
//! `Lookup.unreflectField`(MethodHandles.java:3438)→ `getDirectFieldCommon`(:3921)→
//! `DirectMethodHandle.make`(:3923;:113-124 字段分支)调 native
//! `MethodHandleNatives.objectFieldOffset`/`staticFieldOffset`/`staticFieldBase`(MethodHandleNatives.java:57-59)
//! + `member` 须先 `resolve`(:53)。本闸门驱动该链,钉成员解析 + DMH 非空。
//!
//! `unreflectGetter` 前置:Field 经 `Class.getDeclaredField`(getDeclaredField0 native,4.15a);
//! MethodHandles.lookup() 经 MethodHandles.<clinit>。需 `javac` + 本机 jmod;缺一跳过。

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
        "rustj-fieldunreflect-{n}-{}-{public_name}",
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

/// 经解释器在 `Probe` 上跑静态法 `name()Ljava/lang/Object;`,返 owned Value。
fn run_static_object(vm: &mut Vm, name: &str) -> Result<Value, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("类注册表缺失");
    let lc = reg
        .get("Probe")
        .unwrap_or_else(|| panic!("Probe 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()Ljava/lang/Object;");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 Probe.{name}()Ljava/lang/Object;"));
    let code = method.code.as_ref().expect("应有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

const SOURCE: &str = r#"
import java.lang.reflect.Field;
import jdk.internal.access.SharedSecrets;
import jdk.internal.access.JavaLangInvokeAccess;
public class Probe {
    // 实例字段 getter:Integer.value(int 实例字段)。经 JLIA.unreflectField(IMPL_LOOKUP 路径,
    // 同 MethodHandleAccessorFactory.newFieldAccessor:185)—— 绕开 MethodHandles.lookup() 的
    // @CallerSensitive getCallerClass 门。
    public static Object instanceGetter() throws Exception {
        Field f = Integer.class.getDeclaredField("value");
        JavaLangInvokeAccess jlia = SharedSecrets.getJavaLangInvokeAccess();
        return jlia.unreflectField(f, false);
    }
    // 静态字段 getter:Integer.MIN_VALUE(int 静态字段)。
    public static Object staticGetter() throws Exception {
        Field f = Integer.class.getDeclaredField("MIN_VALUE");
        JavaLangInvokeAccess jlia = SharedSecrets.getJavaLangInvokeAccess();
        return jlia.unreflectField(f, false);
    }
}
"#;

/// **RED→GREEN**(Phase B.5.1):`unreflectGetter` 经 resolve + objectFieldOffset/staticFieldOffset
/// native 返非 null DirectMethodHandle(member 已解析)。
///
/// 修前:`DirectMethodHandle.make:61` 断 `member.isResolved()` 抛 InternalError,或
/// `MethodHandleNatives.resolve/objectFieldOffset` 未绑抛 ULE。修后:返非 null DMH。
#[test]
fn unreflect_getter_returns_resolved_dmh() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

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

    let mut vm = Vm::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 应成功");
    bootstrap_java_lang_invoke(&mut vm).expect("Phase 3 lite 应成功");

    // 实例字段 getter(Integer.value)。
    let dmh = match run_static_object(&mut vm, "instanceGetter") {
        Ok(Value::Reference(r)) => r,
        Ok(other) => panic!("instanceGetter 期望 Reference(DMH),得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("instanceGetter 抛异常:\n{trace}");
        }
        Err(e) => panic!("instanceGetter 内部错误:{e:?}"),
    };
    assert!(!dmh.is_null(), "实例字段 getter DMH 须非 null");

    // 静态字段 getter(Integer.MIN_VALUE)。
    let sdmh = match run_static_object(&mut vm, "staticGetter") {
        Ok(Value::Reference(r)) => r,
        Ok(other) => panic!("staticGetter 期望 Reference(DMH),得 {other:?}"),
        Err(VmError::ThrownException(exc)) => {
            let trace = vm.format_trace(exc);
            panic!("staticGetter 抛异常:\n{trace}");
        }
        Err(e) => panic!("staticGetter 内部错误:{e:?}"),
    };
    assert!(!sdmh.is_null(), "静态字段 getter DMH 须非 null");
}
