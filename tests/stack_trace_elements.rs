//! 集成闸门(4.10r):真 `Throwable.getStackTrace()` → `StackTraceElement[]`。
//!
//! `St { deep(){1/0} mid(){deep()} top(){mid()} }` —— `top` 经两层调用到 `deep` 抛
//! `ArithmeticException`。在 Rust 侧捕获 `ThrownException(r)` 后,跑 `St.check(r)`
//! (Java 侧调 `e.getStackTrace()`,经**真** `StackTraceElement` 的 getter
//! `getClassName`/`getMethodName`/`getLineNumber` + `String.equals` 断言调用链
//! `deep→mid→top`),成功返 `st.length`(=3)。
//!
//! 验证:① 桩 `Throwable` 声明 native `getStackTrace` 使 `invokevirtual` 解析命中并触发
//! native 分派;② native 经捕获帧快照构造**真** `StackTraceElement[]`(字段回填);
//! ③ 真 STE getter(纯字段读字节码)在回填对象上正确返回;④ 行号经 `frame_source` 解析。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Reference, Value, Vm, VmError};

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
    let dir = std::env::temp_dir().join(format!("rustj-ste-{n}-{}-{public_name}", std::process::id()));
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
    }).unwrap_or_else(|| panic!("未找到方法 St.{name}{desc}"))
}

const SOURCE: &str = r#"
public class St {
    public static int deep() { return 1 / 0; }   // idiv 除零 → ArithmeticException
    public static int mid() { return deep(); }
    public static int top() { return mid(); }
    // 在 Java 侧调 getStackTrace(),经真 STE getter + String.equals 断言调用链。
    // 成功返 st.length(=3);各类失配返负诊断。
    public static int check(Throwable t) {
        StackTraceElement[] st = t.getStackTrace();
        if (st.length < 3) return -100 - st.length;
        if (!st[0].getClassName().equals("St")) return -10;
        if (!st[0].getMethodName().equals("deep")) return -1;
        if (!st[1].getMethodName().equals("mid")) return -2;
        if (!st[2].getMethodName().equals("top")) return -3;
        if (st[0].getLineNumber() <= 0) return -4;
        if (st[1].getLineNumber() <= 0) return -5;
        if (st[2].getLineNumber() <= 0) return -6;
        return st.length;
    }
}
"#;

/// 跑 `St.top` → 期望 `ThrownException`(ArithmeticException),回异常引用。
fn run_top(vm: &mut Vm) -> Reference {
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("St").expect("St 须已加载");
    let m = find_method(lc, "top", "()I");
    let code = m.code.as_ref().expect("top 须有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), "top");
    match interp.interpret_with(&mut frame, vm) {
        Err(VmError::ThrownException(r)) => r,
        other => panic!("St.top 应抛 ArithmeticException(ThrownException),得 {other:?}"),
    }
}

/// 跑 `St.check(Throwable)`(local[0]=exc)→ 返回诊断 int(3=成功)。
fn run_check(vm: &mut Vm, exc: Reference) -> Value {
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("St").expect("St 须已加载");
    let m = find_method(lc, "check", "(Ljava/lang/Throwable;)I");
    let code = m.code.as_ref().expect("check 须有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    frame.locals.set_reference(0, exc).unwrap();
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), "check");
    match interp.interpret_with(&mut frame, vm) {
        Ok(v) => v,
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            panic!("St.check 抛 Java 异常:{cls}(getStackTrace/STE getter 链有缺口)")
        }
        Err(e) => panic!("St.check 内部错误:{e:?}"),
    }
}

/// **集成闸门**:`getStackTrace()` 返回真 `StackTraceElement[]`,getter 回读调用链 deep/mid/top。
#[test]
fn get_stack_trace_returns_real_elements() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 St;载入注册表。
    let dir = compile_dir(SOURCE, "St");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("St.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 2) 预载真 java.base 的 StackTraceElement(及其引用闭包)+ String(getter 返回 / equals)。
    //    Vm 以不可变借用持注册表,运行期不可追加 → 须在 Vm::new 前装好(同 4.10i String 预载)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/StackTraceElement").unwrap();
    assert!(!registry.get("java/lang/StackTraceElement").unwrap().is_synthetic_stub(), "STE 须为真类");
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    // 3) 真 STE 的 getMethodName/getLineNumber 须为真字节码(非 native)——证 getter 在回填对象上可读。
    let ste = registry.get("java/lang/StackTraceElement").unwrap();
    let gm = find_method(ste, "getMethodName", "()Ljava/lang/String;");
    assert!(!gm.access_flags.is_native(), "STE.getMethodName 须为真字节码");
    let gl = find_method(ste, "getLineNumber", "()I");
    assert!(!gl.access_flags.is_native(), "STE.getLineNumber 须为真字节码");

    let mut vm = Vm::new(registry);

    // 4) St.top 抛 ArithmeticException → Rust 捕获引用(调用链已 record_trace 快照)。
    let exc = run_top(&mut vm);
    let cls = match vm.heap().get(exc) {
        Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
        o => panic!("异常须为 Instance,得 {o:?}"),
    };
    assert_eq!(cls, "java/lang/ArithmeticException");

    // 5) St.check(exc):getStackTrace → 真 STE[] → getter + equals 断言 → 返 3。
    //    interpret_with 在 unwind 上 push/pop 对称,故 top 抛出后可立即跑 check。
    let result = run_check(&mut vm, exc);
    assert_eq!(result, Value::Int(3), "getStackTrace 须返回 deep/mid/top 三帧且行号>0(诊断码见 St.check)");
}
