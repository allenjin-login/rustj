//! 集成测试(执行闸门):用 `javac` 编译真实 Java 类,解析其 `.class`,
//! 再用 rustj 解释器**真正执行**其中的 `static int` 方法,断言结果与 JVM 一致。
//!
//! 这是 Layer 3.1 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//! 方法刻意只用 int 子集(算术 / iload-istore / iinc / if* / if_icmp* / goto / ireturn)。

use rustj::classfile::parse;
use rustj::runtime::Value;
use rustj::testkit::*;

#[test]
fn executes_real_static_int_methods() {
    require_javac!();

    let source = r#"
public class IntMath {
    public static int add(int a, int b) {
        return a + b;
    }
    public static int factorial(int n) {
        int r = 1;
        for (int i = 2; i <= n; i++) r *= i;
        return r;
    }
    public static int fib(int n) {
        if (n < 2) return n;
        int a = 0, b = 1;
        for (int i = 2; i <= n; i++) {
            int t = a + b;
            a = b;
            b = t;
        }
        return b;
    }
    public static int gcd(int a, int b) {
        while (b != 0) {
            int t = b;
            b = a % b;
            a = t;
        }
        return a;
    }
}
"#;
    let class_path = compile(source, "IntMath");
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");

    // add
    assert_eq!(run_raw_int(&cf, "add", "(II)I", &[3, 4]), 7);
    assert_eq!(run_raw_int(&cf, "add", "(II)I", &[-5, 10]), 5);

    // factorial
    assert_eq!(run_raw_int(&cf, "factorial", "(I)I", &[0]), 1);
    assert_eq!(run_raw_int(&cf, "factorial", "(I)I", &[1]), 1);
    assert_eq!(run_raw_int(&cf, "factorial", "(I)I", &[5]), 120);
    assert_eq!(run_raw_int(&cf, "factorial", "(I)I", &[6]), 720);

    // fib
    assert_eq!(run_raw_int(&cf, "fib", "(I)I", &[0]), 0);
    assert_eq!(run_raw_int(&cf, "fib", "(I)I", &[1]), 1);
    assert_eq!(run_raw_int(&cf, "fib", "(I)I", &[10]), 55);
    assert_eq!(run_raw_int(&cf, "fib", "(I)I", &[20]), 6765);

    // gcd
    assert_eq!(run_raw_int(&cf, "gcd", "(II)I", &[48, 36]), 12);
    assert_eq!(run_raw_int(&cf, "gcd", "(II)I", &[17, 5]), 1);
    assert_eq!(run_raw_int(&cf, "gcd", "(II)I", &[100, 0]), 100);
}

#[test]
fn executes_real_numeric_methods() {
    require_javac!();

    let source = r#"
public class NumMath {
    public static long longAdd(long a, long b) {
        return a + b;
    }
    public static long factorialLong(int n) {
        long r = 1;
        for (long i = 2; i <= n; i++) r *= i;
        return r;
    }
    public static double avg(int a, int b) {
        return (a + b) / 2.0;
    }
    public static double distanceSquared(double dx, double dy) {
        return dx * dx + dy * dy;
    }
    public static float sumFloat(float a, float b) {
        return a + b;
    }
}
"#;
    let class_path = compile(source, "NumMath");
    let bytes = std::fs::read(&class_path).unwrap();
    let cf = parse(&bytes).expect("解析应成功");

    // longAdd:超过 int 范围,证明是 long
    assert_eq!(
        run_raw_value(&cf, "longAdd", "(JJ)J", &[Arg::L(1_000_000_000), Arg::L(2_000_000_000)]),
        Value::Long(3_000_000_000)
    );

    // factorialLong(20) = 2432902008176640000(long 累乘 + i2l 比较)
    assert_eq!(
        run_raw_value(&cf, "factorialLong", "(I)J", &[Arg::I(20)]),
        Value::Long(2_432_902_008_176_640_000)
    );

    // avg(3, 4) = 3.5(int 算术 + i2d 提升 + ddiv)
    match run_raw_value(&cf, "avg", "(II)D", &[Arg::I(3), Arg::I(4)]) {
        Value::Double(v) => assert!((v - 3.5).abs() < 1e-9),
        other => panic!("avg 返回非 double:{other:?}"),
    }

    // distanceSquared(3.0, 4.0) = 25.0
    match run_raw_value(&cf, "distanceSquared", "(DD)D", &[Arg::D(3.0), Arg::D(4.0)]) {
        Value::Double(v) => assert!((v - 25.0).abs() < 1e-9),
        other => panic!("distanceSquared 返回非 double:{other:?}"),
    }

    // sumFloat(1.5, 2.5) = 4.0
    match run_raw_value(&cf, "sumFloat", "(FF)F", &[Arg::F(1.5), Arg::F(2.5)]) {
        Value::Float(v) => assert!((v - 4.0).abs() < 1e-6),
        other => panic!("sumFloat 返回非 float:{other:?}"),
    }
}
