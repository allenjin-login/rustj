//! 集成闸门(Layer 4.10i,退役 `Oop::String`):预载真 `java/lang/String`(其引用闭包,
//! 含 `StringLatin1`/`StringUTF16`),由 rustj 执行含字符串字面量与 String 方法的真 Java。
//! 验证:
//! - `ldc`/`ldc_w` 取 `CONSTANT_String` → 构造**真** String 实例 → intern(同字面量恒同引用,
//!   故 `"x" == "x"` 成立);
//! - `String.length()` / `equals` / `hashCode` 经**真字节码**(分派到 `StringLatin1`),
//!   非 native 桩(4.10h 脚手架已删)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread};

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

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-sl-{pid}-{s}-{public_name}",
        pid = std::process::id()
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
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功"))
                .expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(
                cf.constant_pool.get(m.name_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == name
            );
            let d = matches!(
                cf.constant_pool.get(m.descriptor_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == desc
            );
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 运行 `class_name.name(desc)`(无参静态方法,带异常表)。抛 Java 异常时带出类名便于诊断。
fn run(reg: &std::sync::Arc<ClassRegistry>, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp =
        Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let mut vm = VmThread::new(std::sync::Arc::clone(reg));
    match interp.interpret_with(&mut frame, &mut vm) {
        Ok(v) => v,
        Err(rustj::runtime::VmError::ThrownException(r)) => {
            use rustj::oops::Oop;
            let cls = match vm.heap().get(r) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            panic!("{name}{desc} 抛 Java 异常:{cls}")
        }
        Err(e) => panic!("{name}{desc} 执行失败:{e}"),
    }
}

fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

const SOURCE: &str = r#"
public class StringGate {
    // 1. 返回字符串字面量:ldc "hello" → 构造真 String 实例 + areturn。
    public static String greet() {
        return "hello";
    }

    // 2. 真 String.length()(StringLatin1 路径):"hello" → 5。证 ldc 落字节数组 + length 字节码。
    public static int greetLength() {
        return "hello".length();
    }

    // 3. 同字面量 == :ldc + ldc + if_acmpeq(intern 给同引用)。
    public static boolean sameLiteral() {
        return "x" == "x";
    }

    // 4. 经局部变量承载同一字面量。
    public static boolean sameViaLocal() {
        String a = "x";
        String b = "x";
        return a == b;
    }

    // 5. 不同字面量 != :intern 给出不同引用。
    public static boolean diffLiteral() {
        return "a" == "b";
    }

    // 6. 真 String.equals(Object)(StringLatin1.equals 逐字节):"abc".equals("abc") → true。
    public static boolean selfEquals() {
        return "abc".equals("abc");
    }

    // 7. 真 String.equals 深路径:`new String("abc")` 与字面量 "abc" **不同引用**(避开
    //    `this == o` 短路),经 instanceof + StringLatin1.equals 逐字节比较 → true。
    //    String(String) 构造器仅 4 字段拷贝(String.java:295),故能端到端跑通。
    public static boolean distinctRefEquals() {
        return "abc".equals(new String("abc"));
    }

    // 8. 真 String.hashCode()(StringLatin1.hashCode:h=31*h+(v&0xff)):"abc" → 96354。
    public static int abcHashCode() {
        return "abc".hashCode();
    }
}
"#;

/// `"abc".hashCode()` 的 Java 规范值:h=97 → 31*97+98=3105 → 31*3105+99=96354。
const ABC_HASH: i32 = 96354;

/// 加载真 `java/lang/String` 闭包(含 StringLatin1/StringUTF16)到 `reg`。
fn load_real_string(reg: &mut ClassRegistry, jmod: &Path) {
    let bytes = std::fs::read(jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let loaded = load_closure(reg, &cp, "java/lang/String").unwrap();
    assert!(loaded >= 1, "闭包应载入 String 本身,实际:{loaded}");
}

#[test]
fn greet_length_via_real_string_bytecode() {
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "greetLength", "()I")), 5);
}

#[test]
fn same_literal_is_equal() {
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "sameLiteral", "()Z")), 1);
}

#[test]
fn same_literal_via_local_is_equal() {
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "sameViaLocal", "()Z")), 1);
}

#[test]
fn different_literals_are_not_equal() {
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "diffLiteral", "()Z")), 0);
}

#[test]
fn real_string_equals_via_bytecode() {
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "selfEquals", "()Z")), 1);
}

#[test]
fn real_string_equals_distinct_ref() {
    // 强于 selfEquals:`new String("abc")` 与字面量不同引用,绕开 `this == o` 短路,
    // 真正经 instanceof + StringLatin1.equals 逐字节比较。
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "distinctRefEquals", "()Z")), 1);
}

#[test]
fn real_string_hashcode_matches_java() {
    if !javac_available() || find_javabase_jmod().is_none() {
        eprintln!("跳过:无 javac 或 java.base.jmod");
        return;
    }
    let jmod = find_javabase_jmod().unwrap();
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "abcHashCode", "()I")), ABC_HASH);
}
