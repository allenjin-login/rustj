//! 集成闸门(Layer 4.15a-2):**反射成员元数据 native — `getDeclaredFields0`**。
//!
//! `Class.getDeclaredFields()`(Class.java:2249)= `copyFields(privateGetDeclaredFields(false))`。
//! `privateGetDeclaredFields`(Class.java:2914)经 `reflectionData()` 缓存(首次 `SoftReference`→
//! null → `newReflectionData` 的 `Atomic.casReflectionData`/Unsafe CAS)后调私有 native
//! `getDeclaredFields0(Z)[Ljava/lang/reflect/Field;`(jmod 实测描述符),再 `Reflection.filterFields`
//! (java.base 类不在 fieldFilterMap → 透传),最后 `copyFields`/`ReflectionFactory.copyField`。
//!
//! 本闸门驱动整条 `java.lang.reflect` 引导链:AccessibleObject `<clinit>`(置 SharedSecrets)→
//! ReflectionFactory 单例 → ReflectAccess.copyField → Field.copy + Unsafe 引用 CAS。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

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
        "rustj-reflectdecl-{n}-{}-{public_name}",
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

/// 解释执行一个无参静态 int 方法(共用传入 Vm)。抛 Java 异常时把类名带出。
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
import java.lang.reflect.Field;
public class ReflectDecl {
    // getDeclaredFields() → 整条 copyFields/privateGetDeclaredFields/getDeclaredFields0 链。
    public static int declaredFieldsNonEmpty() {
        Field[] fs = Integer.class.getDeclaredFields();
        return fs.length > 0 ? 1 : -1;
    }
    // getDeclaredField("value") → searchFields + 同链;Integer.value = private final int
    // (ACC_PRIVATE|ACC_FINAL == 18);Field.getName() 真字节码读 name 字段。
    public static int valueFieldModifiers() {
        try {
            Field f = Integer.class.getDeclaredField("value");
            return f.getName().equals("value") ? f.getModifiers() : -2;
        } catch (NoSuchFieldException e) {
            return -100;
        }
    }
}
"#;

/// **集成闸门**:Layer 4.15a-2 getDeclaredFields0 反射成员元数据。
#[test]
fn class_declared_fields0_constructs_field_array() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编 ReflectDecl;载入注册表。
    let dir = compile_dir(SOURCE, "ReflectDecl");
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("ReflectDecl.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载反射链所需的真 java.base 类。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for c in [
        "java/lang/Class",
        "java/lang/Integer",
        "java/lang/String",
        "java/lang/reflect/Field",
        "java/lang/reflect/AccessibleObject",
        "java/lang/NoSuchFieldException",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/access/SharedSecrets",
        "java/lang/ref/SoftReference",
        "jdk/internal/misc/Unsafe",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // 3) getDeclaredFields() 经整条链返非空 Field[]。
    assert_eq!(
        run_static_int(&mut vm, "ReflectDecl", "declaredFieldsNonEmpty"),
        Ok(1),
        "getDeclaredFields0 须返非空 Field[]"
    );
    // 4) getDeclaredField("value") 命中并读 modifiers(18 = PRIVATE|FINAL)。
    assert_eq!(
        run_static_int(&mut vm, "ReflectDecl", "valueFieldModifiers"),
        Ok(18),
        "Integer.value modifiers 须为 PRIVATE|FINAL == 18"
    );
}
