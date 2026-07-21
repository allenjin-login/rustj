//! 集成闸门(Layer 4.3b):javac 编译多维数组分配的真实 Java,解析 `.class` 由 rustj 执行,
//! 验证 multianewarray(完全分配 / 部分分配)与 JVM 一致。
//!
//! 这是"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。

use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class MultiArray {
    // 完全分配 int[2][3]:写 a[0][1]=7, a[1][2]=9, 返回和 16
    public static int fullAlloc() {
        int[][] a = new int[2][3];
        a[0][1] = 7;
        a[1][2] = 9;
        return a[0][1] + a[1][2];
    }
    // 各维长度:int[2][3] -> a.length * a[0].length = 6
    public static int lengths() {
        int[][] a = new int[2][3];
        return a.length * a[0].length;
    }
    // 部分分配 int[2][]:a[0] == null -> 1
    public static int partialIsNull() {
        int[][] a = new int[2][];
        if (a[0] == null) return 1;
        return 0;
    }
    // 三维部分 int[2][3][]:a[0][0] == null -> 1
    public static int threeDimPartial() {
        int[][][] a = new int[2][3][];
        if (a[0][0] == null) return 1;
        return 0;
    }
}
"#;

#[test]
fn full_allocation_write_and_read() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "MultiArray", "fullAlloc", "()I"), Value::Int(16));
}

#[test]
fn multi_dimension_lengths() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "MultiArray", "lengths", "()I"), Value::Int(6));
}

#[test]
fn partial_dimension_is_null() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "MultiArray", "partialIsNull", "()I"), Value::Int(1));
}

#[test]
fn three_dim_partial_inner_null() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(
        run(&reg, "MultiArray", "threeDimPartial", "()I"),
        Value::Int(1)
    );
}
