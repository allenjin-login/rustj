//! 集成闸门(Layer 4.10i,退役 `Oop::String`):预载真 `java/lang/String`(其引用闭包,
//! 含 `StringLatin1`/`StringUTF16`),由 rustj 执行含字符串字面量与 String 方法的真 Java。
//! 验证:
//! - `ldc`/`ldc_w` 取 `CONSTANT_String` → 构造**真** String 实例 → intern(同字面量恒同引用,
//!   故 `"x" == "x"` 成立);
//! - `String.length()` / `equals` / `hashCode` 经**真字节码**(分派到 `StringLatin1`),
//!   非 native 桩(4.10h 脚手架已删)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::Path;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::testkit::*;

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
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "greetLength", "()I")), 5);
}

#[test]
fn same_literal_is_equal() {
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "sameLiteral", "()Z")), 1);
}

#[test]
fn same_literal_via_local_is_equal() {
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "sameViaLocal", "()Z")), 1);
}

#[test]
fn different_literals_are_not_equal() {
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "diffLiteral", "()Z")), 0);
}

#[test]
fn real_string_equals_via_bytecode() {
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "selfEquals", "()Z")), 1);
}

#[test]
fn real_string_equals_distinct_ref() {
    // 强于 selfEquals:`new String("abc")` 与字面量不同引用,绕开 `this == o` 短路,
    // 真正经 instanceof + StringLatin1.equals 逐字节比较。
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "distinctRefEquals", "()Z")), 1);
}

#[test]
fn real_string_hashcode_matches_java() {
    require_javac!();
    require_javabase!(jmod);
    let mut reg = compile_and_load(SOURCE, "StringGate");
    load_real_string(&mut reg, &jmod);
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "StringGate", "abcHashCode", "()I")), ABC_HASH);
}
