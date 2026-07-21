//! 集成闸门(Layer 4.41 / Phase B.1):用 `javac` 编译调用 `Thread.currentThread().threadId()`
//! 与 `getName()` 的最小真 Java 程序,从真实 `java.base.jmod` 加载 `Thread`(连同传递依赖),
//! 由 rustj 解释器执行——端到端验证 main 线程镜像 `name="main"`、`tid=1`(`alloc_main_thread`
//! 置字段 → 真字节码 `threadId()`/`getName()` 读字段返正确值)。
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
