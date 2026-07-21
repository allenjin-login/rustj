//! 探测闸门:int↔string 全往返(`StringBuilder.append(int).toString()`、`String.valueOf(int)`、
//! `Integer.parseInt`)。验证 4.10w putByte/getByte + DecimalDigits 链端到端。绿则证明 int↔string
//! 真实可用;红则首个失败即下一层。需 javac + java.base.jmod;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class IntStr {
    // StringBuilder.append(int).toString() → "123" → 长度 3。
    public static int sbToStringLen() {
        return new StringBuilder().append(123).toString().length();
    }
    // String.valueOf(int) → "-5" → 长度 2。
    public static int valueOfLen() {
        return String.valueOf(-5).length();
    }
    // Integer.parseInt(String) → 42。
    public static int parse() {
        return Integer.parseInt("42");
    }
}
"#;



/// **探测**:int↔string 全往返。
#[test]
fn int_string_round_trip() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "IntStr", &[]);
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("IntStr.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // 预载 StringBuilder + Integer 闭包(连带 DecimalDigits/Arrays/System 等)。
    load_closure(&mut registry, &cp, "java/lang/StringBuilder").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

    let mut vm = VmThread::new(registry);
    let n = run_static_int(&mut vm, "IntStr", "sbToStringLen").unwrap_or_else(|e| panic!("sbToStringLen 失败:{e:?}"));
    assert_eq!(n, 3, "append(123).toString() → \"123\" 长度 3");
    let v = run_static_int(&mut vm, "IntStr", "valueOfLen").unwrap_or_else(|e| panic!("valueOfLen 失败:{e:?}"));
    assert_eq!(v, 2, "String.valueOf(-5) → \"-5\" 长度 2");
    let p = run_static_int(&mut vm, "IntStr", "parse").unwrap_or_else(|e| panic!("parse 失败:{e:?}"));
    assert_eq!(p, 42, "Integer.parseInt(\"42\") → 42");
}
