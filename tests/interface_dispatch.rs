//! 集成测试(执行闸门):用 `javac` 编译含**接口 + default 方法 + 私有方法 + super 调用**
//! 的真实 Java 层次,解析其 `.class`,再用 rustj 解释器真正执行,验证 invokeinterface
//! 虚分派 / default 方法 / invokespecial 私有与 super / StackOverflowError 与 JVM 一致。
//!
//! 这是 Layer 4.2b 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。

use rustj::oops::ClassRegistry;
use rustj::runtime::{DEFAULT_STACK_LIMIT, Frame, Interpreter, Value, VmThread, VmError};
use rustj::testkit::*;

fn compile_and_load_all(source: &str, public_name: &str) -> ClassRegistry {
    let dir = compile_dir(source, public_name, &[]);
    let mut registry = ClassRegistry::new();
    load_dir(&mut registry, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    registry
}

/// 以给定深度上限执行 static 方法,返回(结果, vm)。单文件特例(栈深度探测),保留。
fn run_with_limit(
    registry: &std::sync::Arc<ClassRegistry>,
    class_name: &str,
    name: &str,
    desc: &str,
    stack_limit: u32,
) -> (Result<Value, VmError>, VmThread) {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = VmThread::new(std::sync::Arc::clone(registry)).with_stack_limit(stack_limit);
    let result = interp.interpret_with(&mut frame, &mut vm);
    (result, vm)
}

/// 执行方法(给定深度上限),断言其抛出运行时异常(统一为 `ThrownException`),返回异常类名。
fn run_thrown_class_with_limit(
    registry: &std::sync::Arc<ClassRegistry>,
    class_name: &str,
    name: &str,
    desc: &str,
    stack_limit: u32,
) -> String {
    let (result, vm) = run_with_limit(registry, class_name, name, desc, stack_limit);
    let err = result.expect_err("期望抛出异常");
    let VmError::ThrownException(exc) = err else {
        panic!("应抛 ThrownException, 得 {err:?}")
    };
    match vm.heap().get(exc) {
        Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
        other => panic!("异常应为实例对象, 得 {other:?}"),
    }
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
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "ifacePoly", "()I"), Value::Int(32));
}

#[test]
fn invokeinterface_hits_default_method() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "defaultOnIface", "()I"), Value::Int(201));
}

#[test]
fn invokevirtual_falls_through_to_default() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "defaultViaClass", "()I"), Value::Int(301));
}

#[test]
fn invokespecial_super_inherited() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(run(&registry, "Vm", "superInherited", "()I"), Value::Int(10));
}

#[test]
fn infinite_recursion_is_stackoverflow() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_thrown_class_with_limit(&registry, "Vm", "infinite", "()I", 16),
        "java/lang/StackOverflowError"
    );
}

#[test]
fn invokeinterface_on_null_is_nullpointer() {
    require_javac!();
    let registry = compile_and_load_all(SOURCE, "Vm");
    let registry = std::sync::Arc::new(registry);
    assert_eq!(
        run_thrown_class_with_limit(&registry, "Vm", "nullIface", "()I", DEFAULT_STACK_LIMIT),
        "java/lang/NullPointerException"
    );
}
