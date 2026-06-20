//! 栈帧:局部变量表 + 操作数栈 + 程序计数器。

use super::local_vars::LocalVars;
use super::operand_stack::OperandStack;

/// 栈帧操作错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// 操作数栈溢出(超过 `max_stack`)。
    Overflow,
    /// 操作数栈下溢。
    Underflow,
    /// 期望某类型,但栈顶/槽位类型不符。
    TypeMismatch,
    /// 局部变量索引越界。
    BadLocalIndex(u16),
}

/// 一个方法的执行栈帧(JVMS §2.6)。
///
/// 本结构只持有执行状态;方法体(`CodeAttribute`)与常量池由 Layer 3 的
/// 解释器与 `Frame` 配对持有,以保持本层与类元数据的解耦。
#[derive(Debug, Clone)]
pub struct Frame {
    pub locals: LocalVars,
    pub operands: OperandStack,
    /// 当前指令偏移(字节码位置)。
    pub pc: u16,
}

impl Frame {
    /// 按 `max_locals` / `max_stack`(槽位)构造新栈帧,`pc` 初始为 0。
    pub fn new(max_locals: u16, max_stack: u16) -> Self {
        Self {
            locals: LocalVars::new(max_locals),
            operands: OperandStack::new(max_stack),
            pc: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_frame_has_empty_stack_and_zero_pc() {
        let f = Frame::new(3, 5);
        assert_eq!(f.pc, 0);
        assert_eq!(f.operands.depth(), 0);
        assert_eq!(f.locals.len(), 3);
    }
}
