//! 集成闸门(Layer 4.41 / Phase B.1):用 `javac` 编译调用 `Thread.currentThread().threadId()`
//! 与 `getName()` 的最小真 Java 程序,从真实 `java.base.jmod` 加载 `Thread`(连同传递依赖),
//! 由 rustj 解释器执行——端到端验证 main 线程镜像 `name="main"`、`tid=1`(`alloc_main_thread`
//! 置字段 → 真字节码 `threadId()`/`getName()` 读字段返正确值)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 找本机首个 `java.base.jmod`;无则 `None`。
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

/// javac 编译单个 public 类到临时目录,返回该目录。
fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-thread-{n}-{}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac").arg("-d").arg(&dir).arg(&src).output().expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// 按名+描述符在类中找方法。
fn find_method<'a>(
    cf: &'a rustj::metadata::ClassFile,
    cp: &rustj::constant_pool::ConstantPool,
    name: &str,
    desc: &str,
) -> &'a rustj::metadata::MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(cp.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(cp.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 解释执行一个静态方法(无参)。
fn run_static(registry: &std::sync::Arc<ClassRegistry>, class: &str, name: &str, desc: &str) -> Result<Value, VmError> {
    let lc = registry.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, &lc.cf.constant_pool, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = VmThread::new(std::sync::Arc::clone(registry));
    interp.interpret_with(&mut frame, &mut vm)
}

const SOURCE: &str = r#"
public class ThreadGate {
    // main 线程镜像:threadId() 读 tid 字段(须=1),getName() 读 name 字段(须="main")。
    public static long tid() {
        return Thread.currentThread().threadId();
    }
    public static boolean isMain() {
        return "main".equals(Thread.currentThread().getName());
    }
}
"#;

/// **RED→GREEN**(S4):main 线程镜像 `tid=1`、`name="main"`(真字节码 `threadId()`/`getName()`)。
#[test]
fn main_thread_mirror_has_name_main_and_tid_one() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 ThreadGate;载入注册表。
    let dir = compile_dir(SOURCE, "ThreadGate");
    let mut registry = ClassRegistry::new();
    let tg = parse(&std::fs::read(dir.join("ThreadGate.class")).unwrap()).unwrap();
    registry.load(tg).unwrap();

    // 2) 真 Thread 从 jmod 载入(连同传递依赖:String 等)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/Thread").unwrap();
    let registry = std::sync::Arc::new(registry);

    // 3) threadId() → 1L(alloc_main_thread 置 tid 字段)。
    assert_eq!(
        run_static(&registry, "ThreadGate", "tid", "()J").unwrap(),
        Value::Long(1),
        "main 线程 tid 须为 1"
    );

    // 4) isMain() → true("main".equals(getName()))。
    assert_eq!(
        run_static(&registry, "ThreadGate", "isMain", "()Z").unwrap(),
        Value::Int(1),
        "main 线程 name 须为 \"main\""
    );
}
