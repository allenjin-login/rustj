//! 集成测试(执行闸门):用 `javac` 编译含**接口 + default 方法 + 私有方法 + super 调用**
//! 的真实 Java 层次,解析其 `.class`,再用 rustj 解释器真正执行,验证 invokeinterface
//! 虚分派 / default 方法 / invokespecial 私有与 super / StackOverflowError 与 JVM 一致。
//!
//! 这是 Layer 4.2b 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{DEFAULT_STACK_LIMIT, Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static COMPILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load_all(source: &str, public_name: &str) -> ClassRegistry {
    let seq = COMPILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("rustj-iface-{}-{seq}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let output = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        output.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut registry = ClassRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let bytes = std::fs::read(&path).unwrap();
            let cf = parse(&bytes).expect("解析应成功");
            registry.load(cf).expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    registry
}

fn utf8(cf: &ClassFile, index: u16) -> String {
    match cf.constant_pool.get(index).unwrap() {
        ConstantPoolEntry::Utf8(s) => s.clone(),
        e => panic!("expected Utf8 at {index}, got {e:?}"),
    }
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| utf8(cf, m.name_index) == name && utf8(cf, m.descriptor_index) == desc)
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 以给定深度上限执行 static 方法,返回结果。
fn run_with_limit(
    registry: &std::sync::Arc<ClassRegistry>,
    class_name: &str,
    name: &str,
    desc: &str,
    stack_limit: u32,
) -> Result<Value, VmError> {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(std::sync::Arc::clone(registry)).with_stack_limit(stack_limit);
    interp.interpret_with(&mut frame, &mut vm)
}

/// 执行方法(给定深度上限),断言其抛出运行时异常(统一为 `ThrownException`),返回异常类名。
fn run_thrown_class_with_limit(
    registry: &std::sync::Arc<ClassRegistry>,
    class_name: &str,
    name: &str,
    desc: &str,
    stack_limit: u32,
) -> String {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(std::sync::Arc::clone(registry)).with_stack_limit(stack_limit);
    let err = interp
        .interpret_with(&mut frame, &mut vm)
        .expect_err("期望抛出异常");
    let VmError::ThrownException(exc) = err else {
        panic!("应抛 ThrownException, 得 {err:?}")
    };
    match vm.heap().get(exc) {
        Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
        other => panic!("异常应为实例对象, 得 {other:?}"),
    }
}

fn run(registry: &std::sync::Arc<ClassRegistry>, class_name: &str, name: &str, desc: &str) -> Value {
    run_with_limit(registry, class_name, name, desc, DEFAULT_STACK_LIMIT)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
interface Shape {
    int kind();
    default int tag() { return kind() * 100 + 1; }
}
class Circle implements Shape {
    public int kind() { return 2; }
}
class Square implements Shape {
    public int kind() { return 3; }
    public int ownTag() { return tag(); }
}
class Root {
    int base() { return 10; }
}
class Mid extends Root { }
class Leaf extends Mid {
    int viaSuper() { return super.base(); }
}
public class Vm {
    // invokeinterface 多态:a.kind()=2,b.kind()=3 → 2 + 30 = 32
    public static int ifacePoly() {
        Shape a = new Circle();
        Shape b = new Square();
        return a.kind() + b.kind() * 10;
    }
    // default method:类未覆盖 tag → 落到接口默认(kind()*100+1)= 201
    public static int defaultOnIface() {
        Shape s = new Circle();
        return s.tag();
    }
    // default 经类类型调用:ownTag 内 this.tag()(javac 发 invokevirtual 或 invokeinterface,
    // 两者皆经 resolve_dispatch 走接口 default)→ 3*100+1 = 301
    public static int defaultViaClass() {
        Square s = new Square();
        return s.ownTag();
    }
    // super 调用继承方法:Mid 不声明 base → invokespecial 虚查到 Root.base = 10
    public static int superInherited() {
        Leaf l = new Leaf();
        return l.viaSuper();
    }
    // 无限递归 → StackOverflowError(深度计数,小上限快速触发)
    public static int infinite() {
        return infinite();
    }
    // null 引用 invokeinterface → NullPointerException
    public static int nullIface() {
        Shape s = null;
        return s.kind();
    }
}
"#;

#[test]
fn invokeinterface_is_polymorphic() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "ifacePoly", "()I"), Value::Int(32));
}

#[test]
fn invokeinterface_hits_default_method() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "defaultOnIface", "()I"), Value::Int(201));
}

#[test]
fn invokevirtual_falls_through_to_default() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "defaultViaClass", "()I"), Value::Int(301));
}

#[test]
fn invokespecial_super_inherited() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "superInherited", "()I"), Value::Int(10));
}

#[test]
fn infinite_recursion_is_stackoverflow() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_thrown_class_with_limit(&registry, "Vm", "infinite", "()I", 16),
        "java/lang/StackOverflowError"
    );
}

#[test]
fn invokeinterface_on_null_is_nullpointer() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_thrown_class_with_limit(&registry, "Vm", "nullIface", "()I", DEFAULT_STACK_LIMIT),
        "java/lang/NullPointerException"
    );
}
