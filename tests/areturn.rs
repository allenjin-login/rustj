//! 集成闸门(Layer 4.5):javac 编译返回引用的方法,由 rustj 执行,
//! 验证 areturn + Value::Reference + invoke 回填引用 与 JVM 一致。需 `javac`(无则跳过)。

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

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-aret-{}-{s}-{public_name}",
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
    let mut vm = VmThread::new(std::sync::Arc::clone(reg));
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
public class Areturn {
    // 返回 int[] 引用(areturn)
    public static int[] makeArray() {
        return new int[5];
    }
    // 调 makeArray(),读返回数组长度 -> 5(验证引用经 invoke 回填到调用者栈)
    public static int useArrayLength() {
        return makeArray().length;
    }
    // 返回 null 引用
    public static int[] makeNull() {
        return null;
    }
    // 调 makeNull(),判 null -> 1
    public static int checkNull() {
        int[] a = makeNull();
        if (a == null) return 1;
        return 0;
    }
}
"#;

#[test]
fn areturn_array_reference_round_trip() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "Areturn");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(
        run(&reg, "Areturn", "useArrayLength", "()I"),
        Value::Int(5)
    );
}

#[test]
fn areturn_null_reference() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "Areturn");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(run(&reg, "Areturn", "checkNull", "()I"), Value::Int(1));
}
