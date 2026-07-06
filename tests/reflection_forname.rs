//! 集成闸门(Layer 4.15a):**反射类元数据 native — `Class.forName0`**。
//!
//! `Class.forName(String, boolean, ClassLoader)`(Class.java:557)经 `validateClassNameLength`
//! 调私有 static native `forName0`(Class.java:565)。rustj 移植 jvm.cpp `JVM_FindClassFromCaller`
//! 语义:按名(点形)查注册表 → `init=true` 触发 `ensure_class_initialized` → 返类镜像;
//! 未找到 → `ClassNotFoundException`。本层先做 `forName0`(反射地基);`getDeclared*0`
//! (Field[]/Method[]/Constructor[] 构造)顺延本层后续部分。
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
        "rustj-reflect-{n}-{}-{public_name}",
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
public class Reflect {
    // Class.forName(name, true, null) → forName0 → 按名查注册表返镜像 == Integer.class。
    public static int forNameInteger() {
        try {
            Class<?> c = Class.forName("java.lang.Integer", true, null);
            return c == Integer.class ? 1 : -1;
        } catch (ClassNotFoundException e) {
            return -100;
        }
    }
    // 未知名 → forName0 抛 ClassNotFoundException → catch 返 2。
    public static int forNameUnknownThrows() {
        try {
            Class.forName("no.such.Class", true, null);
            return -1; // 不应到达
        } catch (ClassNotFoundException e) {
            return 2;
        }
    }
    // initialize=false:不触发 <clinit>,仍返同一镜像 == String.class。
    public static int forNameNoInit() {
        try {
            Class<?> c = Class.forName("java.lang.String", false, null);
            return c == String.class ? 3 : -3;
        } catch (ClassNotFoundException e) {
            return -100;
        }
    }
}
"#;

/// **集成闸门**:Layer 4.15a Class.forName0 反射类查找。
#[test]
fn class_for_name0_resolves_by_name() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编 Reflect;载入注册表。
    let dir = compile_dir(SOURCE, "Reflect");
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("Reflect.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 Class(forName 所在)+ Integer/String(测试体)
    //    + ClassNotFoundException(catch 引用)+ ClassLoader(forName 描述符)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/Class").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassNotFoundException").unwrap();

    let mut vm = Vm::new(&registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // 3) forName0 按名查注册表返镜像(== Integer.class,同一 intern)。
    assert_eq!(run_static_int(&mut vm, "Reflect", "forNameInteger"), Ok(1), "forName0 须返 Integer 镜像");
    // 4) 未知名 → ClassNotFoundException(catch 命中)。
    assert_eq!(
        run_static_int(&mut vm, "Reflect", "forNameUnknownThrows"),
        Ok(2),
        "forName0 未知名须抛 ClassNotFoundException"
    );
    // 5) initialize=false 仍返正确镜像(不触发 <clinit>)。
    assert_eq!(run_static_int(&mut vm, "Reflect", "forNameNoInit"), Ok(3), "forName0(init=false) 须返 String 镜像");
}
