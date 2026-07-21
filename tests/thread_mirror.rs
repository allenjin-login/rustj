//! 集成闸门(Layer 4.41 / Phase B.1):用 `javac` 编译调用 `Thread.currentThread().threadId()`
//! 与 `getName()` 的最小真 Java 程序,从真实 `java.base.jmod` 加载 `Thread`(连同传递依赖),
//! 由 rustj 解释器执行——端到端验证 main 线程镜像 `name="main"`、`tid=1`(`alloc_main_thread`
//! 置字段 → 真字节码 `threadId()`/`getName()` 读字段返正确值)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::classfile::parse;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::Value;
use rustj::testkit::*;

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
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编译 ThreadGate;载入注册表。
    let dir = compile_dir(SOURCE, "ThreadGate", &[]);
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
        run_result(&registry, "ThreadGate", "tid", "()J").0.unwrap(),
        Value::Long(1),
        "main 线程 tid 须为 1"
    );

    // 4) isMain() → true("main".equals(getName()))。
    assert_eq!(
        run_result(&registry, "ThreadGate", "isMain", "()Z").0.unwrap(),
        Value::Int(1),
        "main 线程 name 须为 \"main\""
    );
}
