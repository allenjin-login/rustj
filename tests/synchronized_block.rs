//! 集成闸门(Layer 4.41 / Phase B.1):用 `javac` 编译含 `synchronized` 块的真 Java 程序,
//! 从真实 `java.base.jmod` 加载 `Object`/`Thread`/`RuntimeException`,由 rustj 解释器执行——
//! 端到端验证 `monitorenter`/`monitorexit` 真实管程语义:获取→`holdsLock` 真、释放→假、
//! 同线程重入、块内抛出时异常表驱动 `monitorexit` 释放。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

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
    let dir = std::env::temp_dir().join(format!("rustj-sync-{n}-{}-{public_name}", std::process::id()));
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

/// 解释执行一个无参静态方法(带异常表——synchronized 块的 monitorexit 释放依赖异常表)。
fn run_static(registry: &std::sync::Arc<ClassRegistry>, vm: &mut Vm, class: &str, name: &str, desc: &str) -> Result<Value, VmError> {
    let lc = registry.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, &lc.cf.constant_pool, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

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
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 SyncGate;载入注册表。
    let dir = compile_dir(SOURCE, "SyncGate");
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
    let registry = std::sync::Arc::new(registry);
    let mut vm = Vm::new(std::sync::Arc::clone(&registry));

    // 基本:块体执行并返回 42。
    assert_eq!(
        run_static(&registry, &mut vm, "SyncGate", "blockReturns", "()I").unwrap(),
        Value::Int(42),
        "synchronized 块体应执行并返回"
    );

    // 判别性:holdsLock 块内 = true(monitorenter 已获取)。
    assert_eq!(
        run_static(&registry, &mut vm, "SyncGate", "holdsInside", "()Z").unwrap(),
        Value::Int(1),
        "块内 holdsLock 须为 true(管程已获取)"
    );

    // 块外 holdsLock = false(monitorexit 已释放)。
    assert_eq!(
        run_static(&registry, &mut vm, "SyncGate", "holdsOutside", "()Z").unwrap(),
        Value::Int(0),
        "块外 holdsLock 须为 false(管程已释放)"
    );

    // 重入:嵌套 synchronized 同锁,holdsLock 仍 true。
    assert_eq!(
        run_static(&registry, &mut vm, "SyncGate", "nestedReentry", "()Z").unwrap(),
        Value::Int(1),
        "同线程重入后 holdsLock 须为 true"
    );

    // 块内抛出:异常表驱动 monitorexit;catch 后 holdsLock = false。
    assert_eq!(
        run_static(&registry, &mut vm, "SyncGate", "throwsAndReleases", "()Z").unwrap(),
        Value::Int(0),
        "块内抛出后 monitorexit 须经异常表释放 → holdsLock=false"
    );
}
