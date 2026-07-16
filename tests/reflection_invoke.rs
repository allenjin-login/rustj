//! 集成闸门(Layer 4.15b):**反射调用 — `Method.invoke` 端到端**。
//!
//! `Method.invoke(obj, args)` 经真 java.base 字节码路径 → `MethodAccessor`(惰性)→
//! `ReflectionFactory.newMethodAccessor` → `MethodHandleAccessorFactory.newMethodAccessor` →
//! `useNativeAccessor`(`!VM.isJavaLangInvokeInited()` → true,rustj 不跑 initPhase3)→
//! `DirectMethodHandleAccessor$NativeAccessor` → **native `invoke0`**(= HotSpot `JVM_InvokeMethod`)。
//! 绕过「MethodHandle 直接调用」墙。详见 spec `2026-07-11-layer-4.15b-reflection-invocation-design.md`。
//!
//! `Method.invoke` 的 `checkAccess` → `Reflection.verifyPublicMemberAccess` → `Module.isExported`
//! → `implIsExportedOrOpen` 读 java.base `Module.exportedPackages` 实例 Map——由 Layer 4.14c
//! `populate_module_exports`(bootstrap_module_system 末尾)据 `module-info` 的非限定 exports 填充。
//!
//! 覆盖:静态法(parseInt(String)→42)、实例法(String.length()→5)、重载+拆箱
//! (parseInt(String,int)→255,Integer 参拆箱)。需 `javac` + 本机 jmod;缺一跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
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
        "rustj-reflectinvoke-{n}-{}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
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

fn run_static_int(vm: &mut VmThread, class: &str, name: &str) -> Result<i32, String> {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表缺失"));
    let lc = reg
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {class}.{name}()I"));
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance Oop:{o:?})"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

const SOURCE: &str = r#"
import java.lang.reflect.Method;
public class Probe {
    // 静态法:Integer.parseInt(String) → 42(public static;不改 accessibility)。
    public static int invokeStatic() throws Exception {
        Method m = Integer.class.getDeclaredMethod("parseInt", String.class);
        return (int) m.invoke(null, "42");
    }
    // 实例法:String.length() → 5(虚分派 + 非空 receiver)。
    public static int invokeInstance() throws Exception {
        Method m = String.class.getDeclaredMethod("length");
        return (int) m.invoke("hello", new Object[0]);
    }
    // 重载 + 拆箱:parseInt(String, int) → parseInt("ff", 16) == 255(Integer 参拆箱)。
    public static int invokeOverload() throws Exception {
        Method m = Integer.class.getDeclaredMethod("parseInt", String.class, int.class);
        return (int) m.invoke(null, "ff", 16);
    }
}
"#;

/// **RED→GREEN**(Layer 4.15b):`Method.invoke` 经真字节码路径 + native `invoke0`(JVM_InvokeMethod)。
///
/// Layer 4.14c(`populate_module_exports` 填 java.base `Module.exportedPackages`)解锁了
/// `Method.invoke` 的 `checkAccess` → `Module.isExported` 访问检查,使本端到端闸门转绿。
#[test]
fn method_invoke_end_to_end() {
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
        "java/lang/String",
        "java/lang/reflect/Method",
        "java/lang/reflect/AccessibleObject",
        "java/lang/reflect/InvocationTargetException",
        "java/lang/NoSuchMethodException",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/reflect/MethodHandleAccessorFactory",
        "jdk/internal/reflect/DirectMethodHandleAccessor",
        "jdk/internal/access/SharedSecrets",
        "jdk/internal/misc/Unsafe",
        "jdk/internal/misc/VM",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    assert_eq!(
        run_static_int(&mut vm, "Probe", "invokeStatic"),
        Ok(42),
        "Method.invoke 静态法:Integer.parseInt(\"42\") 须返 42"
    );
    assert_eq!(
        run_static_int(&mut vm, "Probe", "invokeInstance"),
        Ok(5),
        "Method.invoke 实例法:\"hello\".length() 须返 5"
    );
    assert_eq!(
        run_static_int(&mut vm, "Probe", "invokeOverload"),
        Ok(255),
        "Method.invoke 重载+拆箱:Integer.parseInt(\"ff\", 16) 须返 255"
    );
}
