//! 集成测试(执行闸门):用 `javac` 编译含**类继承 + 方法重写 + 继承字段**的真实 Java 层次,
//! 解析其 `.class`,再用 rustj 解释器**真正执行**,验证 `invokevirtual` 虚分派与继承字段
//! 扁平化与 JVM 一致。
//!
//! 这是 Layer 4.2 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//! 层次:`Shape`(id, kind/describe) ← `Square`(side, 重写 kind, area) ← `Rect`(h, 重写 kind/area)。

use std::sync::Arc;

use rustj::oops::ClassRegistry;
use rustj::runtime::Value;
use rustj::testkit::*;

fn compile_and_load_all(source: &str, public_name: &str) -> ClassRegistry {
    let dir = compile_dir(source, public_name, &[]);
    let mut registry = ClassRegistry::new();
    load_dir(&mut registry, &dir);
    registry
}

const SOURCE: &str = r#"
class Shape {
    int id;
    int kind() { return 0; }
    int describe() { return id * 10 + kind(); }
}
class Square extends Shape {
    int side;
    int kind() { return 1; }
    int area() { return side * side; }
}
class Rect extends Square {
    int h;
    int kind() { return 2; }
    int area() { return side * h; }
}
public class Vm {
    // invokevirtual 命中各自类的方法:Shape.kind=0, Square.kind=1, Rect.kind=2 → 12
    public static int polyKind() {
        Shape a = new Shape();
        Square b = new Square();
        Rect c = new Rect();
        return a.kind() * 100 + b.kind() * 10 + c.kind();
    }
    // 继承字段(id/side)+ 多层重写(kind)+ 继承方法调用(describe 虚调 kind)
    // r.id=3 → describe = id*10 + kind() = 30 + 2(Rect) = 32
    public static int inheritedFieldAndOverride() {
        Rect r = new Rect();
        r.id = 3;
        r.side = 4;
        r.h = 5;
        return r.describe();
    }
    // 多层重写:Rect.area 覆盖 Square.area,用继承字段 side + 自有 h → 6*7 = 42
    public static int multiLevelOverride() {
        Rect r = new Rect();
        r.side = 6;
        r.h = 7;
        return r.area();
    }
    // 精确类对象上的方法(无重写介入):new Square().area → Square.area = 64
    public static int exactClassNoOverride() {
        Square sq = new Square();
        sq.side = 8;
        return sq.area();
    }
    // new 子类后继承字段与自有字段全默认 0
    public static int defaultInheritedFields() {
        Rect r = new Rect();
        return r.id + r.side + r.h;
    }
    // 对 null 引用 invokevirtual → NullPointerException
    public static int nullVirtual() {
        Shape s = null;
        return s.kind();
    }
}
"#;

#[test]
fn invokevirtual_is_polymorphic() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "polyKind", "()I"), Value::Int(12));
}

#[test]
fn inherited_fields_and_multi_level_override() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = Arc::new(registry);
    assert_eq!(
        run(&registry, "Vm", "inheritedFieldAndOverride", "()I"),
        Value::Int(32)
    );
    assert_eq!(
        run(&registry, "Vm", "multiLevelOverride", "()I"),
        Value::Int(42)
    );
}

#[test]
fn exact_class_dispatch_without_override() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "exactClassNoOverride", "()I"), Value::Int(64));
}

#[test]
fn new_subclass_defaults_inherited_fields() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = Arc::new(registry);
    assert_eq!(
        run(&registry, "Vm", "defaultInheritedFields", "()I"),
        Value::Int(0)
    );
}

#[test]
fn invokevirtual_on_null_is_nullpointer() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = Arc::new(registry);
    let (result, mut vm) = run_result(&registry, "Vm", "nullVirtual", "()I");
    assert_throws!(result, &mut vm, "java/lang/NullPointerException");
}
