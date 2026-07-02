//! 集成闸门(Layer 4.10x):**dup/swap 栈操作指令族**经 `javac` 真字节码端到端验证。
//!
//! 这些模式(`a[0]+=v`、链式字段/数组赋值、`a[i]++`)是 javac 日常产出,此前一律
//! `UnsupportedOpcode(DupX1/Dup2/...)`——ArrayList 的 `elementData[size++]=e` 即卡在 dup_x1。
//! 本闸门用默认 javac(无任何 `-XD` 退路)编出覆盖**全部 dup 形式**的方法:
//! - `bumpArr`(`a[0]+=5`)→ **dup2**;`chainArr`(`a[0]=a[1]=7`)→ **dup_x2**;
//! - `chainIntField`(`d.f=(d.f=7)`)→ **dup_x1**;`chainLongField`(`d.f2=(d.f2=5L)`)→ **dup2_x1**;
//! - `chainLongArr`(`g[0]=g[0]=5L`)→ **dup2_x2**;`postInc`(`a[0]++`)→ dup/dup2/dup_x2。
//!
//! 需 PATH 中 `javac`(无则跳过)。`new DupGate()` 经隐式 `<init>` → `Object.<init>`(引导桩),
//! 同 `object_fields` 的 `new Point()` 路径。

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
    let dir = std::env::temp_dir()
        .join(format!("rustj-dup-{seq}-{public_name}-{}", std::process::id()));
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
fn run(registry: &ClassRegistry, name: &str, desc: &str) -> Value {
    let lc = registry.get("DupGate").expect("DupGate 须已加载");
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(registry);
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("DupGate.{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
public class DupGate {
    int f; long f2;
    // dup2:int[] 复合赋值 a[0]+=5
    public static int bumpArr() {
        int[] a = new int[1];
        a[0] = 10;
        a[0] += 5;
        return a[0];
    }
    // dup_x2:链式 int 数组赋值 a[0]=a[1]=7
    public static int chainArr() {
        int[] a = new int[2];
        a[0] = a[1] = 7;
        return a[0] + a[1];
    }
    // dup_x1:链式 int 字段赋值 d.f=(d.f=7)
    public static int chainIntField() {
        DupGate d = new DupGate();
        d.f = (d.f = 7);
        return d.f;
    }
    // dup2_x1:链式 long 字段赋值 d.f2=(d.f2=5L)
    public static long chainLongField() {
        DupGate d = new DupGate();
        d.f2 = (d.f2 = 5L);
        return d.f2;
    }
    // dup2_x2:链式 long 数组赋值 g[0]=g[0]=5L
    public static long chainLongArr() {
        long[] g = new long[1];
        g[0] = g[0] = 5L;
        return g[0];
    }
    // dup/dup2/dup_x2:数组元素后自增,返回旧值
    public static int postInc() {
        int[] a = new int[]{9};
        return a[0]++;
    }
}
"#;

/// **集成闸门**:dup/swap 族真字节码端到端(全形式)。
#[test]
fn dup_family_real_bytecode() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "DupGate");

    assert_eq!(run(&registry, "bumpArr", "()I"), Value::Int(15), "a[0]+=5");
    assert_eq!(run(&registry, "chainArr", "()I"), Value::Int(14), "a[0]=a[1]=7");
    assert_eq!(run(&registry, "chainIntField", "()I"), Value::Int(7), "d.f=(d.f=7)");
    assert_eq!(run(&registry, "chainLongField", "()J"), Value::Long(5), "d.f2=(d.f2=5L)");
    assert_eq!(run(&registry, "chainLongArr", "()J"), Value::Long(5), "g[0]=g[0]=5L");
    assert_eq!(run(&registry, "postInc", "()I"), Value::Int(9), "a[0]++ 返回旧值");
}
