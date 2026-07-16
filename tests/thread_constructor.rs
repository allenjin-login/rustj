//! 集成闸门(Phase B.4a):用 `javac` 编译一个调真 `new Thread(runnable, name)` 构造器的 Java 程序,
//! 从 `java.base.jmod` 加载 `Thread`/`ThreadGroup`/`Runnable`/`Object`,由 rustj 解释器执行——端到端
//! 验证 `Thread` 构造器(`<init>` 字节码:`currentThread`→`parent.getThreadGroup`→`Math.min`→
//! `new FieldHolder`→`ThreadIdentifiers.next`→置 name)跑通,且 `getName`/`getId`/`getThreadGroup`/
//! `getPriority`/`isDaemon` 真字节码字段读返期望值。
//!
//! **B.4a 边界**:显式名 `new Thread(p, "w")`(绕开 `genThreadName`→`ThreadNumbering` 反射墙,顺延 4.15b)。
//! start/join 在 B.4b;interrupt 在 B.4c。本闸聚焦**构造器 + 字段读**。字符串比较留在 Java 侧
//!(`String.equals`),不读 rustj 私有 `string` 模块。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::{ClassRegistry, Oop};
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Reference, Value, VmThread};

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
    let dir = std::env::temp_dir().join(format!("rustj-ctor-{n}-{}-{public_name}", std::process::id()));
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

/// 解释执行一个静态方法,locals 从 `args` 顺序填入(引用/long/int)。返回值或 `VmError`。
fn run_static(
    registry: &std::sync::Arc<ClassRegistry>,
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    args: &[Value],
) -> Result<Value, rustj::runtime::VmError> {
    let lc = registry.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let mut slot: u16 = 0;
    for v in args {
        match v {
            Value::Long(x) => {
                frame.locals.set_long(slot, *x).unwrap();
                slot = slot.saturating_add(2);
            }
            Value::Int(x) => {
                frame.locals.set_int(slot, *x).unwrap();
                slot = slot.saturating_add(1);
            }
            Value::Reference(r) => {
                frame.locals.set_reference(slot, *r).unwrap();
                slot = slot.saturating_add(1);
            }
            _ => panic!("run_static 不支持该参数类型:{v:?}"),
        }
    }
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

const SOURCE: &str = r#"
public class Probe implements Runnable {
    public void run() {}
    public static Probe makeProbe() { return new Probe(); }
    public static Thread makeThread(Probe p) { return new Thread(p, "w"); }
    // 字符串比较留 Java 侧(String.equals),避免测试读 rustj 私有 string 模块。
    public static boolean nameIsW(Thread t) { return t.getName().equals("w"); }
    public static boolean groupIsMain(Thread t) {
        String n = t.getThreadGroup().getName();
        return n.equals("main") || n.equals("system");
    }
    public static long idOf(Thread t) { return t.getId(); }
    public static int priorityOf(Thread t) { return t.getPriority(); }
    public static boolean daemonOf(Thread t) { return t.isDaemon(); }
}
"#;

/// **RED→GREEN**(Phase B.4a):真 `new Thread(runnable, name)` 构造器端到端 + 字段读。
#[test]
fn thread_constructor_end_to_end() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 Probe;载入注册表。
    let dir = compile_dir(SOURCE, "Probe");
    let mut registry = ClassRegistry::new();
    let pcf = parse(&std::fs::read(dir.join("Probe.class")).unwrap()).unwrap();
    registry.load(pcf).unwrap();

    // 2) 真 Thread / ThreadGroup / Runnable / Object / String / Math 从 jmod 载入(构造器传递依赖)。
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

    // 3) 分配 Probe 实例(makeThread 的入参;fieldless,不跑 <init> 无碍)。
    let probe = {
        let lc = registry.get("Probe").unwrap();
        let inst = registry.new_instance(&lc);
        vm.heap_mut().alloc(Oop::Instance(inst))
    };

    // 4) makeThread(probe) → new Thread(p, "w") 构造器跑通,返 Thread 引用。
    let thread = match run_static(
        &registry,
        &mut vm,
        "Probe",
        "makeThread",
        "(LProbe;)Ljava/lang/Thread;",
        &[Value::Reference(probe)],
    )
    .expect("makeThread 应构造成功")
    {
        Value::Reference(r) => r,
        other => panic!("makeThread 须返 Thread 引用,得 {other:?}"),
    };
    assert!(!thread.is_null(), "new Thread(p,\"w\") 须返非 null");

    // 5) name == "w"(构造器置 this.name=name,非 null 故绕开 genThreadName)。
    let name_ok = match run_static(
        &registry,
        &mut vm,
        "Probe",
        "nameIsW",
        "(Ljava/lang/Thread;)Z",
        &[Value::Reference(thread)],
    )
    .expect("nameIsW 应非抛")
    {
        Value::Int(v) => v != 0,
        other => panic!("nameIsW 须返 boolean,得 {other:?}"),
    };
    assert!(name_ok, "getName().equals(\"w\") 须为 true");

    // threadGroup.getName() ∈ {main, system}(main 线程 holder.group)。
    let group_ok = match run_static(
        &registry,
        &mut vm,
        "Probe",
        "groupIsMain",
        "(Ljava/lang/Thread;)Z",
        &[Value::Reference(thread)],
    )
    .expect("groupIsMain 应非抛")
    {
        Value::Int(v) => v != 0,
        other => panic!("groupIsMain 须返 boolean,得 {other:?}"),
    };
    assert!(group_ok, "getThreadGroup().getName 须为 main/system");

    // id > 0(ThreadIdentifiers.next 递增;main 线程占 tid=1,子 ≥ 2)。
    let id = match run_static(
        &registry,
        &mut vm,
        "Probe",
        "idOf",
        "(Ljava/lang/Thread;)J",
        &[Value::Reference(thread)],
    )
    .expect("idOf 应非抛")
    {
        Value::Long(v) => v,
        other => panic!("idOf 须返 long,得 {other:?}"),
    };
    assert!(id > 0, "getId 须 > 0,得 {id}");

    // priority == 5(NORM_PRIORITY;main 持 NORM_PRIORITY,组 maxPriority=10,min=5)。
    let pri = match run_static(
        &registry,
        &mut vm,
        "Probe",
        "priorityOf",
        "(Ljava/lang/Thread;)I",
        &[Value::Reference(thread)],
    )
    .expect("priorityOf 应非抛")
    {
        Value::Int(v) => v,
        other => panic!("priorityOf 须返 int,得 {other:?}"),
    };
    assert_eq!(pri, 5, "getPriority 须 == NORM_PRIORITY(5),得 {pri}");

    // isDaemon == false(main 非 daemon → 子非 daemon)。
    let daemon = match run_static(
        &registry,
        &mut vm,
        "Probe",
        "daemonOf",
        "(Ljava/lang/Thread;)Z",
        &[Value::Reference(thread)],
    )
    .expect("daemonOf 应非抛")
    {
        Value::Int(v) => v,
        other => panic!("daemonOf 须返 boolean,得 {other:?}"),
    };
    assert_eq!(daemon, 0, "isDaemon 须 == false,得 {daemon}");
}

#[test]
fn type_check_reference() {
    // 确保 Reference: Copy(value 形)可经 Value::Reference 传递(编译期闸)。
    let r = Reference::null();
    let _v = Value::Reference(r);
    let _v2 = Value::Reference(r);
}
