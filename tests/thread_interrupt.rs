//! 集成闸门(Phase B.4c):`Thread.interrupt()` / `isInterrupted()` / `interrupted()` / 中断唤醒
//! `Object.wait` 与 `Thread.sleep` → `InterruptedException`,用 `javac` 编译真 Java 程序端到端验证。
//!
//! **B.4c 边界**:`interrupt()`(Thread.java:1618 `interrupted = true; interrupt0();`——字段由字节码置,
//! `interrupt0` 唤醒被阻塞者)、`isInterrupted()`(字节码 `return interrupted;`)、
//! `interrupted()`(`currentThread().getAndClearInterrupt()`——字节码,清标志)。`Object.wait`/`Thread.sleep`
//! 阻塞中被中断 → 清标志 + 抛 `InterruptedException`(JLS §17.2.3 / Thread.sleep 契约)。
//!
//! 验证:
//! (1) `self_interrupt_visible`:自中断后 `isInterrupted()` 真(且不清标志)。
//! (2) `interrupted_clears_flag`:`Thread.interrupted()` 首返 true 并清标志,次返 false。
//! (3) `wait_interrupted_throws`:子线程 `synchronized(lock){lock.wait();}`,主线程 `interrupt()`
//!   → 子线程 `wait0` 被唤醒、检中断、抛 `InterruptedException`(catch 置 result=42)。
//! (4) `sleep_interrupted_throws`:子线程 `Thread.sleep(20s)`,主线程 `interrupt()` → 抛 IEE。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, Vm};

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
    let dir = std::env::temp_dir().join(format!("rustj-b4c-{n}-{}-{public_name}", std::process::id()));
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
    name: &str,
    desc: &str,
) -> &'a rustj::metadata::MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 解释执行一个静态方法(无参),返回值或 `VmError`。
fn run_static(
    registry: &std::sync::Arc<ClassRegistry>,
    vm: &mut Vm,
    class: &str,
    name: &str,
    desc: &str,
) -> Result<Value, rustj::runtime::VmError> {
    let lc = registry.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

/// 装载 Probe + java.base 关键类,返回 (registry, vm)。
fn load() -> (std::sync::Arc<ClassRegistry>, Vm) {
    let dir = compile_dir(SOURCE, "Probe");
    let mut registry = ClassRegistry::new();
    let pcf = parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap();
    registry.load(pcf).unwrap();
    let Some(jmod) = find_javabase_jmod() else {
        panic!("需 java.base.jmod");
    };
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in [
        "java/lang/Thread",
        "java/lang/ThreadGroup",
        "java/lang/Runnable",
        "java/lang/Object",
        "java/lang/String",
        "java/lang/Math",
        "java/util/concurrent/TimeUnit",
    ] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    let registry = std::sync::Arc::new(registry);
    let vm = Vm::new(std::sync::Arc::clone(&registry));
    (registry, vm)
}

const SOURCE: &str = r#"
public class Probe implements Runnable {
    public static int result = 0;
    private final int mode;
    private final Object lock;
    public Probe(int mode, Object lock) { this.mode = mode; this.lock = lock; }
    public void run() {
        try {
            if (mode == 1) {
                synchronized (lock) { lock.wait(); }
            } else {
                Thread.sleep(20_000);
            }
        } catch (InterruptedException e) {
            result = 42;
        }
    }
    public static int selfInterrupt() {
        Thread.currentThread().interrupt();
        return Thread.currentThread().isInterrupted() ? 1 : 0;
    }
    public static int interruptedClears() {
        Thread.currentThread().interrupt();
        boolean a = Thread.interrupted();
        boolean b = Thread.interrupted();
        return (a && !b) ? 1 : 0;
    }
    public static int waitInterrupt() {
        result = 0;
        Object lock = new Object();
        Thread t = new Thread(new Probe(1, lock), "w");
        t.start();
        try { Thread.sleep(50); } catch (InterruptedException e) {}
        t.interrupt();
        try { t.join(); } catch (InterruptedException e) {}
        return result;
    }
    public static int sleepInterrupt() {
        result = 0;
        Thread t = new Thread(new Probe(2, null), "w");
        t.start();
        try { Thread.sleep(50); } catch (InterruptedException e) {}
        t.interrupt();
        try { t.join(); } catch (InterruptedException e) {}
        return result;
    }
}
"#;

fn assert_int(reg: &std::sync::Arc<ClassRegistry>, vm: &mut Vm, name: &str, expected: i32) {
    let v = match run_static(reg, vm, "Probe", name, "()I").expect("{name} 应非抛") {
        Value::Int(v) => v,
        other => panic!("{name} 须返 int,得 {other:?}"),
    };
    assert_eq!(v, expected, "{name} 须返 {expected}");
}

/// **RED→GREEN**(Phase B.4c):自中断后 `isInterrupted()` 真(不清标志)。
#[test]
fn self_interrupt_visible() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    if find_javabase_jmod().is_none() {
        eprintln!("跳过:无 java.base.jmod");
        return;
    }
    let (reg, mut vm) = load();
    assert_int(&reg, &mut vm, "selfInterrupt", 1);
}

/// **RED→GREEN**(Phase B.4c):`Thread.interrupted()` 首返 true 并清标志,次返 false。
#[test]
fn interrupted_clears_flag() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    if find_javabase_jmod().is_none() {
        eprintln!("跳过:无 java.base.jmod");
        return;
    }
    let (reg, mut vm) = load();
    assert_int(&reg, &mut vm, "interruptedClears", 1);
}

/// **RED→GREEN**(Phase B.4c):子线程 `lock.wait()` 被中断 → InterruptedException(catch 置 result=42)。
#[test]
fn wait_interrupted_throws() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    if find_javabase_jmod().is_none() {
        eprintln!("跳过:无 java.base.jmod");
        return;
    }
    let (reg, mut vm) = load();
    assert_int(&reg, &mut vm, "waitInterrupt", 42);
}

/// **RED→GREEN**(Phase B.4c):子线程 `Thread.sleep(20s)` 被中断 → InterruptedException。
#[test]
fn sleep_interrupted_throws() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    if find_javabase_jmod().is_none() {
        eprintln!("跳过:无 java.base.jmod");
        return;
    }
    let (reg, mut vm) = load();
    assert_int(&reg, &mut vm, "sleepInterrupt", 42);
}
