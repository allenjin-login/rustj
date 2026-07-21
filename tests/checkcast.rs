//! 集成闸门(Layer 4.6):javac 编 instanceof / 强制转型的真实 Java,由 rustj 执行,
//! 验证 checkcast/instanceof 与 JVM 一致。需 `javac`(无则跳过)。

use std::sync::Arc;

use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class CheckCast {
    static class Shape {}
    static class Square extends Shape {}
    interface Drawable {}
    static class Circle extends Shape implements Drawable {}

    // instanceof 类:true
    public static boolean squareIsShape() {
        Object o = new Square();
        return o instanceof Shape;
    }
    // instanceof 接口:true(Circle implements Drawable)
    public static boolean circleIsDrawable() {
        Object o = new Circle();
        return o instanceof Drawable;
    }
    // instanceof 不匹配:false
    public static boolean squareIsCircle() {
        Object o = new Square();
        return o instanceof Circle;
    }
    // instanceof null:false
    public static boolean nullIsShape() {
        Object o = null;
        return o instanceof Shape;
    }
    // checkcast 通过:转型成功即返回 1
    public static int castOk() {
        Object o = new Square();
        Square s = (Square) o;
        return 1;
    }
    // checkcast 失败:ClassCastException
    public static int castFail() {
        Object o = new Square();
        Circle c = (Circle) o;  // Square 不能转 Circle
        return 1;
    }
}
"#;

fn bool_to_int(v: Value) -> i32 {
    as_int(v)
}

#[test]
fn instanceof_class_match() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "CheckCast");
    let reg = Arc::new(reg);
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "squareIsShape", "()Z")), 1);
}

#[test]
fn instanceof_interface_match() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "CheckCast");
    let reg = Arc::new(reg);
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "circleIsDrawable", "()Z")), 1);
}

#[test]
fn instanceof_no_match() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "CheckCast");
    let reg = Arc::new(reg);
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "squareIsCircle", "()Z")), 0);
}

#[test]
fn instanceof_null_is_zero() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "CheckCast");
    let reg = Arc::new(reg);
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "nullIsShape", "()Z")), 0);
}

#[test]
fn checkcast_passes() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "CheckCast");
    let reg = Arc::new(reg);
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "castOk", "()I")), 1);
}

#[test]
fn checkcast_fails_with_classcastexception() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "CheckCast");
    let reg = Arc::new(reg);
    let (result, mut vm) = run_result(&reg, "CheckCast", "castFail", "()I");
    assert_throws!(result, &mut vm, "java/lang/ClassCastException");
}
