//! 集成闸门(Layer 4.10m):真 `java.lang.String` 端到端后的**字符串拼接**闸门。
//!
//! `s + s`(s 为非常量局部变量 → javac 编 `StringBuilder.append/toString`,预 JDK9 风格,
//! 经 `-XDstringConcat=inline` 强制)。"abc"+"abc" = "abcabc" → length() = 6。
//! 串联 4.10l(`System.arraycopy`)与 4.10m(`Float`/`Double` 位转换 native,解锁
//! `Math.<clinit>` → `Arrays.copyOfRange` → `String.<init>`)。曾为研究探针(逐缺口下移),
//! 现 **String concat 链全绿** → 转为回归闸门:任何抛出即拼接链回归。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class StringConcat {
    // s 非常量(局部变量)→ javac 编为 StringBuilder;s+s = "abcabc" → 6。
    public static int selfConcatLength() {
        String s = "abc";
        return (s + s).length();
    }
}
"#;

#[test]
fn string_concat_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    // 强制 StringBuilder(预 JDK9 风格),避开 invokedynamic。
    let dir = compile_dir(SOURCE, "StringConcat", &["-XDstringConcat=inline"]);

    let mut registry = ClassRegistry::new();
    let cf =
        rustj::classfile::parse(&std::fs::read(dir.join("StringConcat.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // 预载拼接链核心类(load_closure 拉入引用闭包)。
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/StringBuilder").unwrap();

    let registry = std::sync::Arc::new(registry);

    // 跑 selfConcatLength:"abc"+"abc" → "abcabc" → length 6。
    // 拼接链已全绿;任何抛出即回归(run 失败 panic,见 4.10l arraycopy / 4.10m Float/Double 位转换)。
    assert_eq!(
        run(&registry, "StringConcat", "selfConcatLength", "()I"),
        Value::Int(6),
        "\"abc\"+\"abc\".length() 应为 6"
    );
}