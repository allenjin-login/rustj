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

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread};

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
    let dir = std::env::temp_dir().join(format!("rustj-b4b-{n}-{}-{public_name}", std::process::id()));
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
    vm: &mut VmThread,
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
    let result = match run_static(&registry, &mut vm, "Probe", "runAndJoin", "()I").expect("runAndJoin 应非抛") {
        Value::Int(v) => v,
        other => panic!("runAndJoin 须返 int,得 {other:?}"),
    };
    assert_eq!(result, 42, "start()+join() 后子线程副作用须可见(result==42)");

    // join() 返回意味着 terminate 的 notifyAll 唤醒了 joiner(否则永久 wait 死锁)。
    // counter==40000 证子线程 run() 忙循环跑完(非提前死锁)。
    let counter = match run_static(&registry, &mut vm, "Probe", "getCounter", "()I").expect("getCounter 应非抛") {
        Value::Int(v) => v,
        other => panic!("getCounter 须返 int,得 {other:?}"),
    };
    assert_eq!(counter, 40000, "子线程忙循环须跑完(counter==40000)");
}

/// **RED→GREEN**(Phase B.4b):二次 `start()` 读 `holder.threadStatus!=0` → IMSE。
#[test]
fn double_start_throws_imse() {
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

    let result = match run_static(&registry, &mut vm, "Probe", "doubleStart", "()I").expect("doubleStart 应非抛") {
        Value::Int(v) => v,
        other => panic!("doubleStart 须返 int,得 {other:?}"),
    };
    assert_eq!(result, 1, "二次 start() 须抛 IllegalThreadStateException(返 1)");
}
