//! 集成闸门(Phase B.4b):用 `javac` 编译一个真 Java 多线程程序——`new Thread(r,"w").start(); t.join()`
//! 端到端——从 `java.base.jmod` 加载 `Thread`/`ThreadGroup`/`Runnable`/`Object`,由 rustj 解释器执行。
//!
//! **B.4b 边界**:`Thread.start()`(`synchronized(this){ if(holder.threadStatus!=0) throw IMSE; start0(); }`,
//! Thread.java:1465)走真字节码;`Thread.join()`(Thread.java:1901 `synchronized(this){ while(isAlive()) wait(0); }`,
//! `isAlive()`=`eetop!=0`)走真字节码 + B.3c Object.wait/notify。验证:
//! (1) `start_join_end_to_end`:子线程副作用(putstatic 写静态字段)经 static_storage Mutex 跨线程可见;
//!   join() 阻塞-唤醒往返正确(子线程 terminate 时 `ensure_join` 调 notifyAll 唤醒 joiner——否则永久 wait)。
//! (2) `double_start_throws_imse`:二次 `start()` 读 `holder.threadStatus!=0`(start0 置 RUNNABLE)→ IMSE。
//!
//! 子线程 `run()` 含忙循环,确保 joiner 进入 wait() 时子线程仍 alive——使 notifyAll 路径被真正行使。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::classfile::parse;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
public class Probe implements Runnable {
    public static int result = 0;
    public static int counter = 0;
    // 忙循环:确保 joiner 进入 wait() 时子线程仍 alive——使 terminate 的 notifyAll 路径被真正行使。
    public void run() {
        result = 42;
        for (int i = 0; i < 40000; i++) counter++;
    }
    public static int runAndJoin() throws InterruptedException {
        result = 0;
        Probe p = new Probe();
        Thread t = new Thread(p, "w");
        t.start();
        t.join();
        return result;
    }
    public static int doubleStart() {
        Probe p = new Probe();
        Thread t = new Thread(p, "w");
        t.start();
        try {
            t.start();
        } catch (IllegalThreadStateException e) {
            return 1;
        }
        return 0;
    }
    public static int getCounter() { return counter; }
}
"#;

/// **RED→GREEN**(Phase B.4b):`new Thread(r,"w").start()` + `join()` 端到端 + 二次 start IMSE。
#[test]
fn start_join_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "Probe", &[]);
    let mut registry = ClassRegistry::new();
    let pcf = parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap();
    registry.load(pcf).unwrap();

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
    ] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    let registry = std::sync::Arc::new(registry);
    let mut vm = VmThread::new(std::sync::Arc::clone(&registry));

    // t.start() → 子线程跑 run() 置 result=42;t.join() 阻塞-唤醒;返 result。
    let result = match run_static_in(&mut vm, "Probe", "runAndJoin", "()I").expect("runAndJoin 应非抛") {
        Value::Int(v) => v,
        other => panic!("runAndJoin 须返 int,得 {other:?}"),
    };
    assert_eq!(result, 42, "start()+join() 后子线程副作用须可见(result==42)");

    // join() 返回意味着 terminate 的 notifyAll 唤醒了 joiner(否则永久 wait 死锁)。
    // counter==40000 证子线程 run() 忙循环跑完(非提前死锁)。
    let counter = match run_static_in(&mut vm, "Probe", "getCounter", "()I").expect("getCounter 应非抛") {
        Value::Int(v) => v,
        other => panic!("getCounter 须返 int,得 {other:?}"),
    };
    assert_eq!(counter, 40000, "子线程忙循环须跑完(counter==40000)");
}

/// **RED→GREEN**(Phase B.4b):二次 `start()` 读 `holder.threadStatus!=0` → IMSE。
#[test]
fn double_start_throws_imse() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "Probe", &[]);
    let mut registry = ClassRegistry::new();
    let pcf = parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap();
    registry.load(pcf).unwrap();

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
    ] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    let registry = std::sync::Arc::new(registry);
    let mut vm = VmThread::new(std::sync::Arc::clone(&registry));

    let result = match run_static_in(&mut vm, "Probe", "doubleStart", "()I").expect("doubleStart 应非抛") {
        Value::Int(v) => v,
        other => panic!("doubleStart 须返 int,得 {other:?}"),
    };
    assert_eq!(result, 1, "二次 start() 须抛 IllegalThreadStateException(返 1)");
}
