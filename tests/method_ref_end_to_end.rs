//! 集成闸门(Layer 4.10ab):**实例方法引用**(`obj::method` / `Type::method`)端到端。
//!
//! 承 4.10aa(lambda 体 / 静态方法引用):实例方法引用的引导方法同为
//! `LambdaMetafactory.metafactory`,但实现方法句柄的 reference_kind 为
//! `REF_invokeVirtual`(5)/`special`(7)/`interface`(9)——接收者隐含。此前
//! `dispatch_lambda` 仅派发 `REF_invokeStatic`,实例引用一律「句柄种类未支持」。
//! 本层:接收者 = 捕获或 SAM 首参,按其运行时类虚分派(尊重覆写)后经 `run_callee` 执行。
//!
//! javac 对**绑定**实例方法引用(`b::get`)在 invokedynamic 前插入
//! `invokestatic java/util/Objects.requireNonNull` 空检 → 须载入真 `Objects`(经 java.base.jmod,
//! 同 4.10y/z 的真 java.base 路径)。本地类 `Box` + 本地函数式接口隔离 lambda 机制。
//! 需 PATH 中 `javac` + 本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java")
            .join(ver)
            .join("jmods/java.base.jmod");
        if p.exists() {
            return Some(p);
        }
    }
    std::env::var("JAVA_HOME")
        .ok()
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-mref-{n}-{public_name}-{}", std::process::id()));
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
    dir
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

/// 执行无参静态方法。`invokedynamic` 须方法身份 → `with_identity`。
fn run(vm: &mut VmThread, class: &str, name: &str, desc: &str) -> Value {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表"));
    let lc = reg.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool).with_identity(class, name);
    match interp.interpret_with(&mut frame, vm) {
        Ok(v) => v,
        Err(VmError::ThrownException(r)) => {
            let cn = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("{o:?}"),
            };
            panic!("{class}.{name}{desc} 抛出 {cn}");
        }
        Err(e) => panic!("{class}.{name}{desc} 执行失败:{e}"),
    }
}

const SOURCE: &str = r#"
class Box {
    int v;
    Box(int v) { this.v = v; }
    int get() { return v; }
    int plus(int n) { return v + n; }
}
interface BoxToInt { int apply(Box b); }
interface BoxIntToInt { int apply(Box b, int n); }
interface IntSupplier { int get(); }
interface BoxFactory { Box make(int v); }
public class MethodRefGate {
    // 无绑定实例方法引用:Box::get → 接收者来自 SAM 首参。
    public static int unbound() {
        BoxToInt f = Box::get;
        return f.apply(new Box(42));
    }
    // 带参数实例方法引用:Box::plus。
    public static int unboundArg() {
        BoxIntToInt f = Box::plus;
        return f.apply(new Box(10), 5);
    }
    // 绑定实例方法引用:b::get → 接收者 b 在 factoryType 捕获(javac 前插 Objects.requireNonNull)。
    public static int bound() {
        Box b = new Box(7);
        IntSupplier f = b::get;
        return f.get();
    }
    // 构造器引用:Box::new → 分配 + <init>(combined) + 返新实例。
    public static int ctor() {
        BoxFactory f = Box::new;
        return f.make(99).v;
    }
}
"#;

/// **集成闸门**:实例方法引用真字节码端到端(无绑定 / 带参 / 绑定)。
#[test]
fn instance_method_reference_real_bytecode() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_dir(SOURCE, "MethodRefGate");
    let mut registry = ClassRegistry::new();
    // 本地类(Box / 接口 / MethodRefGate)。
    for cls in ["MethodRefGate", "Box", "BoxToInt", "BoxIntToInt", "IntSupplier", "BoxFactory"] {
        registry
            .load(parse(&std::fs::read(dir.join(format!("{cls}.class"))).unwrap()).unwrap())
            .unwrap();
    }
    // 真 `java/util/Objects`(绑定引用的 requireNonNull 空检经此)+ 其引用闭包。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/util/Objects").unwrap();

    let mut vm = VmThread::new(registry);
    assert_eq!(run(&mut vm, "MethodRefGate", "unbound", "()I"), Value::Int(42), "Box::get apply(new Box(42))");
    assert_eq!(run(&mut vm, "MethodRefGate", "unboundArg", "()I"), Value::Int(15), "Box::plus apply(new Box(10),5)");
    assert_eq!(run(&mut vm, "MethodRefGate", "bound", "()I"), Value::Int(7), "b::get(b=new Box(7))");
    assert_eq!(run(&mut vm, "MethodRefGate", "ctor", "()I"), Value::Int(99), "Box::new make(99).v");
}
