//! 字节码解释器(对应 HotSpot `interpreter/zero/bytecodeInterpreter`)。
//!
//! 在一个栈帧上分派执行字节码,直到 `*return`。详见
//! `docs/superpowers/specs/2026-06-20-interpreter-design.md`(Layer 3)。
//!
//! 3.1:仅 int 核心子集;清单外指令返回 [`VmError::UnsupportedOpcode`]。

use crate::bytecode::opcode::{BytecodeError, Opcode};
use crate::classfile::ClassFileError;
use crate::constant_pool::entry::ConstantPoolEntry;
use crate::constant_pool::ConstantPool;

use super::frame::{Frame, FrameError};

/// 解释器执行结果值。3.1 只用 `Int` / `Void`;3.2 起补 `Long`/`Float`/`Double`/`Reference`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    Int(i32),
    Void,
}

/// 运行时错误(JVM 语义层面)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    /// ArithmeticException:int 除零。
    DivideByZero,
    /// 当前子集不支持的指令(随增量推进而收敛)。
    UnsupportedOpcode(Opcode),
    /// PC 越过字节码末尾仍未返回。
    BadPc(usize),
    /// 操作数/局部变量栈帧错误。
    Frame(FrameError),
    /// 常量池索引或类型不符。
    ConstantPool(ClassFileError),
    /// ldc 等取到非预期类型的常量。
    BadConstant(&'static str),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DivideByZero => write!(f, "ArithmeticException: / by zero"),
            Self::UnsupportedOpcode(op) => write!(f, "unsupported opcode: {} (0x{:02X})", op.name(), *op as u8),
            Self::BadPc(pc) => write!(f, "pc ran off bytecode end: {pc}"),
            Self::Frame(e) => write!(f, "frame error: {e:?}"),
            Self::ConstantPool(e) => write!(f, "constant pool: {e:?}"),
            Self::BadConstant(msg) => write!(f, "bad constant: {msg}"),
        }
    }
}
impl std::error::Error for VmError {}

impl From<FrameError> for VmError {
    fn from(e: FrameError) -> Self {
        Self::Frame(e)
    }
}
impl From<ClassFileError> for VmError {
    fn from(e: ClassFileError) -> Self {
        Self::ConstantPool(e)
    }
}
impl From<BytecodeError> for VmError {
    fn from(_e: BytecodeError) -> Self {
        Self::BadConstant("invalid opcode byte")
    }
}

/// 解释器:持有字节码与常量池的不可变借用,在给定栈帧上执行。
pub struct Interpreter<'a> {
    code: &'a [u8],
    cp: &'a ConstantPool,
}

impl<'a> Interpreter<'a> {
    pub fn new(code: &'a [u8], cp: &'a ConstantPool) -> Self {
        Self { code, cp }
    }

    /// 在 `frame` 上执行至 `*return`;返回结果值。
    pub fn interpret(&self, frame: &mut Frame) -> Result<Value, VmError> {
        let mut pc: usize = 0;
        loop {
            if pc >= self.code.len() {
                return Err(VmError::BadPc(pc));
            }
            let op = Opcode::from_u8(self.code[pc])?;
            match op {
                // ---- 常量压栈 ----
                Opcode::IconstM1 => {
                    frame.operands.push_int(-1)?;
                    pc += 1;
                }
                Opcode::Iconst0 => {
                    frame.operands.push_int(0)?;
                    pc += 1;
                }
                Opcode::Iconst1 => {
                    frame.operands.push_int(1)?;
                    pc += 1;
                }
                Opcode::Iconst2 => {
                    frame.operands.push_int(2)?;
                    pc += 1;
                }
                Opcode::Iconst3 => {
                    frame.operands.push_int(3)?;
                    pc += 1;
                }
                Opcode::Iconst4 => {
                    frame.operands.push_int(4)?;
                    pc += 1;
                }
                Opcode::Iconst5 => {
                    frame.operands.push_int(5)?;
                    pc += 1;
                }
                Opcode::Bipush => {
                    let v = self.read_s1(pc + 1)? as i32;
                    frame.operands.push_int(v)?;
                    pc += 2;
                }
                Opcode::Sipush => {
                    let v = self.read_s2(pc + 1)? as i32;
                    frame.operands.push_int(v)?;
                    pc += 3;
                }
                Opcode::Ldc => {
                    let index = self.read_u1(pc + 1)? as u16;
                    match self.cp.get(index)? {
                        ConstantPoolEntry::Integer(v) => frame.operands.push_int(*v)?,
                        _ => return Err(VmError::BadConstant("ldc 非 int 常量(3.1 仅支持 int)")),
                    }
                    pc += 2;
                }
                // ---- 加载局部变量 ----
                Opcode::Iload => {
                    let idx = self.read_u1(pc + 1)? as u16;
                    frame.operands.push_int(frame.locals.get_int(idx)?)?;
                    pc += 2;
                }
                Opcode::Iload0 => {
                    frame.operands.push_int(frame.locals.get_int(0)?)?;
                    pc += 1;
                }
                Opcode::Iload1 => {
                    frame.operands.push_int(frame.locals.get_int(1)?)?;
                    pc += 1;
                }
                Opcode::Iload2 => {
                    frame.operands.push_int(frame.locals.get_int(2)?)?;
                    pc += 1;
                }
                Opcode::Iload3 => {
                    frame.operands.push_int(frame.locals.get_int(3)?)?;
                    pc += 1;
                }
                // ---- 存入局部变量 ----
                Opcode::Istore => {
                    let idx = self.read_u1(pc + 1)? as u16;
                    let v = frame.operands.pop_int()?;
                    frame.locals.set_int(idx, v)?;
                    pc += 2;
                }
                Opcode::Istore0 => {
                    let v = frame.operands.pop_int()?;
                    frame.locals.set_int(0, v)?;
                    pc += 1;
                }
                Opcode::Istore1 => {
                    let v = frame.operands.pop_int()?;
                    frame.locals.set_int(1, v)?;
                    pc += 1;
                }
                Opcode::Istore2 => {
                    let v = frame.operands.pop_int()?;
                    frame.locals.set_int(2, v)?;
                    pc += 1;
                }
                Opcode::Istore3 => {
                    let v = frame.operands.pop_int()?;
                    frame.locals.set_int(3, v)?;
                    pc += 1;
                }
                // ---- 整数算术(补码回绕)----
                Opcode::Iadd => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l.wrapping_add(r))?;
                    pc += 1;
                }
                Opcode::Isub => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l.wrapping_sub(r))?;
                    pc += 1;
                }
                Opcode::Imul => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l.wrapping_mul(r))?;
                    pc += 1;
                }
                Opcode::Idiv => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    if r == 0 {
                        return Err(VmError::DivideByZero);
                    }
                    frame.operands.push_int(l.wrapping_div(r))?;
                    pc += 1;
                }
                Opcode::Irem => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    if r == 0 {
                        return Err(VmError::DivideByZero);
                    }
                    frame.operands.push_int(l.wrapping_rem(r))?;
                    pc += 1;
                }
                Opcode::Ineg => {
                    let v = frame.operands.pop_int()?;
                    frame.operands.push_int(v.wrapping_neg())?;
                    pc += 1;
                }
                Opcode::Iinc => {
                    let idx = self.read_u1(pc + 1)? as u16;
                    let delta = self.read_s1(pc + 2)? as i32;
                    let v = frame.locals.get_int(idx)?;
                    frame.locals.set_int(idx, v.wrapping_add(delta))?;
                    pc += 3;
                }
                Opcode::Iand => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l & r)?;
                    pc += 1;
                }
                Opcode::Ior => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l | r)?;
                    pc += 1;
                }
                Opcode::Ixor => {
                    let r = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l ^ r)?;
                    pc += 1;
                }
                Opcode::Ishl => {
                    let s = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l.wrapping_shl(s as u32))?;
                    pc += 1;
                }
                Opcode::Ishr => {
                    let s = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(l.wrapping_shr(s as u32))?;
                    pc += 1;
                }
                Opcode::Iushr => {
                    let s = frame.operands.pop_int()?;
                    let l = frame.operands.pop_int()?;
                    frame.operands.push_int(((l as u32).wrapping_shr(s as u32)) as i32)?;
                    pc += 1;
                }
                // ---- 栈操作 ----
                Opcode::Nop => {
                    pc += 1;
                }
                Opcode::Dup => {
                    let v = frame.operands.pop_slot()?;
                    frame.operands.push_slot(v)?;
                    frame.operands.push_slot(v)?;
                    pc += 1;
                }
                Opcode::Pop => {
                    frame.operands.pop_slot()?;
                    pc += 1;
                }
                // ---- 单操作数条件分支 ----
                Opcode::Ifeq => pc = self.cond1(pc, |v| v == 0, frame)?,
                Opcode::Ifne => pc = self.cond1(pc, |v| v != 0, frame)?,
                Opcode::Iflt => pc = self.cond1(pc, |v| v < 0, frame)?,
                Opcode::Ifge => pc = self.cond1(pc, |v| v >= 0, frame)?,
                Opcode::Ifgt => pc = self.cond1(pc, |v| v > 0, frame)?,
                Opcode::Ifle => pc = self.cond1(pc, |v| v <= 0, frame)?,
                // ---- 双操作数条件分支 ----
                Opcode::IfIcmpeq => pc = self.cond2(pc, |a, b| a == b, frame)?,
                Opcode::IfIcmpne => pc = self.cond2(pc, |a, b| a != b, frame)?,
                Opcode::IfIcmplt => pc = self.cond2(pc, |a, b| a < b, frame)?,
                Opcode::IfIcmpge => pc = self.cond2(pc, |a, b| a >= b, frame)?,
                Opcode::IfIcmpgt => pc = self.cond2(pc, |a, b| a > b, frame)?,
                Opcode::IfIcmple => pc = self.cond2(pc, |a, b| a <= b, frame)?,
                // ---- 无条件跳转 ----
                Opcode::Goto => {
                    let off = self.read_s2(pc + 1)?;
                    pc = Self::branch_target(pc, off)?;
                }
                Opcode::Return => return Ok(Value::Void),
                Opcode::Ireturn => {
                    let v = frame.operands.pop_int()?;
                    return Ok(Value::Int(v));
                }
                other => return Err(VmError::UnsupportedOpcode(other)),
            }
        }
    }

    // ---- 操作数读取(带越界检查,大端)----

    fn read_u1(&self, at: usize) -> Result<u8, VmError> {
        self.code.get(at).copied().ok_or(VmError::BadPc(at))
    }

    fn read_s1(&self, at: usize) -> Result<i8, VmError> {
        Ok(self.read_u1(at)? as i8)
    }

    fn read_s2(&self, at: usize) -> Result<i16, VmError> {
        let b0 = self.read_u1(at)?;
        let b1 = self.read_u1(at + 1)?;
        Ok(i16::from_be_bytes([b0, b1]))
    }

    /// 单操作数条件分支:弹出 v,`pred(v)` 为真则跳到 `pc+offset`,否则 `pc+3`。
    fn cond1(&self, pc: usize, pred: impl Fn(i32) -> bool, frame: &mut Frame) -> Result<usize, VmError> {
        let v = frame.operands.pop_int()?;
        let off = self.read_s2(pc + 1)?;
        Ok(if pred(v) {
            Self::branch_target(pc, off)?
        } else {
            pc + 3
        })
    }

    /// 双操作数条件分支:弹出 b(顶)、a(底),`pred(a,b)` 为真则跳,否则 `pc+3`。
    fn cond2(&self, pc: usize, pred: impl Fn(i32, i32) -> bool, frame: &mut Frame) -> Result<usize, VmError> {
        let b = frame.operands.pop_int()?;
        let a = frame.operands.pop_int()?;
        let off = self.read_s2(pc + 1)?;
        Ok(if pred(a, b) {
            Self::branch_target(pc, off)?
        } else {
            pc + 3
        })
    }

    /// 分支目标 = `pc + offset`;offset 负到下溢则 `BadPc`。
    fn branch_target(pc: usize, offset: i16) -> Result<usize, VmError> {
        let target = (pc as i64) + (offset as i64);
        if target < 0 {
            return Err(VmError::BadPc(pc));
        }
        Ok(target as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// count=1 的空常量池(仅占位,无可用条目),供不访问常量池的单元测试使用。
    fn empty_cp() -> ConstantPool {
        use crate::classfile::Reader;
        ConstantPool::parse(&mut Reader::new(&[0x00, 0x01])).unwrap()
    }

    #[test]
    fn iconst_then_ireturn_returns_int() {
        let code = [Opcode::Iconst5 as u8, Opcode::Ireturn as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 1);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(5));
    }

    /// 运行 `code` 至返回(空常量池,0 局部变量,给定操作数栈深),返回 Int 值。
    fn run_int(code: &[u8], max_stack: u16) -> i32 {
        let cp = empty_cp();
        let interp = Interpreter::new(code, &cp);
        let mut frame = Frame::new(0, max_stack);
        match interp.interpret(&mut frame).unwrap() {
            Value::Int(v) => v,
            other => panic!("expected Int, got {other:?}"),
        }
    }

    #[test]
    fn iconst_variants_load_constants() {
        assert_eq!(run_int(&[Opcode::IconstM1 as u8, Opcode::Ireturn as u8], 1), -1);
        assert_eq!(run_int(&[Opcode::Iconst0 as u8, Opcode::Ireturn as u8], 1), 0);
        assert_eq!(run_int(&[Opcode::Iconst1 as u8, Opcode::Ireturn as u8], 1), 1);
        assert_eq!(run_int(&[Opcode::Iconst2 as u8, Opcode::Ireturn as u8], 1), 2);
        assert_eq!(run_int(&[Opcode::Iconst3 as u8, Opcode::Ireturn as u8], 1), 3);
        assert_eq!(run_int(&[Opcode::Iconst4 as u8, Opcode::Ireturn as u8], 1), 4);
    }

    #[test]
    fn bipush_loads_signed_byte() {
        assert_eq!(
            run_int(&[Opcode::Bipush as u8, 0x2A, Opcode::Ireturn as u8], 1),
            42
        );
        // 0xFF 应解释为 -1
        assert_eq!(
            run_int(&[Opcode::Bipush as u8, 0xFF, Opcode::Ireturn as u8], 1),
            -1
        );
    }

    #[test]
    fn sipush_loads_signed_short() {
        assert_eq!(
            run_int(&[Opcode::Sipush as u8, 0x03, 0xE8, Opcode::Ireturn as u8], 1),
            1000
        );
        assert_eq!(
            run_int(&[Opcode::Sipush as u8, 0xFF, 0xFE, Opcode::Ireturn as u8], 1),
            -2
        );
    }

    #[test]
    fn ldc_loads_integer_from_constant_pool() {
        // count=2,[1]=Integer(42)
        let cp_bytes = [0x00, 0x02, 0x03, 0x00, 0x00, 0x00, 0x2A];
        let cp = ConstantPool::parse(&mut crate::classfile::Reader::new(&cp_bytes)).unwrap();
        let code = [Opcode::Ldc as u8, 0x01, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 1);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(42));
    }

    /// 在预置局部变量后运行,返回 Int 值。
    fn run_int_locals(code: &[u8], max_locals: u16, setup: impl FnOnce(&mut Frame)) -> i32 {
        let cp = empty_cp();
        let interp = Interpreter::new(code, &cp);
        let mut frame = Frame::new(max_locals, 4);
        setup(&mut frame);
        match interp.interpret(&mut frame).unwrap() {
            Value::Int(v) => v,
            other => panic!("expected Int, got {other:?}"),
        }
    }

    #[test]
    fn iload_reads_local_by_index() {
        let code = [Opcode::Iload as u8, 0x01, Opcode::Ireturn as u8];
        assert_eq!(run_int_locals(&code, 2, |f| f.locals.set_int(1, 99).unwrap()), 99);
    }

    #[test]
    fn iload_n_short_forms_read_locals() {
        assert_eq!(
            run_int_locals(&[Opcode::Iload0 as u8, Opcode::Ireturn as u8], 1, |f| {
                f.locals.set_int(0, 10).unwrap()
            }),
            10
        );
        assert_eq!(
            run_int_locals(&[Opcode::Iload1 as u8, Opcode::Ireturn as u8], 2, |f| {
                f.locals.set_int(1, 20).unwrap()
            }),
            20
        );
        assert_eq!(
            run_int_locals(&[Opcode::Iload2 as u8, Opcode::Ireturn as u8], 3, |f| {
                f.locals.set_int(2, 30).unwrap()
            }),
            30
        );
        assert_eq!(
            run_int_locals(&[Opcode::Iload3 as u8, Opcode::Ireturn as u8], 4, |f| {
                f.locals.set_int(3, 40).unwrap()
            }),
            40
        );
    }

    #[test]
    fn istore_writes_local_by_index() {
        // iconst_5; istore 2; iload 2; ireturn -> 5
        let code = [
            Opcode::Iconst5 as u8,
            Opcode::Istore as u8,
            0x02,
            Opcode::Iload as u8,
            0x02,
            Opcode::Ireturn as u8,
        ];
        assert_eq!(run_int_locals(&code, 3, |_| {}), 5);
    }

    #[test]
    fn istore_n_short_forms_write_locals() {
        assert_eq!(
            run_int_locals(
                &[Opcode::Bipush as u8, 0x09, Opcode::Istore3 as u8, Opcode::Iload3 as u8, Opcode::Ireturn as u8],
                4,
                |_| {}
            ),
            9
        );
    }

    // ---- 算术 ----

    #[test]
    fn iadd_pops_two_and_pushes_sum() {
        let code = [Opcode::Iconst2 as u8, Opcode::Iconst3 as u8, Opcode::Iadd as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 5);
    }

    #[test]
    fn isub_truncates_stack_order() {
        // 5 - 2 = 3
        let code = [Opcode::Iconst5 as u8, Opcode::Iconst2 as u8, Opcode::Isub as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 3);
    }

    #[test]
    fn imul_multiplies() {
        let code = [Opcode::Iconst3 as u8, Opcode::Iconst4 as u8, Opcode::Imul as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 12);
    }

    #[test]
    fn idiv_truncates_toward_zero() {
        // 20 / 3 = 6
        let code = [Opcode::Bipush as u8, 0x14, Opcode::Iconst3 as u8, Opcode::Idiv as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 6);
    }

    #[test]
    fn irem_truncates_toward_zero() {
        // 20 % 3 = 2
        let code = [Opcode::Bipush as u8, 0x14, Opcode::Iconst3 as u8, Opcode::Irem as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 2);
    }

    #[test]
    fn ineg_negates() {
        let code = [Opcode::Iconst5 as u8, Opcode::Ineg as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 1), -5);
    }

    #[test]
    fn iinc_adds_constant_to_local() {
        // local 0 = 5; iinc 0 +3; iload_0; ireturn -> 8
        assert_eq!(
            run_int_locals(
                &[Opcode::Iinc as u8, 0x00, 0x03, Opcode::Iload0 as u8, Opcode::Ireturn as u8],
                1,
                |f| {
                    f.locals.set_int(0, 5).unwrap()
                }
            ),
            8
        );
        // 负常量:iinc 0 -1(0xFF)
        assert_eq!(
            run_int_locals(
                &[Opcode::Iinc as u8, 0x00, 0xFF, Opcode::Iload0 as u8, Opcode::Ireturn as u8],
                1,
                |f| {
                    f.locals.set_int(0, 5).unwrap()
                }
            ),
            4
        );
    }

    #[test]
    fn idiv_by_zero_is_dividebyzero_error() {
        let code = [Opcode::Iconst5 as u8, Opcode::Iconst0 as u8, Opcode::Idiv as u8, Opcode::Ireturn as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap_err(), VmError::DivideByZero);
    }

    #[test]
    fn irem_by_zero_is_dividebyzero_error() {
        let code = [Opcode::Iconst5 as u8, Opcode::Iconst0 as u8, Opcode::Irem as u8, Opcode::Ireturn as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap_err(), VmError::DivideByZero);
    }

    /// 以 [1]=Integer(value) 构造常量池。
    fn cp_with_int(value: i32) -> ConstantPool {
        let mut bytes = vec![0x00, 0x02, 0x03]; // count=2, Integer tag
        bytes.extend_from_slice(&value.to_be_bytes());
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn iadd_wraps_on_overflow() {
        // INT_MAX + 1 == INT_MIN
        let cp = cp_with_int(i32::MAX);
        let code = [Opcode::Ldc as u8, 0x01, Opcode::Iconst1 as u8, Opcode::Iadd as u8, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(i32::MIN));
    }

    #[test]
    fn idiv_min_over_neg1_wraps() {
        // INT_MIN / -1 == INT_MIN (无异常,回绕)
        let cp = cp_with_int(i32::MIN);
        let code = [Opcode::Ldc as u8, 0x01, Opcode::IconstM1 as u8, Opcode::Idiv as u8, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(i32::MIN));
    }

    // ---- 位运算与移位 ----

    #[test]
    fn iand_ior_ixor() {
        let and = [Opcode::Bipush as u8, 0x0C, Opcode::Bipush as u8, 0x0A, Opcode::Iand as u8, Opcode::Ireturn as u8];
        let or = [Opcode::Bipush as u8, 0x0C, Opcode::Bipush as u8, 0x0A, Opcode::Ior as u8, Opcode::Ireturn as u8];
        let xor = [Opcode::Bipush as u8, 0x0C, Opcode::Bipush as u8, 0x0A, Opcode::Ixor as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&and, 2), 0x0C & 0x0A);
        assert_eq!(run_int(&or, 2), 0x0C | 0x0A);
        assert_eq!(run_int(&xor, 2), 0x0C ^ 0x0A);
    }

    #[test]
    fn shifts_match_java_semantics() {
        // 1 << 3 == 8
        let shl = [Opcode::Iconst1 as u8, Opcode::Iconst3 as u8, Opcode::Ishl as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&shl, 2), 8);
        // -8 >> 1 == -4 (算术右移)
        let shr = [Opcode::Bipush as u8, 0xF8, Opcode::Iconst1 as u8, Opcode::Ishr as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&shr, 2), -4);
        // -8 >>> 1 == 0x7FFFFFFC (逻辑右移)
        let ushr = [Opcode::Bipush as u8, 0xF8, Opcode::Iconst1 as u8, Opcode::Iushr as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&ushr, 2), 0x7FFF_FFFC);
    }

    // ---- 栈操作 ----

    #[test]
    fn dup_duplicates_top_category1() {
        // iconst_5; dup; iadd -> 5 + 5 = 10
        let code = [Opcode::Iconst5 as u8, Opcode::Dup as u8, Opcode::Iadd as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 10);
    }

    #[test]
    fn pop_discards_top_category1() {
        // iconst_5; iconst_3; pop; ireturn -> 5
        let code = [Opcode::Iconst5 as u8, Opcode::Iconst3 as u8, Opcode::Pop as u8, Opcode::Ireturn as u8];
        assert_eq!(run_int(&code, 2), 5);
    }

    // ---- 控制流 ----

    /// 运行 [setup..., cond, off_hi, off_lo, Iconst0, Ireturn, Iconst1, Ireturn]。
    /// cond 在 setup.len() 处,off=+5 使分支落到 Iconst1;不跳则顺延到 Iconst0。
    /// 返回 1 表示分支跳了,0 表示没跳。
    fn branch_taken(setup: &[u8], cond: Opcode) -> i32 {
        let mut code = setup.to_vec();
        code.push(cond as u8);
        code.extend_from_slice(&5i16.to_be_bytes()); // offset = +5 → Iconst1
        code.push(Opcode::Iconst0 as u8); // 不跳的落点
        code.push(Opcode::Ireturn as u8);
        code.push(Opcode::Iconst1 as u8); // 跳的落点
        code.push(Opcode::Ireturn as u8);
        run_int(&code, 4)
    }

    #[test]
    fn if_branches_on_correct_condition() {
        use Opcode::*;
        // 单操作数:iconst_X 后接 if*
        assert_eq!(branch_taken(&[Iconst0 as u8], Ifeq), 1); // 0 == 0 → 跳
        assert_eq!(branch_taken(&[Iconst1 as u8], Ifeq), 0); // 1 != 0 → 不跳
        assert_eq!(branch_taken(&[Iconst0 as u8], Ifne), 0); // 0 != 0 → 不跳
        assert_eq!(branch_taken(&[Iconst1 as u8], Ifne), 1); // 1 != 0 → 跳
    }

    #[test]
    fn if_signed_comparisons_are_correct() {
        use Opcode::*;
        // iflt: -1 < 0 → 跳
        assert_eq!(branch_taken(&[Bipush as u8, 0xFF], Iflt), 1);
        // ifge: 0 >= 0 → 跳
        assert_eq!(branch_taken(&[Iconst0 as u8], Ifge), 1);
        // ifgt: 1 > 0 → 跳
        assert_eq!(branch_taken(&[Iconst1 as u8], Ifgt), 1);
        // ifle: -1 <= 0 → 跳
        assert_eq!(branch_taken(&[Bipush as u8, 0xFF], Ifle), 1);
        // ifge 当值 < 0 → 不跳
        assert_eq!(branch_taken(&[Bipush as u8, 0xFF], Ifge), 0);
    }

    #[test]
    fn if_icmp_comparisons_are_correct() {
        use Opcode::*;
        // 栈:value1(底) value2(顶)。if_icmplt: value1 < value2
        // 3 < 5 → 跳
        assert_eq!(branch_taken(&[Iconst3 as u8, Iconst5 as u8], IfIcmplt), 1);
        // 5 < 3 → 不跳
        assert_eq!(branch_taken(&[Iconst5 as u8, Iconst3 as u8], IfIcmplt), 0);
        // 3 == 3 → if_icmpeq 跳
        assert_eq!(branch_taken(&[Iconst3 as u8, Iconst3 as u8], IfIcmpeq), 1);
        // 3 != 4 → if_icmpne 跳
        assert_eq!(branch_taken(&[Iconst3 as u8, Iconst4 as u8], IfIcmpne), 1);
        // 5 >= 3 → if_icmpge 跳
        assert_eq!(branch_taken(&[Iconst5 as u8, Iconst3 as u8], IfIcmpge), 1);
        // 5 > 3 → if_icmpgt 跳
        assert_eq!(branch_taken(&[Iconst5 as u8, Iconst3 as u8], IfIcmpgt), 1);
        // 3 <= 5 → if_icmple 跳
        assert_eq!(branch_taken(&[Iconst3 as u8, Iconst5 as u8], IfIcmple), 1);
    }

    #[test]
    fn goto_unconditionally_jumps() {
        // iconst_1; goto +4; iconst_2(跳过); ireturn -> 1
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::Goto as u8,
            0x00,
            0x04, // offset +4 → 落到 ireturn
            Opcode::Iconst2 as u8, // 被跳过
            Opcode::Ireturn as u8,
        ];
        assert_eq!(run_int(&code, 1), 1);
    }

    #[test]
    fn void_return_returns_void() {
        let code = [Opcode::Return as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 0);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Void);
    }

    #[test]
    fn running_off_end_is_badpc() {
        // 无 return,跑飞末尾
        let code = [Opcode::Nop as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 0);
        assert_eq!(interp.interpret(&mut frame).unwrap_err(), VmError::BadPc(1));
    }
}
