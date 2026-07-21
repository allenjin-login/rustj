//! 集成闸门(Phase B.3c):用 `javac` 编译含 `Object.wait/notify/notifyAll` 的真 Java 程序,
//! 从真实 `java.base.jmod` 加载 `Object`/`System`/`Thread`/异常类,由 rustj 解释器执行——
//! 端到端验证 native 桥(`wait0(J)V` / `notify()V` / `notifyAll()V`)→ [`Vm::object_wait`] 等真阻塞语义:
//! wait(timeout) 真阻塞约 timeout、notify/notifyAll 无等待者时 no-op(不抛)、wait 未持管程→IMSE。
//!
//! **JDK25 实况**:真 `Object.wait(J)V` 为字节码包装(Object.java:377),最终调 private native
//! `wait0(J)V`(396);故本闸端到端覆盖 native 表的 `wait0` 绑定(经字节码 wait→wait0 路径)。
//!
//! 真「生产者-消费者」(双真实 Thread.start)阻塞于 Thread 构造器 ThreadGroup/holder 引导(B.4);
//! 跨线程 wait/notify 协调已由 lib 闸 `concurrent_wait_tests` 覆盖。本闸聚焦 native 桥端到端。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};
use rustj::testkit::*;

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

/// 解释执行一个无参静态方法(带异常表——wait 字节码包装的 try-catch 依赖异常表)。
fn run_static(
    registry: &std::sync::Arc<ClassRegistry>,
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
) -> Result<Value, VmError> {
    let lc = registry.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, &lc.cf.constant_pool, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

const SOURCE: &str = r#"
public class WaitGate {
    // wait(80) 须真阻塞 ~80ms(非立返)。返回 currentTimeMillis 差;调用侧断言 >= 40ms。
    // 经 synchronized → wait(J) 字节码 → wait0(J) native → Vm::object_wait(80) 阻塞。
    public static long waitBlocksForTimeout() {
        Object lock = new Object();
        synchronized (lock) {
            long start = System.currentTimeMillis();
            try { lock.wait(80); } catch (InterruptedException e) { /* 不应中断 */ }
            return System.currentTimeMillis() - start;
        }
    }

    // notify/notifyAll 无等待者时 no-op(不抛)。synchronized 块内调(持有管程,过 CHECK_OWNER)。
    public static boolean notifyNoOpWhenNoWaiter() {
        Object lock = new Object();
        synchronized (lock) {
            lock.notify();
            lock.notifyAll();
            return true;
        }
    }

    // 未持管程调 wait → IMSE(ObjectSynchronizer::wait CHECK_OWNER)。wait0 native 抛,经 wait(J) 字节码透传。
    public static boolean waitOutsideSynchronizedThrowsImse() {
        Object lock = new Object();
        try {
            lock.wait(80);
            return false; // 不应到达
        } catch (IllegalMonitorStateException e) {
            return true;
        } catch (InterruptedException e) {
            return false; // 不应是中断
        }
    }
}
"#;

/// **RED→GREEN**:Object.wait/notify/notifyAll native 桥端到端真阻塞语义。
#[test]
fn object_wait_notify_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编译 WaitGate;载入注册表。
    let dir = compile_dir(SOURCE, "WaitGate", &[]);
    let mut registry = ClassRegistry::new();
    let wg = parse(&std::fs::read(dir.join("WaitGate.class")).unwrap()).unwrap();
    registry.load(wg).unwrap();

    // 2) 真 Object / System / Thread / 异常类 / VirtualThread(wait(J) 字节码 instanceof 引用)从 jmod 载入。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in [
        "java/lang/Object",
        "java/lang/System",
        "java/lang/Thread",
        "java/lang/VirtualThread",
        "java/lang/IllegalMonitorStateException",
        "java/lang/InterruptedException",
        "java/lang/String",
    ] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    let registry = std::sync::Arc::new(registry);
    let mut vm = VmThread::new(std::sync::Arc::clone(&registry));

    // wait(80) 真阻塞 ~80ms:no-op wait 立返 → 差 < 40;真阻塞 → 差 >= 40(且 < 1000 防死锁)。
    match run_static(&registry, &mut vm, "WaitGate", "waitBlocksForTimeout", "()J").unwrap() {
        Value::Long(elapsed) => {
            assert!(
                elapsed >= 40,
                "wait(80) 须阻塞 ~80ms,实际仅 {elapsed}ms(no-op wait 立返)"
            );
            assert!(
                elapsed < 1000,
                "wait(80) 不应死锁/超长阻塞,实际 {elapsed}ms"
            );
        }
        other => panic!("waitBlocksForTimeout 须返 long,得 {other:?}"),
    }

    // notify/notifyAll 无等待者:no-op 不抛 → true。
    assert_eq!(
        run_static(&registry, &mut vm, "WaitGate", "notifyNoOpWhenNoWaiter", "()Z").unwrap(),
        Value::Int(1),
        "notify/notifyAll 无等待者须 no-op(不抛)"
    );

    // 未持管程调 wait → IMSE → catch → true。
    assert_eq!(
        run_static(&registry, &mut vm, "WaitGate", "waitOutsideSynchronizedThrowsImse", "()Z").unwrap(),
        Value::Int(1),
        "未持管程调 wait 须抛 IllegalMonitorStateException"
    );
}
