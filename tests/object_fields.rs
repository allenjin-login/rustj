//! 集成测试(执行闸门):用 `javac` 编译含**对象创建与字段访问**的真实 Java 类,
//! 解析其 `.class`,再用 rustj 解释器**真正执行**,验证 `new`/`aconst_null`/
//! `getfield`/`putfield`/`getstatic`/`putstatic` 与 JVM 一致。
//!
//! 这是 Layer 4.1 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//! 源文件含多个顶层类(`Point`/`Counter`/`Holder`/`Objects`),全数加载进同一注册表,
//! 以覆盖跨类字段访问。

use std::path::PathBuf;
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

/// 全局计数器:为每次编译分配唯一临时目录,避免并行测试争用同一目录。
static COMPILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 编译源文件到临时目录,返回该目录(含全部生成的 `.class`)。
fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    let seq = COMPILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rustj-objects-{}-{seq}-{public_name}", std::process::id()));
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

    dir
}

/// 编译 + 加载目录下**所有** `.class` 进同一注册表(覆盖跨类字段访问)。
fn compile_and_load_all(source: &str, public_name: &str) -> ClassRegistry {
    let dir = compile_dir(source, public_name);
    let mut registry = ClassRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let bytes = std::fs::read(&path).unwrap();
            let cf = parse(&bytes).expect("解析应成功");
            registry.load(cf).expect("加载应成功");
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

/// 实参:按 JVM 调用约定(long/double 占两槽)写入局部变量。
enum Arg {
    I(i32),
    L(i64),
}

/// 执行静态方法,返回结果值(失败则 panic)。
fn run(registry: &ClassRegistry, class_name: &str, name: &str, desc: &str, args: &[Arg]) -> Value {
    run_result(registry, class_name, name, desc, args).unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

/// 执行静态方法,返回 `Result`(供断言异常路径)。
fn run_result(
    registry: &ClassRegistry,
    class_name: &str,
    name: &str,
    desc: &str,
    args: &[Arg],
) -> Result<Value, VmError> {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));

    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let mut slot: u16 = 0;
    for a in args {
        match a {
            Arg::I(v) => {
                frame.locals.set_int(slot, *v).unwrap();
                slot += 1;
            }
            Arg::L(v) => {
                frame.locals.set_long(slot, *v).unwrap();
                slot += 2;
            }
        }
    }

    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(registry);
    interp.interpret_with(&mut frame, &mut vm)
}

/// 执行方法,断言其抛出运行时异常(统一为 `ThrownException`),返回异常对象的类内部名。
fn run_thrown_class(registry: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> String {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(registry);
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
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run(&registry, "Objects", "makeAndSum", "(II)I", &[Arg::I(3), Arg::I(4)]),
        Value::Int(7)
    );
    assert_eq!(
        run(&registry, "Objects", "makeAndSum", "(II)I", &[Arg::I(100), Arg::I(-23)]),
        Value::Int(77)
    );
}

#[test]
fn instance_long_field_round_trip() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run(&registry, "Objects", "tagRoundTrip", "(J)J", &[Arg::L(123_456_789_012)]),
        Value::Long(123_456_789_012)
    );
}

#[test]
fn static_int_field_round_trip_and_accumulate() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run(&registry, "Objects", "staticRoundTrip", "(I)I", &[Arg::I(42)]),
        Value::Int(42)
    );
    assert_eq!(
        run(&registry, "Objects", "staticAccumulate", "(I)I", &[Arg::I(5)]),
        Value::Int(5)
    );
}

#[test]
fn static_long_field_accumulate() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run(&registry, "Objects", "staticLongAccumulate", "(I)J", &[Arg::I(100)]),
        Value::Long(100)
    );
}

#[test]
fn reference_field_cross_object_access() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run(&registry, "Objects", "viaHolder", "(I)I", &[Arg::I(7)]),
        Value::Int(7)
    );
}

#[test]
fn new_object_has_default_zero_fields() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run(&registry, "Objects", "defaultField", "()I", &[]),
        Value::Int(0)
    );
}

#[test]
fn getfield_on_null_is_nullpointer() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Objects");
    assert_eq!(
        run_thrown_class(&registry, "Objects", "nullField", "()I"),
        "java/lang/NullPointerException"
    );
}
