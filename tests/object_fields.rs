//! 集成测试(执行闸门):用 `javac` 编译含**对象创建与字段访问**的真实 Java 类,
//! 解析其 `.class`,再用 rustj 解释器**真正执行**,验证 `new`/`aconst_null`/
//! `getfield`/`putfield`/`getstatic`/`putstatic` 与 JVM 一致。
//!
//! 这是 Layer 4.1 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//! 源文件含多个顶层类(`Point`/`Counter`/`Holder`/`Objects`),全数加载进同一注册表,
//! 以覆盖跨类字段访问。

use rustj::runtime::Value;
use rustj::testkit::*;



const SOURCE: &str = r#"
class Point { int x; int y; long tag; }
class Counter { static int value; static long total; }
class Holder { Point ref; }
public class Objects {
    // new + putfield(int) + getfield(int)
    public static int makeAndSum(int a, int b) {
        Point p = new Point();
        p.x = a;
        p.y = b;
        return p.x + p.y;
    }
    // new + putfield/getfield(long,cat-2)
    public static long tagRoundTrip(long v) {
        Point p = new Point();
        p.tag = v;
        return p.tag;
    }
    // putstatic/getstatic(int)
    public static int staticRoundTrip(int v) {
        Counter.value = v;
        return Counter.value;
    }
    // 同一执行内多次 putstatic/getstatic 累积
    public static int staticAccumulate(int n) {
        Counter.value = 0;
        for (int i = 0; i < n; i++) {
            Counter.value = Counter.value + 1;
        }
        return Counter.value;
    }
    // putstatic/getstatic(long,cat-2)累积
    public static long staticLongAccumulate(int n) {
        Counter.total = 0L;
        for (int i = 0; i < n; i++) {
            Counter.total = Counter.total + 1L;
        }
        return Counter.total;
    }
    // 引用字段:putfield/getfield 引用 + 跨对象 getfield
    public static int viaHolder(int a) {
        Holder h = new Holder();
        Point p = new Point();
        p.x = a;
        h.ref = p;
        return h.ref.x;
    }
    // 默认初始化:new 后字段为零
    public static int defaultField() {
        Point p = new Point();
        return p.x + p.y;
    }
    // aconst_null + getfield → NullPointerException
    public static int nullField() {
        Point p = null;
        return p.x;
    }
}
"#;

#[test]
fn new_and_instance_int_fields_round_trip() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_args(&registry, "Objects", "makeAndSum", "(II)I", &[Arg::I(3), Arg::I(4)]),
        Value::Int(7)
    );
    assert_eq!(
        run_args(&registry, "Objects", "makeAndSum", "(II)I", &[Arg::I(100), Arg::I(-23)]),
        Value::Int(77)
    );
}

#[test]
fn instance_long_field_round_trip() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_args(&registry, "Objects", "tagRoundTrip", "(J)J", &[Arg::L(123_456_789_012)]),
        Value::Long(123_456_789_012)
    );
}

#[test]
fn static_int_field_round_trip_and_accumulate() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_args(&registry, "Objects", "staticRoundTrip", "(I)I", &[Arg::I(42)]),
        Value::Int(42)
    );
    assert_eq!(
        run_args(&registry, "Objects", "staticAccumulate", "(I)I", &[Arg::I(5)]),
        Value::Int(5)
    );
}

#[test]
fn static_long_field_accumulate() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_args(&registry, "Objects", "staticLongAccumulate", "(I)J", &[Arg::I(100)]),
        Value::Long(100)
    );
}

#[test]
fn reference_field_cross_object_access() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_args(&registry, "Objects", "viaHolder", "(I)I", &[Arg::I(7)]),
        Value::Int(7)
    );
}

#[test]
fn new_object_has_default_zero_fields() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_args(&registry, "Objects", "defaultField", "()I", &[]),
        Value::Int(0)
    );
}

#[test]
fn getfield_on_null_is_nullpointer() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "Objects");
    let registry = std::sync::Arc::new(registry);
    let (r, vm) = run_result(&registry, "Objects", "nullField", "()I");
    assert_throws!(r, &vm, "java/lang/NullPointerException");
}
