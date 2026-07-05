//! 集成闸门(Layer 4.13):**VM 运行时初始化 Phase 1 —— 系统属性引导原生能力**。
//!
//! 真 JVM 由 native launcher(`Threads::create_vm` 后)调 `System.initPhase1-3`
//!(`System.java:1724/1929/1952`)完成运行时初始化。Phase 1 的核心 `VM.saveProperties`
//!(`VM.java:237`)置 `savedProps` 后,`VM.getSavedProperty`(`VM.java:209`)才不抛
//! `IllegalStateException("Not yet initialized")`——凡读 savedProps 的 `<clinit>`
//!(Integer/Long/Boolean/… 的缓存)都依赖它先跑。
//!
//! 修前:测试须用 `RustjBootstrap` Java 辅助类手动调 `VM.saveProperties(new HashMap<>())`
//! 充数(见 `real_integer.rs`)。本闸门验证 Layer 4.13 把它收编为 VM 原生能力
//!(`initialize_system_class`):**无任何 Java 辅助类**,直接 `Integer.valueOf(42).intValue()=42`、
//! `VM.getSavedProperty("x")` 返 null(非异常)、`VM.initLevel()`=1。需 `javac` + 本机 jmod;缺一跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

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

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-boot-{n}-{}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// 解释执行一个**无参静态方法**(共用传入 Vm)。抛 Java 异常时把类名带出,便于定位下一缺口。
fn run_static_int(vm: &mut Vm<'_>, class: &str, name: &str) -> Result<i32, String> {
    let lc = vm
        .registry()
        .and_then(|r| r.get(class))
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {class}.{name}()I"));
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance Oop:{o:?})"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

const SOURCE: &str = r#"
public class Boot {
    // Integer.valueOf(42):触发 Integer$IntegerCache.<clinit> → VM.getSavedProperty。
    // 仅当 savedProps 已引导(Phase 1)才不抛 IllegalStateException。
    public static int run() {
        return Integer.valueOf(42).intValue();
    }
}
"#;

/// **集成闸门**:VM 原生 Phase 1 引导(`initialize_system_class`)→ 无 Java 辅助类即可跑真 java.base。
#[test]
fn initialize_system_class_bootstraps_saved_props() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编 Boot(无 RustjBootstrap 辅助类);载入注册表。
    let dir = compile_dir(SOURCE, "Boot");
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("Boot.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 Integer(→ Number/Object/VM/HashMap/Runtime/…)
    //    + 显式 HashMap(String clinit 链路用)。**不预编译任何 RustjBootstrap**。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    assert!(!registry.get("java/lang/Integer").unwrap().is_synthetic_stub(), "Integer 须为真类");

    let mut vm = Vm::new(&registry);

    // 3) **VM 原生 Phase 1 引导**(替代旧 RustjBootstrap.init()):savedProps 置空 HashMap、initLevel(1)。
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    // 4) VM.initLevel() = 1(Phase 1 完成);VM.getSavedProperty("x") = null(非 IllegalStateException)。
    assert_eq!(run_static_int(&mut vm, "jdk/internal/misc/VM", "initLevel"), Ok(1), "initLevel 须为 1");
    // getSavedProperty(String) 返 String;未设键 → null。返 null 表示 savedProps 已就绪(否则抛异常)。
    let lc = vm.registry().and_then(|r| r.get("jdk/internal/misc/VM")).expect("VM 须已加载");
    let get_prop = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "getSavedProperty");
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "(Ljava/lang/String;)Ljava/lang/String;");
        n && d
    }).expect("VM 须有 getSavedProperty");
    let code = get_prop.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    // local[0] = null String 实参(getSavedProperty 在 savedProps 就绪时不抛,直接返 Map.get → null)。
    frame.locals.set_reference(0, rustj::runtime::Reference::null()).unwrap();
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, &mut vm) {
        Ok(Value::Reference(r)) if r.is_null() => { /* 期望:null */ }
        Ok(other) => panic!("getSavedProperty 期望 null,得 {other:?}"),
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                _ => "<unknown>".into(),
            };
            panic!("getSavedProperty 不应抛异常(savedProps 须已就绪),却抛:{cls}");
        }
        Err(e) => panic!("getSavedProperty 内部错误:{e:?}"),
    }

    // 5) 真程序 Boot.run():Integer.valueOf(42).intValue() = 42(IntegerCache.<clinit> 不再失败)。
    assert_eq!(run_static_int(&mut vm, "Boot", "run"), Ok(42), "Integer.valueOf 须可跑(savedProps 已引导)");
}
