//! 集成闸门(4.10s):真 `Throwable.getMessage()` / `getCause()` 经真实例字段。
//!
//! 三静态法,成功返 1/2/3,失配返负诊断:
//! - `autoMessage`:idiv→ArithmeticException(由 JVM **自动**抛出,不经真 `<init>`),
//!   catch 内 `e.getMessage().equals("/ by zero")` → 1。修前 `detailMessage` 字段未填
//!   (new_instance 跳过 `<init>`,record_message 仅写并行 exception_meta)→ getMessage 返
//!   null → `.equals` 抛 NPE;修后 throw_exception_with_message 直接回填真字段。
//! - `userMessage`:`throw new RuntimeException("boom")`(经真 `<init>(String)` 自置 detailMessage)
//!   → 2。验证既有真 `<init>` 路径。
//! - `userCause`:`throw new RuntimeException("boom", root)`(经真 `<init>(String,Throwable)`
//!   自置 cause 字段,无 toString 依赖)→ getCause() == root → 3。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

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
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

/// javac 编译单个类到唯一临时目录,返回该目录。
fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-tm-{n}-{}-{public_name}", std::process::id()));
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

/// 按名 + 描述符在已加载类中找方法(`cf.methods` 线性扫)。
fn find_method<'a>(lc: &'a rustj::oops::LoadedClass, name: &str, desc: &str) -> &'a rustj::metadata::MethodInfo {
    use rustj::constant_pool::ConstantPoolEntry;
    lc.cf.methods.iter().find(|m| {
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 Tm.{name}{desc}"))
}

const SOURCE: &str = r#"
public class Tm {
    // JVM 自动抛出(idiv 除零),不经真 <init>:detailMessage 须由 throw_exception_with_message 回填。
    public static int autoMessage() {
        try { int x = 1 / 0; return x; }
        catch (ArithmeticException e) {
            return e.getMessage().equals("/ by zero") ? 1 : -1;
        }
    }
    // 用户抛出:经真 RuntimeException.<init>(String) 自置 detailMessage。
    public static int userMessage() {
        try { throw new RuntimeException("boom"); }
        catch (RuntimeException e) {
            return e.getMessage().equals("boom") ? 2 : -2;
        }
    }
    // 用户包裹:经真 RuntimeException.<init>(String,Throwable) 自置 cause(无 toString 依赖)。
    public static int userCause() {
        Exception root = new Exception("root");
        try { throw new RuntimeException("boom", root); }
        catch (RuntimeException e) {
            Throwable c = e.getCause();
            if (c == null) return -3;
            if (c == root) return 3;
            return -31;
        }
    }
}
"#;

/// 跑 `Tm.<name>()I`(无参静态)→ 返回 int 值;抛 Java 异常则 panic(给出诊断)。
fn run_int(vm: &mut VmThread, name: &str) -> i32 {
    let reg = vm.registry().expect("Tm 须已加载");
    let lc = reg.get("Tm").expect("Tm 须已加载");
    let m = find_method(&lc, name, "()I");
    let code = m.code.as_ref().expect("{name} 须有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => n,
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            panic!("Tm.{name} 抛 Java 异常:{cls}(getMessage/getCause 链有缺口)")
        }
        other => panic!("Tm.{name} 应返 int,得 {other:?}"),
    }
}

/// **集成闸门**:getMessage/getCause 经真实例字段回读正确。
#[test]
fn get_message_and_get_cause_via_real_fields() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 Tm;载入注册表。
    let dir = compile_dir(SOURCE, "Tm");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("Tm.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 2) 预载真 java.base 的 ArithmeticException(闭包带入 RuntimeException/Exception/Throwable/
    //    Object)+ String(getMessage/equals 返回真 String)。Vm 以不可变借用持注册表,运行期
    //    不可追加 → 须在 Vm::new 前装好(同 4.10i/4.10r 预载约束)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ArithmeticException").unwrap();
    assert!(!registry.get("java/lang/ArithmeticException").unwrap().is_synthetic_stub(), "ArithmeticException 须为真类");
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    // 3) getMessage/getCause 须为真字节码(非 native)——证字段回读走真字节码。
    let thr = registry.get("java/lang/Throwable").unwrap();
    let gm = find_method(&thr, "getMessage", "()Ljava/lang/String;");
    assert!(!gm.access_flags.is_native(), "Throwable.getMessage 须为真字节码");
    let gc = find_method(&thr, "getCause", "()Ljava/lang/Throwable;");
    assert!(!gc.access_flags.is_native(), "Throwable.getCause 须为真字节码");

    let mut vm = VmThread::new(registry);

    // 4) 三法逐一断言:1=自动抛出 detailMessage 回填,2=用户 <init>(String),3=<init>(String,Throwable)。
    assert_eq!(run_int(&mut vm, "autoMessage"), 1, "自动抛出 ArithmeticException 的 getMessage 须为 \"/ by zero\"");
    assert_eq!(run_int(&mut vm, "userMessage"), 2, "用户抛出 RuntimeException(\"boom\") 的 getMessage 须为 \"boom\"");
    assert_eq!(run_int(&mut vm, "userCause"), 3, "用户包裹 RuntimeException(\"boom\",root) 的 getCause 须 == root");
}
