//! 集成闸门(Layer 4.3b):javac 编译多维数组分配的真实 Java,解析 `.class` 由 rustj 执行,
//! 验证 multianewarray(完全分配 / 部分分配)与 JVM 一致。
//!
//! 这是"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm};

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
        "rustj-mna-{}-{s}-{public_name}",
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

fn run(reg: &std::sync::Arc<ClassRegistry>, class_name: &str, name: &str, desc: &str) -> Value {
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
    let mut vm = Vm::new(std::sync::Arc::clone(reg));
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
public class MultiArray {
    // 完全分配 int[2][3]:写 a[0][1]=7, a[1][2]=9, 返回和 16
    public static int fullAlloc() {
        int[][] a = new int[2][3];
        a[0][1] = 7;
        a[1][2] = 9;
        return a[0][1] + a[1][2];
    }
    // 各维长度:int[2][3] -> a.length * a[0].length = 6
    public static int lengths() {
        int[][] a = new int[2][3];
        return a.length * a[0].length;
    }
    // 部分分配 int[2][]:a[0] == null -> 1
    public static int partialIsNull() {
        int[][] a = new int[2][];
        if (a[0] == null) return 1;
        return 0;
    }
    // 三维部分 int[2][3][]:a[0][0] == null -> 1
    public static int threeDimPartial() {
        int[][][] a = new int[2][3][];
        if (a[0][0] == null) return 1;
        return 0;
    }
}
"#;

#[test]
fn full_allocation_write_and_read() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "MultiArray", "fullAlloc", "()I"), Value::Int(16));
}

#[test]
fn multi_dimension_lengths() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "MultiArray", "lengths", "()I"), Value::Int(6));
}

#[test]
fn partial_dimension_is_null() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "MultiArray", "partialIsNull", "()I"), Value::Int(1));
}

#[test]
fn three_dim_partial_inner_null() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "MultiArray");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(
        run(&reg, "MultiArray", "threeDimPartial", "()I"),
        Value::Int(1)
    );
}
