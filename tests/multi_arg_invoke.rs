//! 集成闸门(多实参 invoke 槽位顺序):证 `invokevirtual`/`invokespecial` 把多个实参
//! **按声明顺序**写入被调用者局部变量(arg0→local1、arg1→local2 …)。
//!
//! 背景:`invoke_*` 自调用者栈逆序弹实参 → 须 `reverse()` 翻回正序再写入。
//! `invoke_static` 早已翻转,但 virtual/special/interface 三路曾漏 `reverse()` →
//! 多实参实例/构造器调用的局部变量槽位整体倒置。单实参路径(equals(Object)、length()
//! 等)对此无感,故长期潜伏;直到真 `String.getBytes([BIB)V`(3 实参)以
//! `Frame(TypeMismatch)` 暴露。
//!
//! 本闸门用两个**不可交换**运算门钉死槽位顺序:错排即值错,非崩溃。
//! 需 `javac`(PATH);缺则跳过。

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

/// 编译 SOURCE → 加载 `MultiArg`(仅依赖 java/lang/Object 根类,无需 java.base.jmod)。
fn compile_and_load() -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-ma-{}-{s}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("MultiArg.java");
    std::fs::write(&src, SOURCE).unwrap();
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
    reg.load(parse(&std::fs::read(dir.join("MultiArg.class")).unwrap()).expect("解析应成功"))
        .expect("加载应成功");
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(
                cf.constant_pool.get(m.name_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == name
            );
            let d = matches!(
                cf.constant_pool.get(m.descriptor_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == desc
            );
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 运行 `MultiArg.name(desc)`(无参静态方法)。抛 Java 异常时带出类名便于诊断。
fn run(reg: &ClassRegistry, name: &str, desc: &str) -> Value {
    let lc = reg.get("MultiArg").unwrap();
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp =
        Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let mut vm = Vm::new(reg);
    match interp.interpret_with(&mut frame, &mut vm) {
        Ok(v) => v,
        Err(rustj::runtime::VmError::ThrownException(r)) => {
            use rustj::oops::Oop;
            let cls = match vm.heap().get(r) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            panic!("{name}{desc} 抛 Java 异常:{cls}")
        }
        Err(e) => panic!("{name}{desc} 执行失败:{e}"),
    }
}

fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

const SOURCE: &str = r#"
public class MultiArg {
    int x;
    int y;

    // invokespecial <init>(II):两个 int 实参 → x=a、y=b。
    // 错排(无 reverse)→ x=b、y=a。
    MultiArg(int a, int b) {
        this.x = a;
        this.y = b;
    }

    // invokevirtual combine(III):三 int 实参,不可交换编码 p*100+q*10+r。
    // 正序 (100,30,5) → 10305;错排(局部变量倒置)→ 900。
    int combine(int p, int q, int r) {
        return p * 100 + q * 10 + r;
    }

    // 构造器实参顺序门:new (100,30) → x-y = 70;错排 → -70。
    public static int ctorOrder() {
        MultiArg m = new MultiArg(100, 30);
        return m.x - m.y;
    }

    // 实例方法实参顺序门:combine(100,30,5) → 10305。
    public static int callOrder() {
        MultiArg m = new MultiArg(0, 0);
        return m.combine(100, 30, 5);
    }
}
"#;

#[test]
fn invokespecial_two_args_keep_order() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let reg = compile_and_load();
    // 正确 70;若 invokespecial 漏 reverse → ctor 把 b 写入 x、a 写入 y → -70。
    assert_eq!(as_int(run(&reg, "ctorOrder", "()I")), 70);
}

#[test]
fn invokevirtual_three_args_keep_order() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let reg = compile_and_load();
    // 正确 10305;若 invokevirtual 漏 reverse → 实参倒置入局部变量 → 900。
    assert_eq!(as_int(run(&reg, "callOrder", "()I")), 10305);
}
