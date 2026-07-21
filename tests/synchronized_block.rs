//! 集成闸门(Layer 4.41 / Phase B.1):用 `javac` 编译含 `synchronized` 块的真 Java 程序,
//! 从真实 `java.base.jmod` 加载 `Object`/`Thread`/`RuntimeException`,由 rustj 解释器执行——
//! 端到端验证 `monitorenter`/`monitorexit` 真实管程语义:获取→`holdsLock` 真、释放→假、
//! 同线程重入、块内抛出时异常表驱动 `monitorexit` 释放。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::classfile::parse;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
public class SyncGate {
    // 基本:synchronized 块体执行并返回(javac 编译为 monitorenter / body / monitorexit)。
    public static int blockReturns() {
        Object lock = new Object();
        synchronized (lock) {
            return 42;
        }
    }

    // holdsLock 在块内为 true —— monitorenter 已获取管程。
    public static boolean holdsInside() {
        Object lock = new Object();
        synchronized (lock) {
            return Thread.holdsLock(lock);
        }
    }

    // 块外 holdsLock 为 false —— monitorexit 已释放。
    public static boolean holdsOutside() {
        Object lock = new Object();
        synchronized (lock) {
            // 空块:monitorenter + monitorexit。
        }
        return Thread.holdsLock(lock);
    }

    // 同线程重入:嵌套 synchronized 同锁(holdsLock 仍 true,count 累加)。
    public static boolean nestedReentry() {
        Object lock = new Object();
        synchronized (lock) {
            synchronized (lock) {
                return Thread.holdsLock(lock);
            }
        }
    }

    // 块内抛出:异常表驱动 monitorexit 释放;catch 后 holdsLock 为 false。
    public static boolean throwsAndReleases() {
        Object lock = new Object();
        try {
            synchronized (lock) {
                throw new RuntimeException("boom");
            }
        } catch (RuntimeException e) {
            return Thread.holdsLock(lock);
        }
    }
}
"#;

/// **RED→GREEN**(S6):`monitorenter`/`monitorexit` 真实管程语义端到端。
#[test]
fn synchronized_block_uses_real_monitor_semantics() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编译 SyncGate;载入注册表。
    let dir = compile_dir(SOURCE, "SyncGate", &[]);
    let mut registry = ClassRegistry::new();
    let sg = parse(&std::fs::read(dir.join("SyncGate.class")).unwrap()).unwrap();
    registry.load(sg).unwrap();

    // 2) 真 Object / Thread / RuntimeException / String 从 jmod 载入(连同传递依赖)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in ["java/lang/Object", "java/lang/Thread", "java/lang/RuntimeException", "java/lang/String"] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    let mut vm = VmThread::new(registry);

    // 基本:块体执行并返回 42。
    assert_eq!(
        run_static_in(&mut vm, "SyncGate", "blockReturns", "()I").unwrap(),
        Value::Int(42),
        "synchronized 块体应执行并返回"
    );

    // 判别性:holdsLock 块内 = true(monitorenter 已获取)。
    assert_eq!(
        run_static_in(&mut vm, "SyncGate", "holdsInside", "()Z").unwrap(),
        Value::Int(1),
        "块内 holdsLock 须为 true(管程已获取)"
    );

    // 块外 holdsLock = false(monitorexit 已释放)。
    assert_eq!(
        run_static_in(&mut vm, "SyncGate", "holdsOutside", "()Z").unwrap(),
        Value::Int(0),
        "块外 holdsLock 须为 false(管程已释放)"
    );

    // 重入:嵌套 synchronized 同锁,holdsLock 仍 true。
    assert_eq!(
        run_static_in(&mut vm, "SyncGate", "nestedReentry", "()Z").unwrap(),
        Value::Int(1),
        "同线程重入后 holdsLock 须为 true"
    );

    // 块内抛出:异常表驱动 monitorexit;catch 后 holdsLock = false。
    assert_eq!(
        run_static_in(&mut vm, "SyncGate", "throwsAndReleases", "()Z").unwrap(),
        Value::Int(0),
        "块内抛出后 monitorexit 须经异常表释放 → holdsLock=false"
    );
}
