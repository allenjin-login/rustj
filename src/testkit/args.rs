//! 低层 runner 的实参槽位写入(JVM 调用约定:I/F=1 槽,L/D=2 槽)。

use crate::runtime::Frame;

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
