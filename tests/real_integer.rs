//! 集成探测(4.10g 之后):用 `load_closure` 预载 `java/lang/Integer` 的整个引用闭包(真类覆盖桩),
//! 再跑 `Integer.valueOf(42).intValue()`——端到端验证**真实 java.base 类的 `<clinit>`(IntegerCache)
//! + 静态方法 + 构造器 + 实例字段** 全链经解释器执行。
//!
//! 选 Integer(而非 String):Integer 是普通类(无 `Oop::String` 那种特殊变体冲突),`valueOf`/
//! `intValue` 均为真字节码,直击「加载并运行真实 java.base 类」北极星。
//!
//! **当前状态(`#[ignore]`):** 4.10g 已让 `Integer.<clinit>` 跑过
//! `Class.getPrimitiveClass` / `desiredAssertionStatus` / 类字面量 ldc;但 `Integer$IntegerCache.<clinit>`
//! 仍抛 `ExceptionInInitializerError`——根因 `VM.getSavedProperty` 检测 `savedProps==null` 抛
//! `IllegalStateException("Not yet initialized")`。修复需**JDK 系统属性引导**层:
//! 在用户代码前运行 `VM.saveProperties(new HashMap<>())`(等价 launcher 的 System.initializeSystemClass),
//! 其连带依赖 `Runtime.maxMemory`(native)、真 `HashMap` 方法、**真 `String` 方法**(`"true".equals(...)`
//! 于 `Oop::String`)——后者正是「退役 Oop::String 特殊变体、加载真 String 类」的契机。
//! 本测试是『JDK 引导』层的回归闸门:转绿即移除 `#[ignore]`。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

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
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

/// javac 编译单个 public 类到临时目录,返回该目录。
fn compile_dir(source: &str, public_name: &str) -> PathBuf {
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

const SOURCE: &str = r#"
public class IntegerGate {
    // Integer.valueOf(42):命中 IntegerCache(-128..127)→ 缓存实例;intValue() 返回 value 字段 → 42。
    // 全程用真 java.base 的 Integer(Number 子类、Comparable 实现)+ 其 <clinit>(IntegerCache)。
    public static int run() {
        return Integer.valueOf(42).intValue();
    }
}
"#;

/// **探测**:真 `java/lang/Integer`(经闭包预载)的 `<clinit>` + `valueOf` + 构造器 + `intValue` 端到端。
/// 见模块文档「当前状态」——待『JDK 系统属性引导』层转绿。
#[test]
#[ignore = "待 JDK 系统属性引导层(savedProps/HashMap/真 String 方法);见模块文档"]
fn real_integer_valueof_intvalue_runs() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 IntegerGate;载入注册表。
    let dir = compile_dir(SOURCE, "IntegerGate");
    let mut registry = ClassRegistry::new();
    let ng = rustj::classfile::parse(&std::fs::read(dir.join("IntegerGate.class")).unwrap()).unwrap();
    registry.load(ng).unwrap();

    // 2) 真 java.base.jmod 加入 ClassPath;闭包预载 Integer(及其引用:Number/Comparable/Object/
    //    Integer$IntegerCache/…),真类覆盖合成桩。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let loaded = load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    assert!(loaded >= 1, "闭包应载入 Integer 本身,实际:{loaded}");

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

    // 4) 跑 IntegerGate.run():Integer.<clinit>(IntegerCache)→ valueOf(42)(命中缓存)→ intValue()→ 42。
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
    let mut vm = Vm::new(&registry);
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
