//! 集成闸门(Phase B.4 收尾):子线程 `run()` 抛未捕获异常时,VM 须分派
//! `Thread.dispatchUncaughtException(e)`(`getUncaughtExceptionHandler().uncaughtException(this, e)`)
//! ——而非静默吞掉(原 spawn 闭包 `let _ = res;`)。用自定义 `UncaughtExceptionHandler`(lambda 记录
//! throwable 到静态字段)端到端验证分派路径。
//!
//! **HotSpot 语义**:`JavaThread` 跑完 `run()`(或抛出)后,VM 在终止前调
//! `Thread.dispatchUncaughtException(Throwable)`(Thread.java:2561,包私有字节码)→
//! `getUncaughtExceptionHandler().uncaughtException(this, e)`。自定义 handler 字段非 null 时用它,
//! 否则用 ThreadGroup(rustj 默认路径顺延 stderr 打印)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread};
use rustj::testkit::*;

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
    public static Throwable caught = null;
    public void run() {
        throw new RuntimeException("boom");
    }
    public static int uncaughtDispatched() {
        caught = null;
        Probe p = new Probe();
        Thread t = new Thread(p, "w");
        t.setUncaughtExceptionHandler(new java.lang.Thread.UncaughtExceptionHandler() {
            public void uncaughtException(Thread th, Throwable e) {
                caught = e;
            }
        });
        t.start();
        try { t.join(); } catch (InterruptedException e) {}
        return (caught != null && "boom".equals(caught.getMessage())) ? 1 : 0;
    }
}
"#;

/// **RED→GREEN**(Phase B.4 收尾):子线程 `run()` 抛未捕获异常 → VM 分派 dispatchUncaughtException
/// → 自定义 handler 记录 throwable(caught != null 且 message=="boom")。
#[test]
fn uncaught_exception_dispatched() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "Probe", &[]);
    let mut registry = ClassRegistry::new();
    let pcf = parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap();
    registry.load(pcf).unwrap();
    // 匿名 handler 类 Probe$1(javac 生成;非 ClassPath → 显式加载)。
    let p1 = parse(&std::fs::read(dir.join("Probe$1.class")).unwrap()).unwrap();
    registry.load(p1).unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in [
        "java/lang/Thread",
        "java/lang/Thread$UncaughtExceptionHandler",
        "java/lang/ThreadGroup",
        "java/lang/Runnable",
        "java/lang/Object",
        "java/lang/String",
        "java/lang/Math",
        "java/lang/RuntimeException",
    ] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }
    let registry = std::sync::Arc::new(registry);
    let mut vm = VmThread::new(std::sync::Arc::clone(&registry));

    let result = match run_static(&registry, &mut vm, "Probe", "uncaughtDispatched", "()I")
        .expect("uncaughtDispatched 应非抛")
    {
        Value::Int(v) => v,
        other => panic!("uncaughtDispatched 须返 int,得 {other:?}"),
    };
    assert_eq!(
        result, 1,
        "子线程未捕获异常须经 dispatchUncaughtException 分派到自定义 handler(caught 非 null 且 message=='boom')"
    );
}
