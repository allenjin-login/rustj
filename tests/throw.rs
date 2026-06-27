//! 集成闸门(Layer 4.7):javac 编 try/catch/finally 的真实 Java,由 rustj 执行,
//! 验证 `athrow` + 异常表分派 + 跨帧(invoke)异常传播与 JVM 一致。需 `javac`(无则跳过)。
//!
//! 范围:用户 `athrow` 抛出的异常,经本帧或调用者帧异常表捕获。
//! 仅用**已加载**异常类型(BaseExc/SubExc/OtherExc)——`catch(Throwable)` 需
//! 加载 `java/lang/Throwable`(4.7b 顺延),故本闸门不涉;catch-all 语义由
//! `finally`(catch_type 0)验证。

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

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-th-{pid}-{s}-{public_name}",
        pid = std::process::id()
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

/// 运行 `class_name.name(desc)`,带其**异常表**(同帧 try/catch 与跨帧捕获的调用者表)。
fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    // 入口解释器必须带上本方法异常表:同帧 athrow 靠它找处理者;跨帧时它作为
    // 调用者表供 invoke 的 finish_invoke 扫描。
    let interp = Interpreter::new_with_exception_table(&code.code, &lc.cf.constant_pool, &code.exception_table);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

/// 运行并断言失败:捕获未处理的用户异常以 `ThrownException` 上传。
fn run_err(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> VmError {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new_with_exception_table(&code.code, &lc.cf.constant_pool, &code.exception_table);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .expect_err("期望失败")
}

const SOURCE: &str = r#"
public class ThrowGate {
    // 用 RuntimeException(非受检)派生:javac 不强制 catch/throws 声明,故本闸门可
    // 自由构造"未抛出的 catch""仅 finally 无 catch"等场景。派生类本身均**已加载**,
    // 故 is_instance 的 exact/超类型判定不受未加载根类影响(catch RuntimeException/
    // Throwable 需加载根类,留待 4.7b)。
    static class BaseExc extends RuntimeException {}
    static class SubExc extends BaseExc {}
    static class OtherExc extends RuntimeException {}

    // 1. 同帧精确捕获:抛 SubExc,catch SubExc
    public static int caughtExact() {
        try {
            throw new SubExc();
        } catch (SubExc e) {
            return 1;
        }
    }

    // 2. 同帧超类型捕获:抛 SubExc,catch BaseExc(已加载 → is_instance 命中)
    public static int caughtSuper() {
        try {
            throw new SubExc();
        } catch (BaseExc e) {
            return 2;
        }
    }

    // 3. 不匹配 catch 被跳过,后续匹配 catch 命中(表内顺序即优先级)
    public static int caughtAfterSkip() {
        try {
            throw new SubExc();
        } catch (OtherExc e) {
            return 10;   // SubExc 不是 OtherExc → 跳过
        } catch (BaseExc e) {
            return 3;    // SubExc 是 BaseExc → 命中
        }
    }

    // 4. 跨帧捕获:调用者 try/catch,被调用者 thrower() 抛出(invoke 异常传播)
    public static int caughtCrossFrame() {
        try {
            thrower();
            return 0;    // 不应到达
        } catch (SubExc e) {
            return 4;
        }
    }
    static void thrower() {
        throw new SubExc();
    }

    // 5. catch-all(catch_type 0)via finally:抛 SubExc,finally 覆盖返回
    public static int caughtFinally() {
        try {
            throw new SubExc();
        } finally {
            return 5;
        }
    }

    // 6. 未捕获:无处理者,异常向上传播出 interpret_with
    public static int uncaught() throws SubExc {
        throw new SubExc();
    }
}
"#;

fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

fn is_thrown(err: VmError) -> bool {
    matches!(err, VmError::ThrownException(_))
}

#[test]
fn caught_exact_type() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtExact", "()I")), 1);
}

#[test]
fn caught_supertype() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtSuper", "()I")), 2);
}

#[test]
fn caught_after_skipping_non_matching() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtAfterSkip", "()I")), 3);
}

#[test]
fn caught_cross_frame_via_invoke() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtCrossFrame", "()I")), 4);
}

#[test]
fn caught_finally_catch_all() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtFinally", "()I")), 5);
}

#[test]
fn uncaught_propagates_as_thrown_exception() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert!(is_thrown(run_err(&reg, "ThrowGate", "uncaught", "()I")));
}
