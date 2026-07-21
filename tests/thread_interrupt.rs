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

use rustj::classfile::parse;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::VmThread;
use rustj::testkit::*;

/// 装载 Probe + java.base 关键类,返回 (registry, vm)。
fn load() -> (std::sync::Arc<ClassRegistry>, VmThread) {
    let dir = compile_dir(SOURCE, "Probe", &[]);
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
    let vm = VmThread::new(std::sync::Arc::clone(&registry));
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

fn assert_int(vm: &mut VmThread, name: &str, expected: i32) {
    assert_eq!(run_static_int(vm, "Probe", name).unwrap(), expected, "{name} 须返 {expected}");
}

/// **RED→GREEN**(Phase B.4c):自中断后 `isInterrupted()` 真(不清标志)。
#[test]
fn self_interrupt_visible() {
    require_javac!();
    require_javabase!(jmod);
    let (_, mut vm) = load();
    assert_int(&mut vm, "selfInterrupt", 1);
}

/// **RED→GREEN**(Phase B.4c):`Thread.interrupted()` 首返 true 并清标志,次返 false。
#[test]
fn interrupted_clears_flag() {
    require_javac!();
    require_javabase!(jmod);
    let (_, mut vm) = load();
    assert_int(&mut vm, "interruptedClears", 1);
}

/// **RED→GREEN**(Phase B.4c):子线程 `lock.wait()` 被中断 → InterruptedException(catch 置 result=42)。
#[test]
fn wait_interrupted_throws() {
    require_javac!();
    require_javabase!(jmod);
    let (_, mut vm) = load();
    assert_int(&mut vm, "waitInterrupt", 42);
}

/// **RED→GREEN**(Phase B.4c):子线程 `Thread.sleep(20s)` 被中断 → InterruptedException。
#[test]
fn sleep_interrupted_throws() {
    require_javac!();
    require_javabase!(jmod);
    let (_, mut vm) = load();
    assert_int(&mut vm, "sleepInterrupt", 42);
}
