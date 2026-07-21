//! 集成闸门(Layer 4.10aa):**lambda / 函数式接口**经 `javac` 真字节码端到端验证。
//!
//! 所有 lambda 经 `invokedynamic <samName>(captures)samType`,引导方法为
//! `java/lang/invoke/LambdaMetafactory.metafactory`——此前一律「未支持的引导方法」。
//! 本闸门用默认 javac 编出**无捕获 + 捕获**两类 lambda:闭包 Oop(`Oop::Lambda`)记实现
//! 方法身份 + 捕获;SAM 调用(`invokeinterface`)把捕获前置 ++ SAM 实参交给实现方法体
//! (`lambda$<caller>$0`)静态执行。本地函数式接口 `IntFunc` 隔离 lambda 机制(不耦合
//! java.base 加载)。
//!
//! 需 PATH 中 `javac`(无则跳过)。

use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, VmThread};
use rustj::testkit::*;

/// 执行无参静态方法,返回结果值(失败则 panic,打印 VmError 便于定位缺口)。
/// invokedynamic 解析 BootstrapMethods 须有方法身份(声明类)→ `with_identity`。
fn run(registry: &std::sync::Arc<ClassRegistry>, name: &str, desc: &str) -> Value {
    let lc = registry.get("LambdaGate").expect("LambdaGate 须已加载");
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool).with_identity("LambdaGate", name);
    let mut vm = VmThread::new(std::sync::Arc::clone(registry));
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("LambdaGate.{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
interface IntFunc {
    int apply(int x);
}
public class LambdaGate {
    // 无捕获 lambda:x -> x*2 → invokedynamic applyAsInt()LIntFunc;(无捕获)。
    public static int noCapture() {
        IntFunc f = x -> x * 2;
        return f.apply(21);
    }
    // 捕获 lambda:捕获局部 base;x -> x + base → invokedynamic apply(I)LIntFunc;(捕获 int)。
    public static int capturing() {
        int base = 7;
        IntFunc f = x -> x + base;
        return f.apply(5);
    }
}
"#;

/// **集成闸门**:lambda / 函数式接口真字节码端到端。
#[test]
fn lambda_real_bytecode() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "LambdaGate");
    let registry = std::sync::Arc::new(registry);

    assert_eq!(run(&registry, "noCapture", "()I"), Value::Int(42), "x->x*2 applyAsInt(21)");
    assert_eq!(run(&registry, "capturing", "()I"), Value::Int(12), "x->x+base(base=7) apply(5)");
}
