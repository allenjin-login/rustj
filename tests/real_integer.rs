//! 集成闸门(4.10h,转绿):`load_closure` 预载 `java/lang/Integer` 的整个引用闭包
//! (真类覆盖桩),先跑 `RustjBootstrap.init()` 引导 `VM.savedProps`(等价 launcher 的
//! `System.initializeSystemClass` 片段),再跑 `Integer.valueOf(42).intValue()` → 42——
//! 端到端验证**真实 java.base 类的 `<clinit>`(IntegerCache)+ 静态方法 + 构造器 + 实例字段**
//! 全链经解释器执行。
//!
//! 选 Integer(而非 String):Integer 是普通类(无 `Oop::String` 那种特殊变体冲突),`valueOf`/
//! `intValue` 均为真字节码,直击「加载并运行真实 java.base 类」北极星。
//!
//! **4.10h 解锁链(Step 0 源码核对):** `Integer$IntegerCache.<clinit>` 调
//! `VM.getSavedProperty` —— `VM.getSavedProperty`(VM.java:209)于 `savedProps==null` 抛
//! `IllegalStateException("Not yet initialized")`。引导 = `VM.saveProperties(new HashMap<>())`
//! (VM.java:237):`savedProps=props` 后,`directMemory = Runtime.getRuntime().maxMemory()`
//! (Runtime.<clinit>=`new Runtime()`,`<init>` 空体;maxMemory native),`pageAlignDirectMemory =
//! "true".equals(props.get(...))`(空表 `HashMap.get` 经 `table==null` 短路返 null,故仅
//! `String.hashCode`+`String.equals(null)`;String 脚手架见 native.rs)。Runtime 已在
//! Integer 闭包内(经 VM.saveProperties 字节码引用)传递性载入。
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

/// javac 编译单个类到唯一临时目录,返回该目录。`extra` 追加 javac 参数
/// (如 `--add-exports` 以访问 `jdk.internal.misc`)。
fn compile_dir(source: &str, public_name: &str, extra: &[&str]) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-integer-{n}-{}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .args(extra)
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

/// 解释执行一个**无参静态方法**(用调用者传入的 Vm,**不另建**)。抛 Java 异常时把类名带出,
/// 便于诊断"下一缺口"。
///
/// **关键约束:整段程序须共用同一 Vm。** 静态字段区虽存于共享注册表(跨调用持久),但其值是
/// **Vm 堆句柄**——堆随 Vm 析构而失效。故引导(写 `VM.savedProps`)与用户代码(读
/// `VM.savedProps`)必须同一 Vm,否则旧句柄在新堆里指向错对象。这对应真实 JVM 单一全局堆的
/// 约定:一个 JVM 实例一个堆,贯穿整个程序。
fn run_static_in(vm: &mut VmThread, class: &str, name: &str, desc: &str) -> Result<Value, String> {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表缺失"));
    let lc = reg
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            use rustj::constant_pool::ConstantPoolEntry;
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {class}.{name}{desc}"));
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(v) => Ok(v),
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
public class IntegerGate {
    // Integer.valueOf(42):命中 IntegerCache(-128..127)→ 缓存实例;intValue() 返回 value 字段 → 42。
    // 全程用真 java.base 的 Integer(Number 子类、Comparable 实现)+ 其 <clinit>(IntegerCache)。
    public static int run() {
        return Integer.valueOf(42).intValue();
    }
}
"#;

/// 系统属性引导(等价 launcher 的 `System.initializeSystemClass` 片段):在用户代码前设
/// `VM.savedProps` 为空 `HashMap`——否则 `VM.getSavedProperty` 抛 `IllegalStateException`,
/// `Integer$IntegerCache.<clinit>` 即失败。需 `--add-exports` 才能编译期访问 `jdk.internal.misc`。
const BOOTSTRAP_SRC: &str = r#"
import java.util.HashMap;
class RustjBootstrap {
    static void init() {
        jdk.internal.misc.VM.saveProperties(new HashMap<String, String>());
    }
}
"#;

/// **集成闸门**:真 `java/lang/Integer`(经闭包预载)的 `<clinit>` + `valueOf` + 构造器 + `intValue`
/// 端到端。先运行 `RustjBootstrap.init()` 设 `savedProps`,再跑 `IntegerGate.run()` → 42。
#[test]
fn real_integer_valueof_intvalue_runs() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 IntegerGate + RustjBootstrap;载入注册表。
    let dir = compile_dir(SOURCE, "IntegerGate", &[]);
    let bdir = compile_dir(
        BOOTSTRAP_SRC,
        "RustjBootstrap",
        &["--add-exports", "java.base/jdk.internal.misc=ALL-UNNAMED"],
    );
    let mut registry = ClassRegistry::new();
    let ng = rustj::classfile::parse(&std::fs::read(dir.join("IntegerGate.class")).unwrap()).unwrap();
    registry.load(ng).unwrap();
    let bsc = rustj::classfile::parse(&std::fs::read(bdir.join("RustjBootstrap.class")).unwrap()).unwrap();
    registry.load(bsc).unwrap();

    // 2) 真 java.base.jmod 加入 ClassPath;闭包预载 Integer(及其引用:Number/Comparable/Object/
    //    Integer$IntegerCache/jdk.internal.misc.VM/…,真类覆盖合成桩);并显式预载 HashMap
    //    (RustjBootstrap 的 `new HashMap` 与 saveProperties 的 `Map` 形参类型所需)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let loaded = load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    assert!(loaded >= 1, "闭包应载入 Integer 本身,实际:{loaded}");
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();
    assert!(registry.get("java/util/HashMap").is_some(), "HashMap 须已预载");
    // 真 String 须预载(4.10i):引导链 VM.saveProperties → HashMap.get(键 String.hashCode)
    // 与 "true".equals(...) 现走真 String 字节码(分派 StringLatin1),非 4.10h 的 native 桩。
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    assert!(registry.get("java/lang/String").is_some(), "String 须已预载");

    let registry = std::sync::Arc::new(registry);

    // 3) 真 Integer.intValue 须**非** native(桩无此法 → 证覆盖成功)。
    let int_lc = registry.get("java/lang/Integer").expect("Integer 须已注册");
    let int_value = int_lc
        .cf
        .methods
        .iter()
        .find(|m| {
            use rustj::constant_pool::ConstantPoolEntry;
            let n = matches!(int_lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "intValue");
            let d = matches!(int_lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .expect("真 Integer 须有 intValue()I");
    assert!(!int_value.access_flags.is_native(), "真 Integer.intValue 须非 native(真字节码)");

    // 4) 系统属性引导 + 5) IntegerGate.run() 须共用同一 Vm:静态字段(VM.savedProps)存于
    //    注册表,但其值是 Vm 堆句柄——堆随 Vm 析构失效,故引导与运行同 Vm(对应真实 JVM 单一
    //    全局堆贯穿整个程序的约定)。
    let mut vm = VmThread::new(std::sync::Arc::clone(&registry));
    // 系统属性引导:跑 RustjBootstrap.init()(= VM.saveProperties(new HashMap<>()),等价 launcher
    // 的 System.initializeSystemClass 片段)。savedProps 一旦非 null,后续 IntegerCache.<clinit>
    // 的 VM.getSavedProperty 即不再抛 IllegalStateException。抛异常即明确暴露下一缺口(类名)。
    if let Err(exc) = run_static_in(&mut vm, "RustjBootstrap", "init", "()V") {
        panic!("引导 RustjBootstrap.init 抛异常:{exc}");
    }

    // 5) 跑 IntegerGate.run():Integer.<clinit>→ IntegerCache.<clinit>(VM.getSavedProperty 现可读
    //    savedProps)→ valueOf(42)(命中缓存)→ intValue()→ 42。
    let ng_lc = registry.get("IntegerGate").expect("IntegerGate 须已注册");
    let method = ng_lc
        .cf
        .methods
        .iter()
        .find(|m| {
            use rustj::constant_pool::ConstantPoolEntry;
            let n = matches!(ng_lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "run");
            let d = matches!(ng_lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .expect("IntegerGate 须有 run()I");
    let code = method.code.as_ref().expect("run 须有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &ng_lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, &mut vm) {
        Ok(v) => assert_eq!(v, Value::Int(42), "valueOf(42).intValue() 须为 42"),
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r).expect("异常引用须在堆") {
                rustj::oops::Oop::Instance(i) => i.class_name().to_string(),
                o => format!("(非 Instance Oop:{o:?})"),
            };
            panic!("跑 IntegerGate.run 抛出 Java 异常:{cls}");
        }
        Err(e) => panic!("跑 IntegerGate.run 内部错误:{e:?}"),
    }
}
