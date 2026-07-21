//! 集成闸门(Layer 4.4):javac 编译含 `== null`、引用相等、`switch(int)` 的真实 Java,
//! 解析 `.class` 由 rustj 执行,验证 ifnull/if_acmp*/tableswitch/lookupswitch 与 JVM 一致。
//!
//! 这是"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//!
//! 方法均为无参 static、内部自带数据,使 javac 仍编出目标指令并返回可断言的 int。

use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class ControlFlow {
    // ifnull:new int[]{1,2,3} 非 null → 返回 a[0] = 1
    public static int nullCheck() {
        int[] a = new int[] { 1, 2, 3 };
        if (a == null) return 0;
        return a[0];
    }
    // if_acmpeq:同一引用比较 → 1
    public static int sameRef() {
        int[] a = new int[] { 5 };
        int[] b = a;
        if (a == b) return 1;
        return 0;
    }
    // tableswitch:密集,x=2 → 102
    public static int denseSwitch() {
        int x = 2;
        switch (x) {
            case 0: return 100;
            case 1: return 101;
            case 2: return 102;
            default: return -1;
        }
    }
    // lookupswitch:稀疏,x=100 → 2
    public static int sparseSwitch() {
        int x = 100;
        switch (x) {
            case 10: return 1;
            case 100: return 2;
            case 1000: return 3;
            default: return -1;
        }
    }
}
"#;

#[test]
fn ifnull_returns_element_when_nonnull() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "nullCheck", "()I"), Value::Int(1));
}

#[test]
fn if_acmpeq_same_reference() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "sameRef", "()I"), Value::Int(1));
}

#[test]
fn dense_switch_hits_case_2() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "denseSwitch", "()I"), Value::Int(102));
}

#[test]
fn sparse_switch_hits_case_100() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ControlFlow");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "ControlFlow", "sparseSwitch", "()I"), Value::Int(2));
}
