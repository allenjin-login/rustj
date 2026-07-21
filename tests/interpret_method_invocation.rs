//! 集成测试(执行闸门):用 `javac` 编译含**方法调用**的真实 Java 类,解析其 `.class`,
//! 再用 rustj 解释器**真正执行**其中的方法,验证 `invokestatic` 的解析、实参传递、
//! 递归与跨栈帧 `*return` 与 JVM 一致。
//!
//! 这是 Layer 3.3 的"能否跑通真实字节码"判据(4.1 起改走 `ClassRegistry` + `Vm`)。
//! 需要 PATH 中有 `javac`(无则跳过)。方法刻意只用数值 + `invokestatic`(同类自调/递归/互调)。

use rustj::runtime::Value;
use rustj::testkit::*;


#[test]
fn executes_recursive_int_methods() {
    require_javac!();

    let source = r#"
public class Recursion {
    public static int factorial(int n) {
        if (n <= 1) return 1;
        return n * factorial(n - 1);
    }
    public static int fib(int n) {
        if (n < 2) return n;
        return fib(n - 1) + fib(n - 2);
    }
    public static int square(int x) { return x * x; }
    public static int sumOfSquares(int a, int b) {
        return square(a) + square(b);
    }
    public static int ackermann(int m, int n) {
        if (m == 0) return n + 1;
        if (n == 0) return ackermann(m - 1, 1);
        return ackermann(m - 1, ackermann(m, n - 1));
    }
}
"#;
    let registry = compile_and_load(source, "Recursion");
    let registry = std::sync::Arc::new(registry);

    // 递归 invokestatic(单 int 实参/返回)
    assert_eq!(run_args(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(0)]), Value::Int(1));
    assert_eq!(run_args(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(1)]), Value::Int(1));
    assert_eq!(run_args(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(5)]), Value::Int(120));
    assert_eq!(run_args(&registry, "Recursion", "factorial", "(I)I", &[Arg::I(10)]), Value::Int(3_628_800));

    // 双递归(同一方法自调两次后相加)
    assert_eq!(run_args(&registry, "Recursion", "fib", "(I)I", &[Arg::I(0)]), Value::Int(0));
    assert_eq!(run_args(&registry, "Recursion", "fib", "(I)I", &[Arg::I(1)]), Value::Int(1));
    assert_eq!(run_args(&registry, "Recursion", "fib", "(I)I", &[Arg::I(10)]), Value::Int(55));
    assert_eq!(run_args(&registry, "Recursion", "fib", "(I)I", &[Arg::I(15)]), Value::Int(610));

    // 互调:sumOfSquares 调 square(两个 int 实参)
    assert_eq!(run_args(&registry, "Recursion", "sumOfSquares", "(II)I", &[Arg::I(3), Arg::I(4)]), Value::Int(25));
    assert_eq!(run_args(&registry, "Recursion", "sumOfSquares", "(II)I", &[Arg::I(5), Arg::I(12)]), Value::Int(169));

    // 嵌套调用作为实参 + 多层递归(Ackermann:验证调用栈深度与返回值回填)
    assert_eq!(run_args(&registry, "Recursion", "ackermann", "(II)I", &[Arg::I(2), Arg::I(3)]), Value::Int(9));
    assert_eq!(run_args(&registry, "Recursion", "ackermann", "(II)I", &[Arg::I(3), Arg::I(3)]), Value::Int(61));
}

#[test]
fn executes_methods_with_mixed_numeric_args() {
    require_javac!();

    let source = r#"
public class Mixed {
    // long 基 + int 指数:验证 cat-1/cat-2 实参的槽位布局。
    public static long pow(long base, int exp) {
        if (exp <= 0) return 1;
        return base * pow(base, exp - 1);
    }
    // int + long:实参类型顺序不同,槽位布局相反。
    public static long shift(int n, long amount) {
        if (n <= 0) return amount;
        return amount + shift(n - 1, amount);
    }
    // double 实参与返回:cat-2 实参传递 + cat-2 返回值压栈。
    public static double powDouble(double base, int exp) {
        if (exp <= 0) return 1;
        return base * powDouble(base, exp - 1);
    }
    // float 实参与返回。
    public static float powFloat(float base, int exp) {
        if (exp <= 0) return 1f;
        return base * powFloat(base, exp - 1);
    }
}
"#;
    let registry = compile_and_load(source, "Mixed");
    let registry = std::sync::Arc::new(registry);

    // pow(2, 10) = 1024:long 在 slot 0-1,int 在 slot 2
    assert_eq!(run_args(&registry, "Mixed", "pow", "(JI)J", &[Arg::L(2), Arg::I(10)]), Value::Long(1024));
    assert_eq!(run_args(&registry, "Mixed", "pow", "(JI)J", &[Arg::L(3), Arg::I(5)]), Value::Long(243));
    assert_eq!(run_args(&registry, "Mixed", "pow", "(JI)J", &[Arg::L(5), Arg::I(0)]), Value::Long(1));

    // shift(3, 100) = 100 + 100 + 100 + 100 = 400:int 在 slot 0,long 在 slot 1-2
    assert_eq!(run_args(&registry, "Mixed", "shift", "(IJ)J", &[Arg::I(3), Arg::L(100)]), Value::Long(400));

    // powDouble(2.0, 10) = 1024.0:double(cat-2)实参 + cat-2 返回
    match run_args(&registry, "Mixed", "powDouble", "(DI)D", &[Arg::D(2.0), Arg::I(10)]) {
        Value::Double(v) => assert!((v - 1024.0).abs() < 1e-9),
        other => panic!("powDouble 返回非 double:{other:?}"),
    }

    // powFloat(2.0, 5) = 32.0:float(cat-1)实参 + cat-1 返回
    match run_args(&registry, "Mixed", "powFloat", "(FI)F", &[Arg::F(2.0), Arg::I(5)]) {
        Value::Float(v) => assert!((v - 32.0).abs() < 1e-5),
        other => panic!("powFloat 返回非 float:{other:?}"),
    }
}
