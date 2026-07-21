//! 低层 runner 的实参槽位写入(JVM 调用约定:I/F=1 槽,L/D=2 槽)。

use crate::runtime::{Frame, Value};

/// 实参类型(低层 `run_raw_value` 用)。
pub enum Arg {
    I(i32),
    L(i64),
    F(f32),
    D(f64),
}

/// 按 JVM 槽位约定把 `args` 写入 `frame` 局部变量区(I/F=1 槽,L/D=2 槽)。
pub fn set_args(frame: &mut Frame, args: &[Arg]) {
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
            Arg::F(v) => {
                frame.locals.set_float(slot, *v).unwrap();
                slot += 1;
            }
            Arg::D(v) => {
                frame.locals.set_double(slot, *v).unwrap();
                slot += 2;
            }
        }
    }
}

/// 按 JVM 槽位约定把 `Value` 实参(含 `Reference`)写入 `frame` 局部变量区
/// (Int/Float/Reference=1 槽,Long/Double=2 槽)。`Value::Void` 非合法实参 → panic。
///
/// 与 [`set_args`] 对称:前者按 `Arg`(仅原始类型),本函数按 `Value`(含对象引用)——
/// 供 [`super::runner::run_static_args`] 复用调用方 `VmThread` 时向方法传堆对象引用。
pub fn set_value_args(frame: &mut Frame, args: &[Value]) {
    let mut slot: u16 = 0;
    for v in args {
        match v {
            Value::Int(x) => {
                frame.locals.set_int(slot, *x).unwrap();
                slot += 1;
            }
            Value::Long(x) => {
                frame.locals.set_long(slot, *x).unwrap();
                slot += 2;
            }
            Value::Float(x) => {
                frame.locals.set_float(slot, *x).unwrap();
                slot += 1;
            }
            Value::Double(x) => {
                frame.locals.set_double(slot, *x).unwrap();
                slot += 2;
            }
            Value::Reference(r) => {
                frame.locals.set_reference(slot, *r).unwrap();
                slot += 1;
            }
            Value::Void => panic!("set_value_args: Void 非合法实参"),
        }
    }
}
