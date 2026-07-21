//! 方法执行辅助(集成测试用)。分两层:
//!
//! - **高层**(经 `VmThread` + `interpret_with`):走完整 VM 语义(`<clinit>`/异常表/堆)。
//!   自建或复用 `VmThread`。主流(59 文件)。
//! - **低层**(直接 `Frame` + `Interpreter::interpret`,不经 `VmThread`):只测纯指令算术,
//!   无 `<clinit>`/堆/异常表(3 文件)。低层 `_raw` 后缀 = 不经 VmThread(别与高层混用)。
//!
//! 提取自 tests/ 的 run/run_result/run_err/run_static_in(高层)与 run_static_int/
//! run_static_value(低层,此处更名 run_raw_int/run_raw_value 以避撞名)。

use std::sync::Arc;

use crate::metadata::ClassFile;
use crate::oops::ClassRegistry;
use crate::runtime::{Frame, Interpreter, Value, VmError, VmThread};

use super::args::{set_args, Arg};
use super::lookup::find_method;

// ===== 高层(经 VmThread)=====

/// 运行 `class.name(desc)`(无参静态方法),自建 `VmThread`(同 `reg`),返回 `(结果, vm)`。
/// `vm` 供调用方读堆上异常对象(如 clinit.rs 的 assert_throws_class)。
pub fn run_result(
    reg: &Arc<ClassRegistry>,
    class: &str,
    name: &str,
    desc: &str,
) -> (Result<Value, VmError>, VmThread) {
    let lc = reg
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    let mut vm = VmThread::new(Arc::clone(reg));
    let result = interp.interpret_with(&mut frame, &mut vm);
    (result, vm)
}

/// 同 [`run_result`] 但异常 panic,只返 `Value`。
pub fn run(reg: &Arc<ClassRegistry>, class: &str, name: &str, desc: &str) -> Value {
    let (result, _vm) = run_result(reg, class, name, desc);
    result.unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

/// 运行 `class.name(desc)`(静态方法),按 `args` 写 locals(经 [`set_args`]),自建 `VmThread`,
/// 异常 panic,返 `Value`。**带参版** [`run`]:用于需向方法传实参的高层测试(如构造对象后调法、
/// 多实参 invoke)。提取自 interpret_method_invocation/object_fields 各自重复的 `run(.., args)`。
pub fn run_args(
    reg: &Arc<ClassRegistry>,
    class: &str,
    name: &str,
    desc: &str,
    args: &[Arg],
) -> Value {
    let lc = reg
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    set_args(&mut frame, args);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    let mut vm = VmThread::new(Arc::clone(reg));
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{class}.{name}{desc} 执行失败:{e}"))
}

/// 运行并**期望失败**,返回 `VmError`(如 `ThrownException`)。
pub fn run_err(reg: &Arc<ClassRegistry>, class: &str, name: &str, desc: &str) -> VmError {
    let (result, _vm) = run_result(reg, class, name, desc);
    result.expect_err("期望失败")
}

/// 运行 `class.name(desc)`(无参静态方法),**复用调用方 `VmThread`**(同堆约束)。
///
/// **关键约束**:静态字段值是 Vm 堆句柄,堆随 Vm 析构失效。故引导(写 savedProps)与
/// 用户代码(读 savedProps)必须同一 Vm——对应真实 JVM 单一全局堆约定(见 real_integer.rs)。
///
/// `vm.registry()` 返回 `Option<Arc<ClassRegistry>>`,故取出 `lc` 后仍可 `&mut vm`
/// 跑 interpret_with。
pub fn run_static_in(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
) -> Result<Value, VmError> {
    let reg = vm
        .registry()
        .unwrap_or_else(|| panic!("类注册表缺失"));
    let lc = reg
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

/// 运行 `class.name()`(无参、返回 int 的静态方法,desc `()I`),**复用调用方 `VmThread`**,
/// 返回 int。委托 [`run_static_in`] 并解 `Value::Int`。异常以 `VmError` 透传(`?`)。
///
/// 提取自 12 个测试文件各自重复的 `run_static_int(vm, class, name)`(它们 Err=异常类名串;
/// 此处统一 VmError——调研确认无调用点依赖 Err 串形式,`assert_eq!(.., Ok(N))` 经 VmError:PartialEq 可编译)。
pub fn run_static_int(vm: &mut VmThread, class: &str, name: &str) -> Result<i32, VmError> {
    match run_static_in(vm, class, name, "()I")? {
        Value::Int(n) => Ok(n),
        _other => Err(VmError::BadConstant("run_static_int 期望 int 返回")),
    }
}

// ===== 低层(不经 VmThread;纯指令算术)=====

/// 执行静态 int 方法 `name{desc}`,实参按顺序写入 local 0..,返回 int。
/// 用 `Interpreter::interpret`(无 vm/异常表);仅供纯指令算术测试。
pub fn run_raw_int(cf: &ClassFile, name: &str, desc: &str, args: &[i32]) -> i32 {
    let method = find_method(cf, name, desc);
    let code = method
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    for (i, &arg) in args.iter().enumerate() {
        frame.locals.set_int(i as u16, arg).unwrap();
    }
    let interp = Interpreter::new(&code.code, &cf.constant_pool);
    match interp.interpret(&mut frame) {
        Ok(Value::Int(v)) => v,
        Ok(other) => panic!("{name} 返回非 int:{other:?}"),
        Err(e) => panic!("{name} 执行失败:{e}"),
    }
}

/// 执行静态方法,按 `Arg` 槽位约定写 local,返回 `Value`。用 `Interpreter::interpret`。
pub fn run_raw_value(cf: &ClassFile, name: &str, desc: &str, args: &[Arg]) -> Value {
    let method = find_method(cf, name, desc);
    let code = method
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    set_args(&mut frame, args);
    let interp = Interpreter::new(&code.code, &cf.constant_pool);
    interp
        .interpret(&mut frame)
        .unwrap_or_else(|e| panic!("{name} 执行失败:{e}"))
}
