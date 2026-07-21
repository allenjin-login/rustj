//! 集成闸门(Layer 4.5):javac 编译返回引用的方法,由 rustj 执行,
//! 验证 areturn + Value::Reference + invoke 回填引用 与 JVM 一致。需 `javac`(无则跳过)。

use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class Areturn {
    // 返回 int[] 引用(areturn)
    public static int[] makeArray() {
        return new int[5];
    }
    // 调 makeArray(),读返回数组长度 -> 5(验证引用经 invoke 回填到调用者栈)
    public static int useArrayLength() {
        return makeArray().length;
    }
    // 返回 null 引用
    public static int[] makeNull() {
        return null;
    }
    // 调 makeNull(),判 null -> 1
    public static int checkNull() {
        int[] a = makeNull();
        if (a == null) return 1;
        return 0;
    }
}
"#;

#[test]
fn areturn_array_reference_round_trip() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Areturn");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(
        run(&reg, "Areturn", "useArrayLength", "()I"),
        Value::Int(5)
    );
}

#[test]
fn areturn_null_reference() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Areturn");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Areturn", "checkNull", "()I"), Value::Int(1));
}
