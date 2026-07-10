//! 集成闸门(Layer 4.3a):用 `javac` 编译使用各类型数组的真实 Java,解析其 `.class`,
//! 用 rustj 真正执行,验证 newarray/anewarray/arraylength/*aload/*astore 与 JVM 一致。
//!
//! 这是"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。
//!
//! 注意:本测试的 Java 故意避开 `== null`/强制类型转换——前者编出 `if_acmpne`/
//! `ifnull`(未实现),后者编出 `checkcast`(未实现)。引用数组改用类型化的 `int[][]`,
//! 使 `outer[0]` 天然为 `int[]`,可直接 `.length` 而无需转型。

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

static COMPILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load_all(source: &str, public_name: &str) -> ClassRegistry {
    let seq = COMPILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-arr-{}-{seq}-{public_name}",
        std::process::id()
    ));
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

fn run(registry: &std::sync::Arc<ClassRegistry>, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(std::sync::Arc::clone(registry));
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
public class Arrays {
    // int[] 求和:1+2+3+4+5 = 15(newarray/iastore/arraylength/iaload/if_icmplt)
    public static int sumInts() {
        int[] a = new int[5];
        for (int i = 0; i < 5; i++) a[i] = i + 1;
        int s = 0;
        for (int i = 0; i < a.length; i++) s += a[i];
        return s;
    }
    // byte[] 符号扩展:存 (byte)200 读回 -56
    public static int byteRoundTrip() {
        byte[] b = new byte[1];
        b[0] = (byte) 200;
        return b[0];
    }
    // char[] 零扩展:存 (char)0xFFFF 读回 65535
    public static int charRoundTrip() {
        char[] c = new char[1];
        c[0] = (char) 0xFFFF;
        return c[0];
    }
    // long[] 求和:10 + 20 = 30(lastore/laload)
    public static long sumLongs() {
        long[] a = new long[2];
        a[0] = 10L; a[1] = 20L;
        return a[0] + a[1];
    }
    // double[] 读写:2.5 + 1.5 = 4.0(dastore/daload)
    public static double sumDoubles() {
        double[] a = new double[2];
        a[0] = 2.5; a[1] = 1.5;
        return a[0] + a[1];
    }
    // 引用数组:anewarray int[] + aastore + aaload + arraylength(用 int[][] 避开转型/==null)
    public static int refArray() {
        int[][] a = new int[2][];   // anewarray:[null,null]
        a[0] = new int[3];          // aastore
        return a[0].length;         // aaload(int[]) + arraylength -> 3
    }
    // arraylength
    public static int lengthOf() {
        int[] a = new int[7];
        return a.length;
    }
}
"#;

#[test]
fn sum_int_array() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "sumInts", "()I"), Value::Int(15));
}

#[test]
fn byte_array_sign_extension() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "byteRoundTrip", "()I"), Value::Int(-56));
}

#[test]
fn char_array_zero_extension() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(
        run(&reg, "Arrays", "charRoundTrip", "()I"),
        Value::Int(65535)
    );
}

#[test]
fn long_array_sum() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "sumLongs", "()J"), Value::Long(30));
}

#[test]
fn double_array_sum() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    match run(&reg, "Arrays", "sumDoubles", "()D") {
        Value::Double(v) => assert!((v - 4.0).abs() < 1e-9, "got {v}"),
        other => panic!("期望 double,得到 {other:?}"),
    }
}

#[test]
fn reference_array_round_trip() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "refArray", "()I"), Value::Int(3));
}

#[test]
fn array_length() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load_all(SOURCE, "Arrays");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Arrays", "lengthOf", "()I"), Value::Int(7));
}
