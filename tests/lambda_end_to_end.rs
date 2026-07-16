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

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, VmThread};

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
        .join(format!("rustj-lambda-{seq}-{public_name}-{}", std::process::id()));
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
    let mut registry = ClassRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let bytes = std::fs::read(&path).unwrap();
            registry.load(parse(&bytes).expect("解析应成功")).expect("加载应成功");
        }
    }
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
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "LambdaGate");
    let registry = std::sync::Arc::new(registry);

    assert_eq!(run(&registry, "noCapture", "()I"), Value::Int(42), "x->x*2 applyAsInt(21)");
    assert_eq!(run(&registry, "capturing", "()I"), Value::Int(12), "x->x+base(base=7) apply(5)");
}
