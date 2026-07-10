//! 集成闸门(Layer 4.31):**`ClassLoader.findLoadedClass0(String)Class` native**。
//!
//! 4.30 安装 `SharedSecrets.javaLangAccess` 后 `getSystemClassLoader()` 返非 null,但
//! `cl.loadClass(name)` 链阻塞于:`ClassLoader.loadClass:490` → `BuiltinClassLoader.loadClass:578`
//! → `loadClassOrNull:592` → `findLoadedClass:1264` → `findLoadedClass0`(native,ULE)。
//! `findLoadedClass0`(`ClassLoader.java:1270 private final native`)移植 `JVM_FindLoadedClass`:
//! 按 binary name 查"本 loader 已加载"集 → 返 Class 镜像或 null。rustj 单注册表模型:`name →
//! registry.get(intern) → intern_class_mirror 或 null`(忽略 per-loader 隔离;已加载类即返)。
//! 解锁 `loadClass` 的**已加载类快速路径**(`findLoadedClass` 命中即返,不进 module/parent 委派)。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
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
    None
}

// cl.loadClass("java.lang.String"):触发 loadClass→findLoadedClass→findLoadedClass0。
// String 已在注册表(闭包预载)→ findLoadedClass0 命中返 String Class → 与 String.class 同一。
const LOAD_PROBE_SOURCE: &str = r#"
public class LoadProbe {
    public static int loadStringIdentity() throws Exception {
        Class<?> c = ClassLoader.getSystemClassLoader().loadClass("java.lang.String");
        return (c == String.class) ? 1 : 0;
    }
}
"#;

fn run_static_int(vm: &mut Vm, class: &str, name: &str) -> Result<i32, String> {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表"));
    let lc = reg
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {class}.{name}()I"));
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("{o:?}"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

/// **集成闸门**(Layer 4.31):`findLoadedClass0` 命中注册表 → `loadClass("java.lang.String")`
/// 返 String Class(与 `String.class` 同一)。修前抛 UnsatisfiedLinkError(findLoadedClass0 未登记)。
#[test]
fn find_loaded_class0_supports_loadclass_already_loaded() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "rustj-load-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::new(0)
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("LoadProbe.java"), LOAD_PROBE_SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(dir.join("LoadProbe.java"))
        .output()
        .expect("javac 失败");
    assert!(
        out.status.success(),
        "javac 失败:{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut registry = ClassRegistry::new();
    registry
        .load(
            rustj::classfile::parse(&std::fs::read(dir.join("LoadProbe.class")).unwrap()).unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassLoader").unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = Vm::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    assert_eq!(
        run_static_int(&mut vm, "LoadProbe", "loadStringIdentity"),
        Ok(1),
        "loadClass(\"java.lang.String\") 须返与 String.class 同一的 Class(findLoadedClass0 命中注册表)"
    );
}
