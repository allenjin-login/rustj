//! 集成闸门(Layer 4.3a):用 `javac` 编译使用各类型数组的真实 Java,解析其 `.class`,
//! 用 rustj 真正执行,验证 newarray/anewarray/arraylength/*aload/*astore 与 JVM 一致。
//!
//! 这是"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//!
//! 注意:本测试的 Java 故意避开 `== null`/强制类型转换——前者编出 `if_acmpne`/
//! `ifnull`(未实现),后者编出 `checkcast`(未实现)。引用数组改用类型化的 `int[][]`,
//! 使 `outer[0]` 天然为 `int[]`,可直接 `.length` 而无需转型。

use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class Arrays {
    // int[] 求和:1+2+3+4+5 = 15(newarray/iastore/arraylength/iaload/if_icmplt)
    public static int sumInts() {
        int[] a = new int[5];
        for (int i = 0; i < 5; i++) a[i] = i + 1;
        int s = 0;
        for (int i = 0; i < a.length; i++) s += a[i];
        return s;
    }
    // byte[] 符号扩展:存 (byte)200 读回 -56
    public static int byteRoundTrip() {
        byte[] b = new byte[1];
        b[0] = (byte) 200;
        return b[0];
    }
    // char[] 零扩展:存 (char)0xFFFF 读回 65535
    public static int charRoundTrip() {
        char[] c = new char[1];
        c[0] = (char) 0xFFFF;
        return c[0];
    }
    // long[] 求和:10 + 20 = 30(lastore/laload)
    public static long sumLongs() {
        long[] a = new long[2];
        a[0] = 10L; a[1] = 20L;
        return a[0] + a[1];
    }
    // double[] 读写:2.5 + 1.5 = 4.0(dastore/daload)
    public static double sumDoubles() {
        double[] a = new double[2];
        a[0] = 2.5; a[1] = 1.5;
        return a[0] + a[1];
    }
    // 引用数组:anewarray int[] + aastore + aaload + arraylength(用 int[][] 避开转型/==null)
    public static int refArray() {
        int[][] a = new int[2][];   // anewarray:[null,null]
        a[0] = new int[3];          // aastore
        return a[0].length;         // aaload(int[]) + arraylength -> 3
    }
    // arraylength
    public static int lengthOf() {
        int[] a = new int[7];
        return a.length;
    }
}
"#;

#[test]
fn sum_int_array() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "sumInts", "()I"), Value::Int(15));
}

#[test]
fn byte_array_sign_extension() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "byteRoundTrip", "()I"), Value::Int(-56));
}

#[test]
fn char_array_zero_extension() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(
        run(&reg, "Arrays", "charRoundTrip", "()I"),
        Value::Int(65535)
    );
}

#[test]
fn long_array_sum() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "sumLongs", "()J"), Value::Long(30));
}

#[test]
fn double_array_sum() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    match run(&reg, "Arrays", "sumDoubles", "()D") {
        Value::Double(v) => assert!((v - 4.0).abs() < 1e-9, "got {v}"),
        other => panic!("期望 double,得到 {other:?}"),
    }
}

#[test]
fn reference_array_round_trip() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "refArray", "()I"), Value::Int(3));
}

#[test]
fn array_length() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "lengthOf", "()I"), Value::Int(7));
}
