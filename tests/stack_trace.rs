//! 集成闸门(Java 栈轨迹捕获):证抛出的异常携带调用链,`Vm::format_trace` 可回读。
//!
//! `class Trace { deep(){1/0} mid(){deep()} top(){mid()} }` —— `top` 经两层调用到 `deep`
//! 抛 `ArithmeticException`。捕获的栈轨迹须含 `deep`/`mid`/`top`,且**抛出帧(deep)在前**
//! (Java 惯例:最内帧首)。修前 `format_trace` 返空 → 红;捕获接通后 → 绿。
//! 需 `javac`(PATH);缺则跳过。

use rustj::testkit::*;
use rustj::oops::Oop;
use rustj::runtime::{Frame, Interpreter, VmThread, VmError};

const SOURCE: &str = r#"
public class Trace {
    public static int deep() { return 1 / 0; }   // idiv 除零 → ArithmeticException
    public static int mid() { return deep(); }
    public static int top() { return mid(); }
}
"#;

/// 跑 `Trace.top` → 期望 `ThrownException`;回读 `format_trace` 验调用链。
#[test]
fn thrown_exception_carries_call_chain() {
    require_javac!();

    let reg = compile_and_load(SOURCE, "Trace");
    let reg = std::sync::Arc::new(reg);
    let lc = reg.get("Trace").unwrap();
    let m = find_method(&lc.cf, "top", "()I");
    let code = m.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), "top");
    let mut vm = VmThread::new(std::sync::Arc::clone(&reg));

    let err = interp
        .interpret_with(&mut frame, &mut vm)
        .expect_err("top() 应抛 ArithmeticException");
    let VmError::ThrownException(r) = err else {
        panic!("期望 ThrownException,得 {err:?}");
    };

    // 异常类正确。
    let cls = match vm.heap().get(r) {
        Some(Oop::Instance(i)) => i.class_name().to_string(),
        o => panic!("异常应为 Instance,得 {o:?}"),
    };
    assert_eq!(cls, "java/lang/ArithmeticException");

    // 栈轨迹含调用链;抛出帧(deep)须在 top 之前(最内帧首)。
    let trace = vm.format_trace(r);
    eprintln!("捕获栈轨迹:\n{trace}");
    let d = trace.find("deep").expect("轨迹应含 deep");
    let mi = trace.find("mid").expect("轨迹应含 mid");
    let t = trace.find("top").expect("轨迹应含 top");
    assert!(d < mi && mi < t, "调用链顺序应 deep→mid→top(最内帧首),得:\n{trace}");

    // 行号(SourceFile + LineNumberTable,默认 javac 即生成):deep 抛点 line 3、
    // mid 调用点 line 4、top 调用点 line 5。格式 `at Class.method(File.java:LINE)`
    // 镜像 HotSpot StackTraceElement。修前仅 `at Class.method`、无 `(…:LINE)` → 红。
    assert!(
        trace.contains("at Trace.deep(Trace.java:3)"),
        "deep 帧须带抛点行号,得:\n{trace}"
    );
    assert!(
        trace.contains("at Trace.mid(Trace.java:4)"),
        "mid 帧须带调用点行号,得:\n{trace}"
    );
    assert!(
        trace.contains("at Trace.top(Trace.java:5)"),
        "top 帧须带调用点行号,得:\n{trace}"
    );
}
