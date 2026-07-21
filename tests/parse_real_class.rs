//! 集成测试:用真正的 `javac` 编译一个 Java 源文件,再用 rustj 解析其 `.class`,
//! 断言解析出的结构与字节码。需要 PATH 中有 `javac`(无则跳过)。

use rustj::classfile::parse;
use rustj::testkit::*;

#[test]
fn parses_javac_compiled_class() {
    require_javac!();

    let source = r#"
public class HelloRust {
    public static int add(int a, int b) {
        return a + b;
    }
    public static void main(String[] args) {
        System.out.println(add(2, 3));
    }
}
"#;
    let class_path = compile(source, "HelloRust");
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");

    assert_eq!(cf.this_class_name(), Some("HelloRust"));
    assert_eq!(cf.super_class_name(), Some("java/lang/Object"));
    // modern javac 使用较新的 major version,只断言合理下界
    assert!(cf.major_version >= 52);

    // add(int,int):int —— 字节码应以 ireturn (0xAC) 结尾
    let add = find_method(&cf, "add", "(II)I");
    let code = add.code.as_ref().expect("add 应有 Code");
    assert!(code.max_locals >= 2, "add 至少需要 2 个局部变量");
    assert!(!code.code.is_empty());
    assert_eq!(*code.code.last().unwrap(), 0xAC, "add 应以 ireturn 结尾");

    // main([Ljava/lang/String;)V
    let main = find_method(&cf, "main", "([Ljava/lang/String;)V");
    assert!(main.code.is_some(), "main 应有 Code");
}

#[test]
fn parses_a_field() {
    require_javac!();

    let source = r#"
public class WithField {
    public static final int ANSWER = 42;
    public static String greeting = "hello";
}
"#;
    let class_path = compile(source, "WithField");
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");

    assert_eq!(cf.fields.len(), 2);
    let names: Vec<String> = cf
        .fields
        .iter()
        .map(|f| utf8(&cf, f.name_index))
        .collect();
    assert!(names.contains(&"ANSWER".to_string()));
    assert!(names.contains(&"greeting".to_string()));
}
