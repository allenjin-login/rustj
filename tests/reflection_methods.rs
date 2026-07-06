//! 集成闸门(Layer 4.15a-3):**反射可执行成员 native — `getDeclaredMethods0`/`getDeclaredConstructors0`**。
//!
//! `Class.getDeclaredMethod(name, params...)`/`getDeclaredConstructor(params...)` 经
//! `privateGetDeclaredMethods`/`privateGetDeclaredConstructors`(同 fields 的 reflectionData CAS
//! 缓存 + Reflection.filterMethods/Constructors)调私有 native
//! `getDeclaredMethods0(Z)[Ljava/lang/reflect/Method;` / `getDeclaredConstructors0(Z)[...]Constructor;`,
//! 构造真 Method[]/Constructor[](`slot`=本类方法序,`parameterTypes`/`exceptionTypes` 为 Class[]
//! 经方法描述符解析)。searchMethods/searchConstructors 按 name+parameterTypes 匹配,故参数类型数组
//! 须精确。复用 part2 的 AccessibleObject clinit 引导 + ReflectionFactory.copyMethod/copyConstructor。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
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
        "rustj-reflectmc-{n}-{}-{public_name}",
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

fn run_static_int(vm: &mut Vm<'_>, class: &str, name: &str) -> Result<i32, String> {
    let lc = vm
        .registry()
        .and_then(|r| r.get(class))
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
import java.lang.reflect.Constructor;
public class ReflectMc {
    // parseInt(String) public static → PUBLIC|STATIC == 9。searchMethods 按 name+parameterTypes 匹配。
    public static int parseIntMethodModifiers() {
        try {
            Method m = Integer.class.getDeclaredMethod("parseInt", String.class);
            return m.getModifiers();
        } catch (NoSuchMethodException e) {
            return -100;
        }
    }
    // getParameterCount() 真字节码读 parameterTypes.length → 1。
    public static int parseIntParamCount() {
        try {
            Method m = Integer.class.getDeclaredMethod("parseInt", String.class);
            return m.getParameterCount();
        } catch (NoSuchMethodException e) {
            return -100;
        }
    }
    // Integer(int) 构造器 → getParameterCount() == 1。
    public static int intValueCtorParamCount() {
        try {
            Constructor<?> c = Integer.class.getDeclaredConstructor(int.class);
            return c.getParameterCount();
        } catch (NoSuchMethodException e) {
            return -100;
        }
    }
}
"#;

/// **集成闸门**:Layer 4.15a-3 getDeclaredMethods0/Constructors0。
#[test]
fn class_declared_methods0_constructs_method_array() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_dir(SOURCE, "ReflectMc");
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("ReflectMc.class")).unwrap()).unwrap())
        .unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for c in [
        "java/lang/Class",
        "java/lang/Integer",
        "java/lang/String",
        "java/lang/reflect/Method",
        "java/lang/reflect/Constructor",
        "java/lang/reflect/AccessibleObject",
        "java/lang/NoSuchMethodException",
        "jdk/internal/reflect/Reflection",
        "jdk/internal/reflect/ReflectionFactory",
        "jdk/internal/access/SharedSecrets",
        "jdk/internal/misc/Unsafe",
        "java/util/Map",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }

    let mut vm = Vm::new(&registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // getDeclaredMethod("parseInt", String.class) 命中,modifiers 9 = PUBLIC|STATIC。
    assert_eq!(
        run_static_int(&mut vm, "ReflectMc", "parseIntMethodModifiers"),
        Ok(9),
        "parseInt(String) modifiers 须为 PUBLIC|STATIC == 9"
    );
    // getParameterCount() == 1(参数类型数组长度)。
    assert_eq!(
        run_static_int(&mut vm, "ReflectMc", "parseIntParamCount"),
        Ok(1),
        "parseInt(String) 参数个数须为 1"
    );
    // getDeclaredConstructor(int.class).getParameterCount() == 1。
    assert_eq!(
        run_static_int(&mut vm, "ReflectMc", "intValueCtorParamCount"),
        Ok(1),
        "Integer(int) 构造器参数个数须为 1"
    );
}
