//! 集成闸门(Layer 4.6):javac 编 instanceof / 强制转型的真实 Java,由 rustj 执行,
//! 验证 checkcast/instanceof 与 JVM 一致。需 `javac`(无则跳过)。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-cc-{}-{s}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功"))
                .expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = match cf.constant_pool.get(m.name_index).unwrap() {
                ConstantPoolEntry::Utf8(s) => s == name,
                _ => false,
            };
            let d = match cf.constant_pool.get(m.descriptor_index).unwrap() {
                ConstantPoolEntry::Utf8(s) => s == desc,
                _ => false,
            };
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

fn run_err(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> VmError {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .expect_err("期望失败")
}

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
    match v {
        Value::Int(b) => b,
        other => panic!("期望 int,得 {other:?}"),
    }
}

#[test]
fn instanceof_class_match() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "squareIsShape", "()Z")), 1);
}

#[test]
fn instanceof_interface_match() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "circleIsDrawable", "()Z")), 1);
}

#[test]
fn instanceof_no_match() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "squareIsCircle", "()Z")), 0);
}

#[test]
fn instanceof_null_is_zero() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "nullIsShape", "()Z")), 0);
}

#[test]
fn checkcast_passes() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "castOk", "()I")), 1);
}

#[test]
fn checkcast_fails_with_classcastexception() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(
        run_err(&reg, "CheckCast", "castFail", "()I"),
        VmError::ClassCastException
    );
}
