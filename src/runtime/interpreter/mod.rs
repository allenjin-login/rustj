//! 字节码解释器(对应 HotSpot `interpreter/zero/bytecodeInterpreter`)。
//!
//! 在一个栈帧上分派执行字节码,直到 `*return`。详见
//! `docs/superpowers/specs/2026-06-20-interpreter-design.md`(Layer 3)。
//!
//! 3.1:仅 int 核心子集;清单外指令返回 [`VmError::UnsupportedOpcode`]。

mod array;
mod clinit;
mod exception;
mod field;
mod invoke;
mod native;
mod string;
mod type_check;

use crate::bytecode::opcode::{BytecodeError, Opcode};
use crate::classfile::attributes::ExceptionTableEntry;
use crate::classfile::ClassFileError;
use crate::constant_pool::entry::ConstantPoolEntry;
use crate::constant_pool::ConstantPool;

use super::frame::{Frame, FrameError};
use super::slot::Reference;
use super::Vm;

/// 解释器执行结果值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Reference(Reference),
    Void,
}

/// 单步分派后,调用方循环应如何继续。
pub(super) enum Step {
    /// 继续下一条指令(`pc` 已由 dispatch 写回推进)。
    Continue,
    /// 当前方法返回(`*return`);携带返回值(void 用 [`Value::Void`])。
    Return(Value),
}

/// 抛出一个 Java 异常:取引导桩类(`class_name` 须已由 [`ClassRegistry::new`] 预装)
/// → `new_instance` → 在堆上分配 → 包装为 [`VmError::ThrownException`]。统一的 Java
/// 异常通道——所有运行时异常(NPE/CCE/ArithmeticException/…)与用户 `athrow` 同源,
/// 均经异常表分派、可被 `try/catch` 捕获。
///
/// 沿用 4.2 的 `'a` 借用技巧:[`Vm::registry`] 返回 `&'a ClassRegistry`(寿命不绑 `&self`),
/// 故取出 `&'a LoadedClass` 后仍可 `&mut vm` 写堆。
pub(super) fn throw_exception(vm: &mut Vm<'_>, class_name: &str) -> VmError {
    use crate::oops::Oop;
    let reg = vm
        .registry()
        .expect("抛异常需类注册表(内部不变量:引导桩须在 Vm 构造期装好)");
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("{class_name} 应作为引导桩已加载(内部不变量)"));
    let oop = Oop::Instance(reg.new_instance(lc));
    let reference = vm.heap_mut().alloc(oop);
    VmError::ThrownException(reference)
}

/// 运行时错误——二分:**Java 异常**(可捕获)vs **内部故障**(不可捕获)。
///
/// - [`VmError::ThrownException`]:唯一的 Java 异常通道——用户 `athrow` 与 JVM 自动抛出
///   (NPE/CCE/ArithmeticException/AIOOBE/…)统一经此,沿异常表分派,**同帧/跨帧均可捕获**。
/// - 其余变体:解释器内部故障(`UnsupportedOpcode`/`BadPc`/`Frame`/`ConstantPool`/
///   `BadConstant`),对应 `VerifyError`/`InternalError` 性质,本层**不**抛给用户代码捕获,
///   直接传播出 `interpret_with`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
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
    /// Java 异常(用户 athrow 或 JVM 自动抛出):异常对象引用,沿异常表分派直至捕获。
    ThrownException(crate::runtime::Reference),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedOpcode(op) => {
                write!(f, "unsupported opcode: {} (0x{:02X})", op.name(), *op as u8)
            }
            Self::BadPc(pc) => write!(f, "pc ran off bytecode end: {pc}"),
            Self::Frame(e) => write!(f, "frame error: {e:?}"),
            Self::ConstantPool(e) => write!(f, "constant pool: {e:?}"),
            Self::BadConstant(msg) => write!(f, "bad constant: {msg}"),
            Self::ThrownException(_) => write!(f, "ThrownException"),
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
    exception_table: &'a [ExceptionTableEntry],
}

impl<'a> Interpreter<'a> {
    pub fn new(code: &'a [u8], cp: &'a ConstantPool) -> Self {
        Self {
            code,
            cp,
            exception_table: &[],
        }
    }

    /// 附上方法的异常表(`Code` 属性)。被调用者解释器(4 处 invoke)与集成闸门入口
    /// 用之;`new(code, cp)` 默认空表。流式:`Interpreter::new(..).with_exception_table(..)`。
    pub fn with_exception_table(mut self, exception_table: &'a [ExceptionTableEntry]) -> Self {
        self.exception_table = exception_table;
        self
    }

    /// 当前字节码所属的常量池(供 invoke 子模块解析 Methodref)。
    pub(crate) fn cp(&self) -> &'a ConstantPool {
        self.cp
    }

    /// 当前方法的异常表(供 `athrow` / invoke 跨帧异常分派扫描处理者)。
    pub(crate) fn exception_table(&self) -> &'a [ExceptionTableEntry] {
        self.exception_table
    }

    /// 便捷入口:无对象/类上下文,用默认空 [`Vm`] 执行(纯数值路径)。
    ///
    /// 既有单帧测试与此路径兼容;需要对象/字段/`invokestatic` 时用 [`Self::interpret_with`]。
    pub fn interpret(&self, frame: &mut Frame) -> Result<Value, VmError> {
        // 便捷路径自带注册表(装好引导异常桩):即便纯算术字节码也可能抛运行时异常
        // (idiv/irem 除零 → ArithmeticException),故构造 registry 而非空 Vm,让
        // throw_exception 有类可分配、有堆可入——避免"无注册表却需抛异常"的 panic。
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        self.interpret_with(frame, &mut vm)
    }

    /// 带 [`Vm`](对象堆 + 类注册表)执行至 `*return`;对象/字段/`invokestatic` 经此路径。
    pub fn interpret_with(&self, frame: &mut Frame, vm: &mut Vm<'_>) -> Result<Value, VmError> {
        let mut pc: usize = 0;
        loop {
            if pc >= self.code.len() {
                return Err(VmError::BadPc(pc));
            }
            let op = Opcode::from_u8(self.code[pc])?;
            // 单步执行;运行时异常(ThrownException)由本帧异常表捕获,未命中才上传
            // (跨帧则由上层 invoke 的 finish_invoke 扫调用者表)。同帧/跨帧共用 find_handler。
            match self.dispatch(op, frame, vm, &mut pc) {
                Ok(Step::Continue) => {}
                Ok(Step::Return(v)) => return Ok(v),
                Err(VmError::ThrownException(exc)) => {
                    match exception::find_handler(self, vm, self.exception_table, pc, exc)? {
                        Some(h) => {
                            frame.operands.clear();
                            frame.operands.push_reference(exc)?;
                            pc = h;
                        }
                        None => return Err(VmError::ThrownException(exc)),
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 单步分派:执行一条指令(`op` @ `*pc_slot`),返回 [`Step`] 或 `Err`。
    ///
    /// `pc` 为本指令起点;正常推进由各臂 `pc += n` / `pc = target` 后于函数尾写回
    /// `*pc_slot`。`Err` 路径不写回 → `*pc_slot` 仍指**故障指令**,供
    /// [`Self::interpret_with`] 的异常表扫描用对 pc。运行时异常经 [`throw_exception`]
    /// 统一为 `ThrownException`(同帧可捕获);内部故障(BadConstant/BadPc/Frame/…)直接
    /// 传播(不可捕获,对应 VerifyError/InternalError 性质)。
    fn dispatch(
        &self,
        op: Opcode,
        frame: &mut Frame,
        vm: &mut Vm<'_>,
        pc_slot: &mut usize,
    ) -> Result<Step, VmError> {
        let mut pc = *pc_slot;
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
                self.load_constant(frame, vm, index)?;
                pc += 2;
            }
            Opcode::LdcW => {
                let index = self.read_u2(pc + 1)?;
                self.load_constant(frame, vm, index)?;
                pc += 3;
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
            // ---- 引用局部变量(aload/astore)----
            Opcode::Aload => {
                let idx = self.read_u1(pc + 1)? as u16;
                frame.operands.push_reference(frame.locals.get_reference(idx)?)?;
                pc += 2;
            }
            Opcode::Aload0 => {
                frame.operands.push_reference(frame.locals.get_reference(0)?)?;
                pc += 1;
            }
            Opcode::Aload1 => {
                frame.operands.push_reference(frame.locals.get_reference(1)?)?;
                pc += 1;
            }
            Opcode::Aload2 => {
                frame.operands.push_reference(frame.locals.get_reference(2)?)?;
                pc += 1;
            }
            Opcode::Aload3 => {
                frame.operands.push_reference(frame.locals.get_reference(3)?)?;
                pc += 1;
            }
            Opcode::Astore => {
                let idx = self.read_u1(pc + 1)? as u16;
                let v = frame.operands.pop_reference()?;
                frame.locals.set_reference(idx, v)?;
                pc += 2;
            }
            Opcode::Astore0 => {
                let v = frame.operands.pop_reference()?;
                frame.locals.set_reference(0, v)?;
                pc += 1;
            }
            Opcode::Astore1 => {
                let v = frame.operands.pop_reference()?;
                frame.locals.set_reference(1, v)?;
                pc += 1;
            }
            Opcode::Astore2 => {
                let v = frame.operands.pop_reference()?;
                frame.locals.set_reference(2, v)?;
                pc += 1;
            }
            Opcode::Astore3 => {
                let v = frame.operands.pop_reference()?;
                frame.locals.set_reference(3, v)?;
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
                    return Err(throw_exception(vm, "java/lang/ArithmeticException"));
                }
                frame.operands.push_int(l.wrapping_div(r))?;
                pc += 1;
            }
            Opcode::Irem => {
                let r = frame.operands.pop_int()?;
                let l = frame.operands.pop_int()?;
                if r == 0 {
                    return Err(throw_exception(vm, "java/lang/ArithmeticException"));
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
            // ---- long:常量与加载 ----
            Opcode::Lconst0 => {
                frame.operands.push_long(0)?;
                pc += 1;
            }
            Opcode::Lconst1 => {
                frame.operands.push_long(1)?;
                pc += 1;
            }
            Opcode::Ldc2W => {
                let index = self.read_u2(pc + 1)?;
                match self.cp.get(index)? {
                    ConstantPoolEntry::Long(v) => frame.operands.push_long(*v)?,
                    ConstantPoolEntry::Double(v) => frame.operands.push_double(*v)?,
                    _ => return Err(VmError::BadConstant("ldc2_w 期望 Long/Double")),
                }
                pc += 3;
            }
            Opcode::Lload => {
                let idx = self.read_u1(pc + 1)? as u16;
                frame.operands.push_long(frame.locals.get_long(idx)?)?;
                pc += 2;
            }
            Opcode::Lload0 => {
                frame.operands.push_long(frame.locals.get_long(0)?)?;
                pc += 1;
            }
            Opcode::Lload1 => {
                frame.operands.push_long(frame.locals.get_long(1)?)?;
                pc += 1;
            }
            Opcode::Lload2 => {
                frame.operands.push_long(frame.locals.get_long(2)?)?;
                pc += 1;
            }
            Opcode::Lload3 => {
                frame.operands.push_long(frame.locals.get_long(3)?)?;
                pc += 1;
            }
            Opcode::Lstore => {
                let idx = self.read_u1(pc + 1)? as u16;
                let v = frame.operands.pop_long()?;
                frame.locals.set_long(idx, v)?;
                pc += 2;
            }
            Opcode::Lstore0 => {
                let v = frame.operands.pop_long()?;
                frame.locals.set_long(0, v)?;
                pc += 1;
            }
            Opcode::Lstore1 => {
                let v = frame.operands.pop_long()?;
                frame.locals.set_long(1, v)?;
                pc += 1;
            }
            Opcode::Lstore2 => {
                let v = frame.operands.pop_long()?;
                frame.locals.set_long(2, v)?;
                pc += 1;
            }
            Opcode::Lstore3 => {
                let v = frame.operands.pop_long()?;
                frame.locals.set_long(3, v)?;
                pc += 1;
            }
            // ---- long:算术 ----
            Opcode::Ladd => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l.wrapping_add(r))?;
                pc += 1;
            }
            Opcode::Lsub => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l.wrapping_sub(r))?;
                pc += 1;
            }
            Opcode::Lmul => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l.wrapping_mul(r))?;
                pc += 1;
            }
            Opcode::Ldiv => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                if r == 0 {
                    return Err(throw_exception(vm, "java/lang/ArithmeticException"));
                }
                frame.operands.push_long(l.wrapping_div(r))?;
                pc += 1;
            }
            Opcode::Lrem => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                if r == 0 {
                    return Err(throw_exception(vm, "java/lang/ArithmeticException"));
                }
                frame.operands.push_long(l.wrapping_rem(r))?;
                pc += 1;
            }
            Opcode::Lneg => {
                let v = frame.operands.pop_long()?;
                frame.operands.push_long(v.wrapping_neg())?;
                pc += 1;
            }
            // ---- long:位运算与移位 ----
            Opcode::Land => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l & r)?;
                pc += 1;
            }
            Opcode::Lor => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l | r)?;
                pc += 1;
            }
            Opcode::Lxor => {
                let r = frame.operands.pop_long()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l ^ r)?;
                pc += 1;
            }
            Opcode::Lshl => {
                let s = frame.operands.pop_int()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l.wrapping_shl(s as u32))?;
                pc += 1;
            }
            Opcode::Lshr => {
                let s = frame.operands.pop_int()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(l.wrapping_shr(s as u32))?;
                pc += 1;
            }
            Opcode::Lushr => {
                let s = frame.operands.pop_int()?;
                let l = frame.operands.pop_long()?;
                frame.operands.push_long(((l as u64).wrapping_shr(s as u32)) as i64)?;
                pc += 1;
            }
            Opcode::Lcmp => {
                let b = frame.operands.pop_long()?;
                let a = frame.operands.pop_long()?;
                let r = if a < b { -1 } else if a > b { 1 } else { 0 };
                frame.operands.push_int(r)?;
                pc += 1;
            }
            Opcode::I2l => {
                let v = frame.operands.pop_int()?;
                frame.operands.push_long(v as i64)?;
                pc += 1;
            }
            Opcode::L2i => {
                let v = frame.operands.pop_long()?;
                frame.operands.push_int(v as i32)?;
                pc += 1;
            }
            // ---- float:常量与加载 ----
            Opcode::Fconst0 => {
                frame.operands.push_float(0.0)?;
                pc += 1;
            }
            Opcode::Fconst1 => {
                frame.operands.push_float(1.0)?;
                pc += 1;
            }
            Opcode::Fconst2 => {
                frame.operands.push_float(2.0)?;
                pc += 1;
            }
            Opcode::Fload => {
                let idx = self.read_u1(pc + 1)? as u16;
                frame.operands.push_float(frame.locals.get_float(idx)?)?;
                pc += 2;
            }
            Opcode::Fload0 => {
                frame.operands.push_float(frame.locals.get_float(0)?)?;
                pc += 1;
            }
            Opcode::Fload1 => {
                frame.operands.push_float(frame.locals.get_float(1)?)?;
                pc += 1;
            }
            Opcode::Fload2 => {
                frame.operands.push_float(frame.locals.get_float(2)?)?;
                pc += 1;
            }
            Opcode::Fload3 => {
                frame.operands.push_float(frame.locals.get_float(3)?)?;
                pc += 1;
            }
            Opcode::Fstore => {
                let idx = self.read_u1(pc + 1)? as u16;
                let v = frame.operands.pop_float()?;
                frame.locals.set_float(idx, v)?;
                pc += 2;
            }
            Opcode::Fstore0 => {
                let v = frame.operands.pop_float()?;
                frame.locals.set_float(0, v)?;
                pc += 1;
            }
            Opcode::Fstore1 => {
                let v = frame.operands.pop_float()?;
                frame.locals.set_float(1, v)?;
                pc += 1;
            }
            Opcode::Fstore2 => {
                let v = frame.operands.pop_float()?;
                frame.locals.set_float(2, v)?;
                pc += 1;
            }
            Opcode::Fstore3 => {
                let v = frame.operands.pop_float()?;
                frame.locals.set_float(3, v)?;
                pc += 1;
            }
            // ---- float:算术 ----
            Opcode::Fadd => {
                let r = frame.operands.pop_float()?;
                let l = frame.operands.pop_float()?;
                frame.operands.push_float(l + r)?;
                pc += 1;
            }
            Opcode::Fsub => {
                let r = frame.operands.pop_float()?;
                let l = frame.operands.pop_float()?;
                frame.operands.push_float(l - r)?;
                pc += 1;
            }
            Opcode::Fmul => {
                let r = frame.operands.pop_float()?;
                let l = frame.operands.pop_float()?;
                frame.operands.push_float(l * r)?;
                pc += 1;
            }
            Opcode::Fdiv => {
                let r = frame.operands.pop_float()?;
                let l = frame.operands.pop_float()?;
                frame.operands.push_float(l / r)?;
                pc += 1;
            }
            Opcode::Frem => {
                let r = frame.operands.pop_float()?;
                let l = frame.operands.pop_float()?;
                frame.operands.push_float(l % r)?;
                pc += 1;
            }
            Opcode::Fneg => {
                let v = frame.operands.pop_float()?;
                frame.operands.push_float(-v)?;
                pc += 1;
            }
            Opcode::Fcmpl => {
                let r = cmp_float(frame, false)?;
                frame.operands.push_int(r)?;
                pc += 1;
            }
            Opcode::Fcmpg => {
                let r = cmp_float(frame, true)?;
                frame.operands.push_int(r)?;
                pc += 1;
            }
            Opcode::I2f => {
                let v = frame.operands.pop_int()?;
                frame.operands.push_float(v as f32)?;
                pc += 1;
            }
            Opcode::F2i => {
                let v = frame.operands.pop_float()?;
                frame.operands.push_int(v as i32)?;
                pc += 1;
            }
            // ---- double:常量与加载 ----
            Opcode::Dconst0 => {
                frame.operands.push_double(0.0)?;
                pc += 1;
            }
            Opcode::Dconst1 => {
                frame.operands.push_double(1.0)?;
                pc += 1;
            }
            Opcode::Dload => {
                let idx = self.read_u1(pc + 1)? as u16;
                frame.operands.push_double(frame.locals.get_double(idx)?)?;
                pc += 2;
            }
            Opcode::Dload0 => {
                frame.operands.push_double(frame.locals.get_double(0)?)?;
                pc += 1;
            }
            Opcode::Dload1 => {
                frame.operands.push_double(frame.locals.get_double(1)?)?;
                pc += 1;
            }
            Opcode::Dload2 => {
                frame.operands.push_double(frame.locals.get_double(2)?)?;
                pc += 1;
            }
            Opcode::Dload3 => {
                frame.operands.push_double(frame.locals.get_double(3)?)?;
                pc += 1;
            }
            Opcode::Dstore => {
                let idx = self.read_u1(pc + 1)? as u16;
                let v = frame.operands.pop_double()?;
                frame.locals.set_double(idx, v)?;
                pc += 2;
            }
            Opcode::Dstore0 => {
                let v = frame.operands.pop_double()?;
                frame.locals.set_double(0, v)?;
                pc += 1;
            }
            Opcode::Dstore1 => {
                let v = frame.operands.pop_double()?;
                frame.locals.set_double(1, v)?;
                pc += 1;
            }
            Opcode::Dstore2 => {
                let v = frame.operands.pop_double()?;
                frame.locals.set_double(2, v)?;
                pc += 1;
            }
            Opcode::Dstore3 => {
                let v = frame.operands.pop_double()?;
                frame.locals.set_double(3, v)?;
                pc += 1;
            }
            // ---- double:算术 ----
            Opcode::Dadd => {
                let r = frame.operands.pop_double()?;
                let l = frame.operands.pop_double()?;
                frame.operands.push_double(l + r)?;
                pc += 1;
            }
            Opcode::Dsub => {
                let r = frame.operands.pop_double()?;
                let l = frame.operands.pop_double()?;
                frame.operands.push_double(l - r)?;
                pc += 1;
            }
            Opcode::Dmul => {
                let r = frame.operands.pop_double()?;
                let l = frame.operands.pop_double()?;
                frame.operands.push_double(l * r)?;
                pc += 1;
            }
            Opcode::Ddiv => {
                let r = frame.operands.pop_double()?;
                let l = frame.operands.pop_double()?;
                frame.operands.push_double(l / r)?;
                pc += 1;
            }
            Opcode::Drem => {
                let r = frame.operands.pop_double()?;
                let l = frame.operands.pop_double()?;
                frame.operands.push_double(l % r)?;
                pc += 1;
            }
            Opcode::Dneg => {
                let v = frame.operands.pop_double()?;
                frame.operands.push_double(-v)?;
                pc += 1;
            }
            Opcode::Dcmpl => {
                let r = cmp_double(frame, false)?;
                frame.operands.push_int(r)?;
                pc += 1;
            }
            Opcode::Dcmpg => {
                let r = cmp_double(frame, true)?;
                frame.operands.push_int(r)?;
                pc += 1;
            }
            Opcode::I2d => {
                let v = frame.operands.pop_int()?;
                frame.operands.push_double(v as f64)?;
                pc += 1;
            }
            Opcode::D2i => {
                let v = frame.operands.pop_double()?;
                frame.operands.push_int(v as i32)?;
                pc += 1;
            }
            // ---- 跨数值类型转换 ----
            Opcode::L2f => {
                let v = frame.operands.pop_long()?;
                frame.operands.push_float(v as f32)?;
                pc += 1;
            }
            Opcode::L2d => {
                let v = frame.operands.pop_long()?;
                frame.operands.push_double(v as f64)?;
                pc += 1;
            }
            Opcode::F2l => {
                let v = frame.operands.pop_float()?;
                frame.operands.push_long(v as i64)?;
                pc += 1;
            }
            Opcode::F2d => {
                let v = frame.operands.pop_float()?;
                frame.operands.push_double(v as f64)?;
                pc += 1;
            }
            Opcode::D2l => {
                let v = frame.operands.pop_double()?;
                frame.operands.push_long(v as i64)?;
                pc += 1;
            }
            Opcode::D2f => {
                let v = frame.operands.pop_double()?;
                frame.operands.push_float(v as f32)?;
                pc += 1;
            }
            Opcode::I2b => {
                let v = frame.operands.pop_int()?;
                frame.operands.push_int((v as i8) as i32)?;
                pc += 1;
            }
            Opcode::I2c => {
                let v = frame.operands.pop_int()?;
                frame.operands.push_int((v as u16) as i32)?;
                pc += 1;
            }
            Opcode::I2s => {
                let v = frame.operands.pop_int()?;
                frame.operands.push_int((v as i16) as i32)?;
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
            // ---- 对象与字段(4.1)----
            Opcode::AconstNull => {
                frame.operands.push_reference(Reference::null())?;
                pc += 1;
            }
            Opcode::New => {
                let index = self.read_u2(pc + 1)?;
                field::new_instance(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Getfield => {
                let index = self.read_u2(pc + 1)?;
                field::get_field(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Putfield => {
                let index = self.read_u2(pc + 1)?;
                field::put_field(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Getstatic => {
                let index = self.read_u2(pc + 1)?;
                field::get_static(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Putstatic => {
                let index = self.read_u2(pc + 1)?;
                field::put_static(self, frame, vm, index)?;
                pc += 3;
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
            Opcode::GotoW => {
                let off = self.read_s4(pc + 1)?;
                pc = Self::branch_target_w(pc, off)?;
            }
            // ---- 引用比较分支(4.4)----
            Opcode::IfAcmpeq => pc = self.cond_ref2(pc, |a, b| a == b, frame)?,
            Opcode::IfAcmpne => pc = self.cond_ref2(pc, |a, b| a != b, frame)?,
            Opcode::Ifnull => pc = self.cond_ref1(pc, |v| v.is_null(), frame)?,
            Opcode::Ifnonnull => pc = self.cond_ref1(pc, |v| !v.is_null(), frame)?,
            // ---- switch(4.4)----
            Opcode::Tableswitch => pc = self.table_switch(pc, frame)?,
            Opcode::Lookupswitch => pc = self.lookup_switch(pc, frame)?,
            // ---- 方法调用(invokestatic:同类内,含递归与互调)----
            Opcode::Invokestatic => {
                let index = self.read_u2(pc + 1)?;
                match invoke::invoke_static(self, frame, vm, index, pc)? {
                    invoke::InvokeFlow::Fallthrough => pc += 3,
                    invoke::InvokeFlow::Jump(h) => pc = h,
                }
            }
            Opcode::Invokespecial => {
                let index = self.read_u2(pc + 1)?;
                match invoke::invoke_special(self, frame, vm, index, pc)? {
                    invoke::InvokeFlow::Fallthrough => pc += 3,
                    invoke::InvokeFlow::Jump(h) => pc = h,
                }
            }
            Opcode::Invokevirtual => {
                let index = self.read_u2(pc + 1)?;
                match invoke::invoke_virtual(self, frame, vm, index, pc)? {
                    invoke::InvokeFlow::Fallthrough => pc += 3,
                    invoke::InvokeFlow::Jump(h) => pc = h,
                }
            }
            Opcode::Invokeinterface => {
                let index = self.read_u2(pc + 1)?;
                // count(pc+3) 与尾 0(pc+4)对运行时冗余,随 pc += 5 丢弃。
                match invoke::invoke_interface(self, frame, vm, index, pc)? {
                    invoke::InvokeFlow::Fallthrough => pc += 5,
                    invoke::InvokeFlow::Jump(h) => pc = h,
                }
            }
            // ---- 数组(4.3a)----
            Opcode::Newarray => {
                let atype = self.read_u1(pc + 1)?;
                array::new_array(frame, vm, atype)?;
                pc += 2;
            }
            Opcode::Anewarray => {
                let index = self.read_u2(pc + 1)?;
                array::a_new_array(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Multianewarray => {
                let index = self.read_u2(pc + 1)?;
                let dims = self.read_u1(pc + 3)?;
                array::multi_new_array(self, frame, vm, index, dims)?;
                pc += 4;
            }
            Opcode::Checkcast => {
                let index = self.read_u2(pc + 1)?;
                type_check::check_cast(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Instanceof => {
                let index = self.read_u2(pc + 1)?;
                type_check::instance_of(self, frame, vm, index)?;
                pc += 3;
            }
            Opcode::Arraylength => {
                array::array_length(frame, vm)?;
                pc += 1;
            }
            Opcode::Iaload => {
                array::array_load(frame, vm, array::ArrayKind::Int)?;
                pc += 1;
            }
            Opcode::Laload => {
                array::array_load(frame, vm, array::ArrayKind::Long)?;
                pc += 1;
            }
            Opcode::Faload => {
                array::array_load(frame, vm, array::ArrayKind::Float)?;
                pc += 1;
            }
            Opcode::Daload => {
                array::array_load(frame, vm, array::ArrayKind::Double)?;
                pc += 1;
            }
            Opcode::Aaload => {
                array::array_load(frame, vm, array::ArrayKind::Ref)?;
                pc += 1;
            }
            Opcode::Baload => {
                array::array_load(frame, vm, array::ArrayKind::Byte)?;
                pc += 1;
            }
            Opcode::Caload => {
                array::array_load(frame, vm, array::ArrayKind::Char)?;
                pc += 1;
            }
            Opcode::Saload => {
                array::array_load(frame, vm, array::ArrayKind::Short)?;
                pc += 1;
            }
            Opcode::Iastore => {
                array::array_store(frame, vm, array::ArrayKind::Int)?;
                pc += 1;
            }
            Opcode::Lastore => {
                array::array_store(frame, vm, array::ArrayKind::Long)?;
                pc += 1;
            }
            Opcode::Fastore => {
                array::array_store(frame, vm, array::ArrayKind::Float)?;
                pc += 1;
            }
            Opcode::Dastore => {
                array::array_store(frame, vm, array::ArrayKind::Double)?;
                pc += 1;
            }
            Opcode::Aastore => {
                array::array_store(frame, vm, array::ArrayKind::Ref)?;
                pc += 1;
            }
            Opcode::Bastore => {
                array::array_store(frame, vm, array::ArrayKind::Byte)?;
                pc += 1;
            }
            Opcode::Castore => {
                array::array_store(frame, vm, array::ArrayKind::Char)?;
                pc += 1;
            }
            Opcode::Sastore => {
                array::array_store(frame, vm, array::ArrayKind::Short)?;
                pc += 1;
            }
            Opcode::Return => return Ok(Step::Return(Value::Void)),
            Opcode::Ireturn => {
                let v = frame.operands.pop_int()?;
                return Ok(Step::Return(Value::Int(v)));
            }
            Opcode::Lreturn => {
                let v = frame.operands.pop_long()?;
                return Ok(Step::Return(Value::Long(v)));
            }
            Opcode::Freturn => {
                let v = frame.operands.pop_float()?;
                return Ok(Step::Return(Value::Float(v)));
            }
            Opcode::Dreturn => {
                let v = frame.operands.pop_double()?;
                return Ok(Step::Return(Value::Double(v)));
            }
            Opcode::Areturn => {
                let v = frame.operands.pop_reference()?;
                return Ok(Step::Return(Value::Reference(v)));
            }
            Opcode::Athrow => {
                let exc = frame.operands.pop_reference()?;
                // throw null → NPE(JVM 语义);否则上传 exc。表查找统一由
                // interpret_with 的循环处理(与指令抛出的异常同源)。
                let err = if exc.is_null() {
                    throw_exception(vm, "java/lang/NullPointerException")
                } else {
                    VmError::ThrownException(exc)
                };
                return Err(err);
            }
            other => return Err(VmError::UnsupportedOpcode(other)),
        }
        *pc_slot = pc;
        Ok(Step::Continue)
    }

    /// `ldc`/`ldc_w` 取常量压栈:`Integer`/`Float` 数值,或 `String`(经 Vm intern 池 → 引用)。
    /// `Class`/`MethodType`/`MethodHandle` 等顺延。
    fn load_constant(
        &self,
        frame: &mut Frame,
        vm: &mut Vm<'_>,
        index: u16,
    ) -> Result<(), VmError> {
        match self.cp.get(index)? {
            ConstantPoolEntry::Integer(v) => frame.operands.push_int(*v)?,
            ConstantPoolEntry::Float(v) => frame.operands.push_float(*v)?,
            ConstantPoolEntry::String { string_index } => {
                let text = match self.cp.get(*string_index)? {
                    ConstantPoolEntry::Utf8(s) => s.clone(),
                    _ => return Err(VmError::BadConstant("ldc String 须指向 Utf8")),
                };
                // 真 java/lang/String 实例(经 intern,同文本恒同引用)。需注册表预载 String。
                let r = string::intern(vm, &text)?;
                frame.operands.push_reference(r)?;
            }
            ConstantPoolEntry::Class { name_index } => {
                // 类字面量(`Foo.class`):ldc 取 CONSTANT_Class → 推 Class 镜像。
                // 对应 HotSpot `ldc` 解析 Class 常量 → `java_lang_Class::as_Klass` 的镜像。
                let name = match self.cp.get(*name_index)? {
                    ConstantPoolEntry::Utf8(s) => s.clone(),
                    _ => return Err(VmError::BadConstant("ldc Class 须指向 Utf8 名")),
                };
                let r = vm
                    .heap_mut()
                    .alloc(crate::oops::Oop::Class(crate::oops::ClassOop::new(name)));
                frame.operands.push_reference(r)?;
            }
            _ => {
                return Err(VmError::BadConstant(
                    "ldc/ldc_w 期望 int/float/string/Class(MethodType 顺延)",
                ))
            }
        }
        Ok(())
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

    fn read_u2(&self, at: usize) -> Result<u16, VmError> {
        let b0 = self.read_u1(at)?;
        let b1 = self.read_u1(at + 1)?;
        Ok(u16::from_be_bytes([b0, b1]))
    }

    /// 4 字节有符号整数(大端)。
    fn read_s4(&self, at: usize) -> Result<i32, VmError> {
        let b0 = self.read_u1(at)?;
        let b1 = self.read_u1(at + 1)?;
        let b2 = self.read_u1(at + 2)?;
        let b3 = self.read_u1(at + 3)?;
        Ok(i32::from_be_bytes([b0, b1, b2, b3]))
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

    /// 单引用分支:弹 v,`pred(v)` 为真则跳到 `pc+offset`,否则 `pc+3`。
    fn cond_ref1(
        &self,
        pc: usize,
        pred: impl Fn(Reference) -> bool,
        frame: &mut Frame,
    ) -> Result<usize, VmError> {
        let v = frame.operands.pop_reference()?;
        let off = self.read_s2(pc + 1)?;
        Ok(if pred(v) {
            Self::branch_target(pc, off)?
        } else {
            pc + 3
        })
    }

    /// 双引用分支:弹 b(顶)、a(底),`pred(a,b)` 为真则跳,否则 `pc+3`。
    fn cond_ref2(
        &self,
        pc: usize,
        pred: impl Fn(Reference, Reference) -> bool,
        frame: &mut Frame,
    ) -> Result<usize, VmError> {
        let b = frame.operands.pop_reference()?;
        let a = frame.operands.pop_reference()?;
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

    /// 宽(i32)分支目标:`pc + offset`,负下溢 → `BadPc`。供 switch / goto_w。
    fn branch_target_w(pc: usize, offset: i32) -> Result<usize, VmError> {
        let target = (pc as i64) + (offset as i64);
        if target < 0 {
            return Err(VmError::BadPc(pc));
        }
        Ok(target as usize)
    }

    /// `tableswitch`:填充对齐 → 读 default/low/high/jump 表 → 按栈顶 index 跳。
    /// 所有偏移 i32,相对 switch 指令地址(`pc`)。
    fn table_switch(&self, pc: usize, frame: &mut Frame) -> Result<usize, VmError> {
        let index = frame.operands.pop_int()?;
        let pad = (4 - ((pc + 1) % 4)) % 4;
        let base = pc + 1 + pad;
        let default = self.read_s4(base)?;
        let low = self.read_s4(base + 4)?;
        let high = self.read_s4(base + 8)?;
        let off = if index < low || index > high {
            default
        } else {
            let entry = base + 12 + ((index - low) as usize) * 4;
            self.read_s4(entry)?
        };
        Self::branch_target_w(pc, off)
    }

    /// `lookupswitch`:填充对齐 → 读 default/npairs/对 → 线性匹配栈顶 key。
    /// 校验器保证按 match 升序;此处线性扫描(npairs 通常很小),命中取其 offset。
    fn lookup_switch(&self, pc: usize, frame: &mut Frame) -> Result<usize, VmError> {
        let key = frame.operands.pop_int()?;
        let pad = (4 - ((pc + 1) % 4)) % 4;
        let base = pc + 1 + pad;
        let default = self.read_s4(base)?;
        let npairs = self.read_s4(base + 4)?;
        let mut off = default;
        for i in 0..npairs as usize {
            let pair = base + 8 + i * 8;
            if self.read_s4(pair)? == key {
                off = self.read_s4(pair + 4)?;
                break;
            }
        }
        Self::branch_target_w(pc, off)
    }
}

/// float 比较:弹出 b、a,返回 -1/0/1。`gt_on_nan` 为真(fcmpg)则 NaN→1,否则(fcmpl)→-1。
fn cmp_float(frame: &mut Frame, gt_on_nan: bool) -> Result<i32, VmError> {
    let b = frame.operands.pop_float()?;
    let a = frame.operands.pop_float()?;
    Ok(if a.is_nan() || b.is_nan() {
        if gt_on_nan { 1 } else { -1 }
    } else if a < b {
        -1
    } else if a > b {
        1
    } else {
        0
    })
}

/// double 比较,语义同 [`cmp_float`]。
fn cmp_double(frame: &mut Frame, gt_on_nan: bool) -> Result<i32, VmError> {
    let b = frame.operands.pop_double()?;
    let a = frame.operands.pop_double()?;
    Ok(if a.is_nan() || b.is_nan() {
        if gt_on_nan { 1 } else { -1 }
    } else if a < b {
        -1
    } else if a > b {
        1
    } else {
        0
    })
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

    /// Stage B 闸门:断言解释结果抛出指定类的运行时异常。
    ///
    /// 运行时异常(NPE/CCE/ArithmeticException/AIOOBE 等)统一为 `ThrownException(Reference)`,
    /// 异常对象由引导桩分配在堆上。`vm` 须带注册表(装好引导桩)以备查类名。
    fn assert_throws(result: Result<Value, VmError>, vm: &Vm<'_>, expected_class: &str) {
        let Err(VmError::ThrownException(exc)) = result else {
            panic!("应抛 ThrownException({expected_class}), 实得 {result:?}");
        };
        let Some(crate::oops::Oop::Instance(i)) = vm.heap().get(exc) else {
            panic!("抛出的异常应为引导桩分配的实例, 引用 {exc:?}");
        };
        assert_eq!(i.class_name(), expected_class, "异常类名不符");
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

    #[test]
    fn ldc_w_integer_loads_value() {
        // count=2,[1]=Integer(7)(补缺:ldc_w 此前从未分派)
        let cp_bytes = [0x00, 0x02, 0x03, 0x00, 0x00, 0x00, 0x07];
        let cp = ConstantPool::parse(&mut crate::classfile::Reader::new(&cp_bytes)).unwrap();
        let code = [Opcode::LdcW as u8, 0x00, 0x01, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 1);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(7));
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
    fn idiv_by_zero_throws_arithmetic_exception() {
        let code = [Opcode::Iconst5 as u8, Opcode::Iconst0 as u8, Opcode::Idiv as u8, Opcode::Ireturn as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 2);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/ArithmeticException",
        );
    }

    #[test]
    fn irem_by_zero_throws_arithmetic_exception() {
        let code = [Opcode::Iconst5 as u8, Opcode::Iconst0 as u8, Opcode::Irem as u8, Opcode::Ireturn as u8];
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 2);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/ArithmeticException",
        );
    }

    /// 以 [1]=Integer(value) 构造常量池。
    fn cp_with_int(value: i32) -> ConstantPool {
        let mut bytes = vec![0x00, 0x02, 0x03]; // count=2, Integer tag
        bytes.extend_from_slice(&value.to_be_bytes());
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }

    /// CP:#1=Class(name_index=#2),#2=Utf8(name)。multianewarray 用索引 1。
    fn cp_with_class(name: &str) -> ConstantPool {
        let mut bytes = vec![0x00, 0x03, 0x07, 0x00, 0x02]; // count=3, Class@1->Utf8@2
        bytes.push(0x01); // Utf8 tag
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }

    // ===== Layer 4.3b:multianewarray(完全分配)=====

    #[test]
    fn multianewarray_full_outer_length() {
        // iconst_2; iconst_3; multianewarray [[I dims=2; arraylength -> 2
        let cp = cp_with_class("[[I");
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8,
            0x00,
            0x01,
            0x02,
            Opcode::Arraylength as u8,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(2));
    }

    #[test]
    fn multianewarray_full_inner_length_and_leaf() {
        // iconst_2; iconst_3; multianewarray [[I dims=2; iconst_0; aaload; arraylength -> 3
        let cp = cp_with_class("[[I");
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8,
            0x00,
            0x01,
            0x02,
            Opcode::Iconst0 as u8,
            Opcode::Aaload as u8,
            Opcode::Arraylength as u8,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(3));
    }

    // ===== Layer 4.3b:multianewarray(部分分配 + 错误)=====

    /// dims=2 < ndim=3:a[0][0] 应为 null(ifnonnull 不跳 → 返回 0)。
    #[test]
    fn multianewarray_partial_inner_is_null() {
        let cp = cp_with_class("[[[I");
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8,
            0x00,
            0x01,
            0x02,
            Opcode::Iconst0 as u8,
            Opcode::Aaload as u8, // a[0] = int[][] len 3
            Opcode::Iconst0 as u8,
            Opcode::Aaload as u8, // a[0][0] = null(部分分配)
            Opcode::Ifnonnull as u8,
            0x00,
            0x05, // 非空跳到 iconst_1
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(0));
    }

    #[test]
    fn multianewarray_negative_size_rejected() {
        let cp = cp_with_class("[[I");
        let code = [
            Opcode::IconstM1 as u8, // 外层 -1
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8,
            0x00,
            0x01,
            0x02,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/NegativeArraySizeException",
        );
    }

    #[test]
    fn multianewarray_dims_exceeds_ndim_rejected() {
        let cp = cp_with_class("[[I"); // ndim=2
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::Iconst1 as u8,
            Opcode::Iconst1 as u8,
            Opcode::Multianewarray as u8,
            0x00,
            0x01,
            0x03, // dims=3 > 2
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert!(matches!(
            interp.interpret(&mut frame),
            Err(crate::runtime::VmError::BadConstant(_))
        ));
    }

    // ===== Layer 4.5:areturn =====

    #[test]
    fn areturn_returns_null_reference() {
        use crate::runtime::Reference;
        let code = [Opcode::AconstNull as u8, Opcode::Areturn as u8];
        let cp = empty_cp();
        let mut frame = Frame::new(0, 1);
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret(&mut frame).unwrap(),
            Value::Reference(Reference::null())
        );
    }

    #[test]
    fn areturn_returns_local_reference() {
        use crate::runtime::Reference;
        let code = [Opcode::Aload0 as u8, Opcode::Areturn as u8];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 1);
        frame
            .locals
            .set_reference(0, Reference::from_id(7))
            .unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret(&mut frame).unwrap(),
            Value::Reference(Reference::from_id(7))
        );
    }

    // ===== Layer 4.7:athrow =====

    /// 构 BaseExc←SubExc 注册表 + 含异常类的 cp。
    /// 实例在各测试的 Vm 内建(避免跨 Vm 句柄失效)。
    /// cp:utf8 1=Throwable 2=BaseExc 3=SubExc ;class 4=Throwable 5=BaseExc 6=SubExc。
    fn athrow_setup() -> (crate::oops::ClassRegistry, crate::constant_pool::ConstantPool) {
        use crate::classfile::Reader;
        use crate::constant_pool::ConstantPool;
        use crate::metadata::{AccessFlags, ClassFile};
        let cp_bytes = {
            let mut b = vec![0x00, 0x07]; // count = 6 entries + 1
            for s in ["java/lang/Throwable", "BaseExc", "SubExc"] {
                b.push(0x01);
                b.extend_from_slice(&(s.len() as u16).to_be_bytes());
                b.extend_from_slice(s.as_bytes());
            }
            for idx in [1u16, 2, 3] {
                b.push(0x07);
                b.extend_from_slice(&idx.to_be_bytes());
            }
            b
        };
        let mk_cp = || ConstantPool::parse(&mut Reader::new(&cp_bytes)).unwrap();
        let mk_cf = |this: u16, super_c: u16| ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: mk_cp(),
            access_flags: AccessFlags::from_bits(0),
            this_class: this,
            super_class: super_c,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(),
        };
        let mut reg = crate::oops::ClassRegistry::new();
        reg.load(mk_cf(5, 4)).unwrap(); // BaseExc(#5) extends Throwable(#4)
        reg.load(mk_cf(6, 5)).unwrap(); // SubExc(#6) extends BaseExc
        (reg, mk_cp())
    }

    #[test]
    fn athrow_caught_jumps_to_handler() {
        use crate::classfile::attributes::ExceptionTableEntry;
        let (reg, cp) = athrow_setup();
        let mut vm = crate::runtime::Vm::new(&reg);
        let lc = reg.get("SubExc").unwrap();
        let inst = vm
            .heap_mut()
            .alloc(crate::oops::Oop::Instance(reg.new_instance(lc)));
        // [aload0(0) athrow(1) iconst1(2) ireturn(3)];try [0,2) handler=2 catch SubExc(#6)
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Athrow as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let table = [ExceptionTableEntry {
            start_pc: 0,
            end_pc: 2,
            handler_pc: 2,
            catch_type: 6,
        }];
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new(&code, &cp).with_exception_table(&table);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn athrow_uncaught_propagates() {
        let (reg, cp) = athrow_setup();
        let mut vm = crate::runtime::Vm::new(&reg);
        let lc = reg.get("SubExc").unwrap();
        let inst = vm
            .heap_mut()
            .alloc(crate::oops::Oop::Instance(reg.new_instance(lc)));
        let code = [Opcode::Aload0 as u8, Opcode::Athrow as u8];
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new(&code, &cp).with_exception_table(&[]);
        match interp.interpret_with(&mut frame, &mut vm).unwrap_err() {
            VmError::ThrownException(r) => assert_eq!(r, inst),
            other => panic!("期望 ThrownException,得 {other:?}"),
        }
    }

    #[test]
    fn athrow_null_throws_nullpointer() {
        let (reg, cp) = athrow_setup();
        let mut vm = crate::runtime::Vm::new(&reg);
        let code = [Opcode::AconstNull as u8, Opcode::Athrow as u8];
        let mut frame = Frame::new(0, 2);
        let interp = Interpreter::new(&code, &cp).with_exception_table(&[]);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/NullPointerException",
        );
    }

    /// 同帧捕获(Stage B 核心闸门):idiv 除零抛 `ArithmeticException`,被**本帧**异常表
    /// 以 `catch(java/lang/Exception)` 捕获 → clear 栈 + 压异常 + 跳 `handler_pc` → 返回哨兵。
    ///
    /// 证明三点:(1) 运行时异常统一为 `ThrownException`、可捕获(旧 `DivideByZero` 是内部
    /// 故障、不可捕获,本测试在那条路上会原样 `Err` 而非走到 handler);(2) 同帧异常表经
    /// `find_handler` 在 dispatch 循环内捕获;(3) `catch` 超类型(`Exception`)经引导桩层次
    /// `is_instance` 命中子类(`ArithmeticException`)——层次与 catch 联动贯通。
    #[test]
    fn same_frame_catch_via_supertype() {
        use crate::classfile::attributes::ExceptionTableEntry;
        use crate::classfile::Reader;
        use crate::constant_pool::ConstantPool;
        // CP:[1]=Utf8"java/lang/Exception" [2]=Class{1}
        let mut cp_bytes = vec![0x00, 0x03]; // count=3
        cp_bytes.push(0x01); // Utf8
        let name = "java/lang/Exception";
        cp_bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        cp_bytes.extend_from_slice(name.as_bytes());
        cp_bytes.push(0x07); // Class
        cp_bytes.extend_from_slice(&1u16.to_be_bytes());
        let cp = ConstantPool::parse(&mut Reader::new(&cp_bytes)).unwrap();

        //   0 iconst_5
        //   1 iconst_0
        //   2 idiv        ← 抛 ArithmeticException(pc=2 ∈ [0,3))
        //   3 ireturn     ← 未达
        //   4 iconst_1    ← handler:压哨兵 1
        //   5 ireturn
        let code = [
            Opcode::Iconst5 as u8,
            Opcode::Iconst0 as u8,
            Opcode::Idiv as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let table = [ExceptionTableEntry {
            start_pc: 0,
            end_pc: 3,
            handler_pc: 4,
            catch_type: 2, // catch Exception(超类型,经 is_instance 命中 ArithmeticException)
        }];
        let interp = Interpreter::new(&code, &cp).with_exception_table(&table);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        let mut frame = Frame::new(0, 2);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(1)
        );
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

    // ===== Layer 3.2:long =====

    /// 运行 `code` 至 lreturn,返回 i64。
    fn run_long(code: &[u8], cp: &ConstantPool, max_stack: u16) -> i64 {
        let interp = Interpreter::new(code, cp);
        let mut frame = Frame::new(2, max_stack);
        match interp.interpret(&mut frame).unwrap() {
            Value::Long(v) => v,
            other => panic!("expected Long, got {other:?}"),
        }
    }

    /// 构造常量池 [1..]=Long(values)。long 占两个索引槽,故 count = 2n+1。
    fn cp_with_longs(values: &[i64]) -> ConstantPool {
        let count = (2 * values.len() + 1) as u16;
        let mut bytes = count.to_be_bytes().to_vec();
        for v in values {
            bytes.push(0x05); // Long tag
            bytes.extend_from_slice(&v.to_be_bytes());
        }
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn lconst_pushes_long_constants() {
        let cp = empty_cp();
        assert_eq!(
            run_long(&[Opcode::Lconst0 as u8, Opcode::Lreturn as u8], &cp, 2),
            0
        );
        assert_eq!(
            run_long(&[Opcode::Lconst1 as u8, Opcode::Lreturn as u8], &cp, 2),
            1
        );
    }

    #[test]
    fn ldc2_w_loads_long() {
        let cp = cp_with_longs(&[1_000_000_000_000]);
        let code = [Opcode::Ldc2W as u8, 0x00, 0x01, Opcode::Lreturn as u8];
        assert_eq!(run_long(&code, &cp, 2), 1_000_000_000_000);
    }

    #[test]
    fn long_load_store_round_trip() {
        let cp = empty_cp();
        // lstore 0; lload 0; lreturn,预设 local 0
        let code = [Opcode::Lstore0 as u8, Opcode::Lload0 as u8, Opcode::Lreturn as u8];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(4, 2);
        frame.operands.push_long(987_654_321_012).unwrap();
        interp.interpret(&mut frame).unwrap();
        // 再次执行:这次先存(顶上的 lconst)再读
        let code2 = [Opcode::Lconst1 as u8, Opcode::Lstore as u8, 0x02, Opcode::Lload as u8, 0x02, Opcode::Lreturn as u8];
        let interp2 = Interpreter::new(&code2, &cp);
        let mut frame2 = Frame::new(4, 2);
        assert_eq!(interp2.interpret(&mut frame2).unwrap(), Value::Long(1));
    }

    #[test]
    fn long_arithmetic_matches_java() {
        // longs 在奇数索引:[1]=100, [3]=7
        let cp = cp_with_longs(&[100, 7]);
        let a = [Opcode::Ldc2W as u8, 0, 1]; // 100
        let b = [Opcode::Ldc2W as u8, 0, 3]; // 7
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Ladd as u8, Opcode::Lreturn as u8], &cp, 4),
            107
        );
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Lsub as u8, Opcode::Lreturn as u8], &cp, 4),
            93
        );
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Lmul as u8, Opcode::Lreturn as u8], &cp, 4),
            700
        );
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Ldiv as u8, Opcode::Lreturn as u8], &cp, 4),
            14
        );
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Lrem as u8, Opcode::Lreturn as u8], &cp, 4),
            2
        );
        // lneg(5) = -5
        let cp2 = cp_with_longs(&[5]);
        assert_eq!(
            run_long(&[Opcode::Ldc2W as u8, 0, 1, Opcode::Lneg as u8, Opcode::Lreturn as u8], &cp2, 2),
            -5
        );
    }

    #[test]
    fn long_bitwise_and_shift() {
        // [1]=0xC, [3]=0xA
        let cp = cp_with_longs(&[0xC, 0xA]);
        let a = [Opcode::Ldc2W as u8, 0, 1];
        let b = [Opcode::Ldc2W as u8, 0, 3];
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Land as u8, Opcode::Lreturn as u8], &cp, 4),
            0xC & 0xA
        );
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Lor as u8, Opcode::Lreturn as u8], &cp, 4),
            0xC | 0xA
        );
        assert_eq!(
            run_long(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Lxor as u8, Opcode::Lreturn as u8], &cp, 4),
            0xC ^ 0xA
        );
        // lshl:long 值在底、int 移位量在顶。0xC << 2 = 0x30
        assert_eq!(
            run_long(&[Opcode::Ldc2W as u8, 0, 1, Opcode::Iconst2 as u8, Opcode::Lshl as u8, Opcode::Lreturn as u8], &cp, 4),
            0xC << 2
        );
        // lushr:最高位为 1 的 long 逻辑右移 1
        let cp2 = cp_with_longs(&[0x8000_0000_0000_0000u64 as i64]);
        assert_eq!(
            run_long(&[Opcode::Ldc2W as u8, 0, 1, Opcode::Iconst1 as u8, Opcode::Lushr as u8, Opcode::Lreturn as u8], &cp2, 4),
            0x4000_0000_0000_0000
        );
    }

    #[test]
    fn lcmp_pushes_int_comparison() {
        // [1]=5, [3]=3
        let cp = cp_with_longs(&[5, 3]);
        let mk = |i1: u8, i2: u8, expect: i32| {
            let code = [Opcode::Ldc2W as u8, 0, i1, Opcode::Ldc2W as u8, 0, i2, Opcode::Lcmp as u8, Opcode::Ireturn as u8];
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(0, 4);
            assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(expect));
        };
        mk(1, 1, 0); // 5 == 5
        mk(1, 3, 1); // 5 > 3
        mk(3, 1, -1); // 3 < 5
    }

    #[test]
    fn long_int_conversions() {
        // i2l:iconst_5; i2l; lreturn -> 5L
        let cp = empty_cp();
        assert_eq!(
            run_long(&[Opcode::Iconst5 as u8, Opcode::I2l as u8, Opcode::Lreturn as u8], &cp, 2),
            5
        );
        // l2i:ldc2_w(long 1_000_000_005); l2i; ireturn -> 截断为 int
        let cp2 = cp_with_longs(&[1_000_000_005]);
        let code = [Opcode::Ldc2W as u8, 0, 1, Opcode::L2i as u8, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp2);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1_000_000_005_i32));
    }

    // ===== Layer 3.2:float =====

    fn run_float(code: &[u8], cp: &ConstantPool, max_stack: u16) -> f32 {
        let interp = Interpreter::new(code, cp);
        let mut frame = Frame::new(0, max_stack);
        match interp.interpret(&mut frame).unwrap() {
            Value::Float(v) => v,
            other => panic!("expected Float, got {other:?}"),
        }
    }

    /// 构造常量池 [1..]=Float(values)。float 占 1 槽,count = n+1。
    fn cp_with_floats(values: &[f32]) -> ConstantPool {
        let count = (values.len() + 1) as u16;
        let mut bytes = count.to_be_bytes().to_vec();
        for v in values {
            bytes.push(0x04); // Float tag
            bytes.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn fconst_pushes_float_constants() {
        let cp = empty_cp();
        assert_eq!(run_float(&[Opcode::Fconst0 as u8, Opcode::Freturn as u8], &cp, 1), 0.0);
        assert_eq!(run_float(&[Opcode::Fconst1 as u8, Opcode::Freturn as u8], &cp, 1), 1.0);
        assert_eq!(run_float(&[Opcode::Fconst2 as u8, Opcode::Freturn as u8], &cp, 1), 2.0);
    }

    #[test]
    fn float_arithmetic_matches_java() {
        let cp = cp_with_floats(&[5.0, 7.0, 3.0]);
        // 1 + 2 = 3
        assert_eq!(
            run_float(&[Opcode::Fconst1 as u8, Opcode::Fconst2 as u8, Opcode::Fadd as u8, Opcode::Freturn as u8], &cp, 2),
            3.0
        );
        // 5 - 2 = 3
        assert_eq!(
            run_float(&[Opcode::Ldc as u8, 0x01, Opcode::Fconst2 as u8, Opcode::Fsub as u8, Opcode::Freturn as u8], &cp, 2),
            3.0
        );
        // 2 * 3 = 6(ldc index 3 = 3.0)
        assert_eq!(
            run_float(&[Opcode::Fconst2 as u8, Opcode::Ldc as u8, 0x03, Opcode::Fmul as u8, Opcode::Freturn as u8], &cp, 2),
            6.0
        );
        // 7 / 2 = 3.5
        assert_eq!(
            run_float(&[Opcode::Ldc as u8, 0x02, Opcode::Fconst2 as u8, Opcode::Fdiv as u8, Opcode::Freturn as u8], &cp, 2),
            3.5
        );
        // 7 % 3 = 1(ldc index 2=7.0, index 3=3.0)
        assert_eq!(
            run_float(&[Opcode::Ldc as u8, 0x02, Opcode::Ldc as u8, 0x03, Opcode::Frem as u8, Opcode::Freturn as u8], &cp, 2),
            1.0
        );
        // -(1) = -1
        assert_eq!(
            run_float(&[Opcode::Fconst1 as u8, Opcode::Fneg as u8, Opcode::Freturn as u8], &cp, 1),
            -1.0
        );
    }

    #[test]
    fn float_compare_pushes_int() {
        let cp = cp_with_floats(&[3.0, 5.0, f32::NAN]);
        let mk = |i1: u8, i2: u8, op: Opcode, expect: i32| {
            let code = [Opcode::Ldc as u8, i1, Opcode::Ldc as u8, i2, op as u8, Opcode::Ireturn as u8];
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(0, 2);
            assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(expect));
        };
        mk(1, 2, Opcode::Fcmpl, -1); // 3 < 5
        mk(2, 1, Opcode::Fcmpl, 1); // 5 > 3
        mk(1, 1, Opcode::Fcmpl, 0); // 3 == 3
        // NaN:fcmpl → -1,fcmpg → 1
        mk(3, 1, Opcode::Fcmpl, -1);
        mk(3, 1, Opcode::Fcmpg, 1);
    }

    #[test]
    fn float_int_conversions() {
        // i2f:iconst_5; i2f; freturn -> 5.0
        let cp = empty_cp();
        assert_eq!(
            run_float(&[Opcode::Iconst5 as u8, Opcode::I2f as u8, Opcode::Freturn as u8], &cp, 1),
            5.0
        );
        // f2i:ldc(3.7); f2i; ireturn -> 3(向零截断)
        let cp2 = cp_with_floats(&[3.7]);
        let code = [Opcode::Ldc as u8, 0x01, Opcode::F2i as u8, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp2);
        let mut frame = Frame::new(0, 1);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(3));
    }

    // ===== Layer 3.2:double =====

    fn run_double(code: &[u8], cp: &ConstantPool, max_stack: u16) -> f64 {
        let interp = Interpreter::new(code, cp);
        let mut frame = Frame::new(0, max_stack);
        match interp.interpret(&mut frame).unwrap() {
            Value::Double(v) => v,
            other => panic!("expected Double, got {other:?}"),
        }
    }

    /// 构造常量池 [1..]=Double(values)。double 占 2 槽,count = 2n+1。
    fn cp_with_doubles(values: &[f64]) -> ConstantPool {
        let count = (2 * values.len() + 1) as u16;
        let mut bytes = count.to_be_bytes().to_vec();
        for v in values {
            bytes.push(0x06); // Double tag
            bytes.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn dconst_pushes_double_constants() {
        let cp = empty_cp();
        assert_eq!(run_double(&[Opcode::Dconst0 as u8, Opcode::Dreturn as u8], &cp, 2), 0.0);
        assert_eq!(run_double(&[Opcode::Dconst1 as u8, Opcode::Dreturn as u8], &cp, 2), 1.0);
    }

    #[test]
    fn double_arithmetic_matches_java() {
        // [1]=5.0, [3]=2.5
        let cp = cp_with_doubles(&[5.0, 2.5]);
        let a = [Opcode::Ldc2W as u8, 0, 1];
        let b = [Opcode::Ldc2W as u8, 0, 3];
        assert_eq!(
            run_double(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Dadd as u8, Opcode::Dreturn as u8], &cp, 4),
            7.5
        );
        assert_eq!(
            run_double(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Dsub as u8, Opcode::Dreturn as u8], &cp, 4),
            2.5
        );
        assert_eq!(
            run_double(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Dmul as u8, Opcode::Dreturn as u8], &cp, 4),
            12.5
        );
        assert_eq!(
            run_double(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Ddiv as u8, Opcode::Dreturn as u8], &cp, 4),
            2.0
        );
        assert_eq!(
            run_double(&[a[0], a[1], a[2], b[0], b[1], b[2], Opcode::Drem as u8, Opcode::Dreturn as u8], &cp, 4),
            5.0 % 2.5
        );
        // dneg(2.5) = -2.5
        assert_eq!(
            run_double(&[Opcode::Ldc2W as u8, 0, 3, Opcode::Dneg as u8, Opcode::Dreturn as u8], &cp, 2),
            -2.5
        );
    }

    #[test]
    fn double_compare_pushes_int() {
        let cp = cp_with_doubles(&[3.0, 5.0, f64::NAN]);
        let mk = |i1: u8, i2: u8, op: Opcode, expect: i32| {
            let code = [Opcode::Ldc2W as u8, 0, i1, Opcode::Ldc2W as u8, 0, i2, op as u8, Opcode::Ireturn as u8];
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(0, 4);
            assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(expect));
        };
        mk(1, 3, Opcode::Dcmpl, -1); // 3 < 5
        mk(3, 1, Opcode::Dcmpl, 1); // 5 > 3
        mk(1, 1, Opcode::Dcmpl, 0); // 3 == 3
        mk(5, 1, Opcode::Dcmpl, -1); // NaN → -1
        mk(5, 1, Opcode::Dcmpg, 1); // NaN → 1
    }

    #[test]
    fn double_int_conversions() {
        // i2d:iconst_5; i2d; dreturn -> 5.0
        let cp = empty_cp();
        assert_eq!(
            run_double(&[Opcode::Iconst5 as u8, Opcode::I2d as u8, Opcode::Dreturn as u8], &cp, 2),
            5.0
        );
        // d2i:ldc2_w(3.9); d2i; ireturn -> 3(向零截断)
        let cp2 = cp_with_doubles(&[3.9]);
        let code = [Opcode::Ldc2W as u8, 0, 1, Opcode::D2i as u8, Opcode::Ireturn as u8];
        let interp = Interpreter::new(&code, &cp2);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(3));
    }

    // ===== Layer 3.2:跨类型转换 =====

    #[test]
    fn long_to_float_double() {
        let cp = cp_with_longs(&[1_000_000]);
        assert_eq!(
            run_float(&[Opcode::Ldc2W as u8, 0, 1, Opcode::L2f as u8, Opcode::Freturn as u8], &cp, 2),
            1_000_000.0
        );
        let cp2 = cp_with_longs(&[1_000_000_000_000]);
        assert_eq!(
            run_double(&[Opcode::Ldc2W as u8, 0, 1, Opcode::L2d as u8, Opcode::Dreturn as u8], &cp2, 2),
            1_000_000_000_000.0
        );
    }

    #[test]
    fn float_to_long_double() {
        let cp = cp_with_floats(&[3.9]);
        // f2l:3.9 -> 3(向零截断)
        let interp = Interpreter::new(&[Opcode::Ldc as u8, 0x01, Opcode::F2l as u8, Opcode::Lreturn as u8], &cp);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Long(3));
        // f2d:2.5 -> 2.5
        let cp2 = cp_with_floats(&[2.5]);
        assert_eq!(
            run_double(&[Opcode::Ldc as u8, 0x01, Opcode::F2d as u8, Opcode::Dreturn as u8], &cp2, 2),
            2.5
        );
    }

    #[test]
    fn double_to_long_float() {
        let cp = cp_with_doubles(&[3.9]);
        // d2l:3.9 -> 3
        let interp = Interpreter::new(&[Opcode::Ldc2W as u8, 0, 1, Opcode::D2l as u8, Opcode::Lreturn as u8], &cp);
        let mut frame = Frame::new(0, 2);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Long(3));
        // d2f:2.5 -> 2.5
        let cp2 = cp_with_doubles(&[2.5]);
        assert_eq!(
            run_float(&[Opcode::Ldc2W as u8, 0, 1, Opcode::D2f as u8, Opcode::Freturn as u8], &cp2, 2),
            2.5
        );
    }

    #[test]
    fn int_narrowing_conversions() {
        let cp = cp_with_int(200); // ldc index 1 = 200
        // i2b:200 的低 8 位符号扩展 -> -56
        let interp = Interpreter::new(&[Opcode::Ldc as u8, 0x01, Opcode::I2b as u8, Opcode::Ireturn as u8], &cp);
        let mut frame = Frame::new(0, 1);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(-56)); // 200 低 8 位符号扩展

        // i2c:-1 的低 16 位零扩展 -> 65535
        let cp2 = empty_cp();
        let interp2 = Interpreter::new(&[Opcode::IconstM1 as u8, Opcode::I2c as u8, Opcode::Ireturn as u8], &cp2);
        let mut frame2 = Frame::new(0, 1);
        assert_eq!(interp2.interpret(&mut frame2).unwrap(), Value::Int(0xFFFF));

        // i2s:0x8000(32768) 低 16 位符号扩展 -> -32768
        let cp3 = cp_with_int(0x8000);
        let interp3 = Interpreter::new(&[Opcode::Ldc as u8, 0x01, Opcode::I2s as u8, Opcode::Ireturn as u8], &cp3);
        let mut frame3 = Frame::new(0, 1);
        assert_eq!(interp3.interpret(&mut frame3).unwrap(), Value::Int(-32768));
    }

    // ===== Layer 4.3a:数组分配 =====

    #[test]
    fn newarray_int_defaults_zero_and_length() {
        // bipush 3; newarray int(10); dup; arraylength; ireturn -> 3
        let code = [
            Opcode::Bipush as u8, 0x03,
            Opcode::Newarray as u8, 10, // atype=int
            Opcode::Dup as u8,
            Opcode::Arraylength as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(0, 2);
        let mut vm = Vm::default();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(3)
        );
    }

    #[test]
    fn newarray_negative_count_is_negativearraysize() {
        // iconst_m1; newarray int -> NegativeArraySize
        let code = [Opcode::IconstM1 as u8, Opcode::Newarray as u8, 10];
        let cp = empty_cp();
        let mut frame = Frame::new(0, 1);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        let interp = Interpreter::new(&code, &cp);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/NegativeArraySizeException",
        );
    }

    #[test]
    fn arraylength_on_null_is_nullpointer() {
        let code = [
            Opcode::AconstNull as u8,
            Opcode::Arraylength as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(0, 1);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        let interp = Interpreter::new(&code, &cp);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/NullPointerException",
        );
    }

    // ===== Layer 4.3a:数组加载 =====

    #[test]
    fn baload_sign_extends_byte() {
        use crate::oops::{ArrayOop, Oop};
        use crate::runtime::Slot;
        // 元素存 Int(200),baload 符号扩展 -> (200 as i8) as i32 = -56。
        let mut vm = Vm::default();
        let arr = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
            "[B".to_string(),
            vec![Slot::Int(200)],
        )));
        // aload_0(引用); iconst_0(下标); baload; ireturn
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Iconst0 as u8,
            Opcode::Baload as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, arr).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(-56)
        );
    }

    #[test]
    fn caload_zero_extends_char() {
        use crate::oops::{ArrayOop, Oop};
        use crate::runtime::Slot;
        // 存 Int(0xFFFF),caload 零扩展 -> (0xFFFF as u16) as i32 = 65535。
        let mut vm = Vm::default();
        let arr = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
            "[C".to_string(),
            vec![Slot::Int(0xFFFF)],
        )));
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Iconst0 as u8,
            Opcode::Caload as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, arr).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(65535)
        );
    }

    #[test]
    fn array_load_out_of_bounds_is_aioobe() {
        use crate::oops::{ArrayOop, Oop};
        use crate::runtime::Slot;
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        let arr = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
            "[I".to_string(),
            vec![Slot::Int(0)],
        ))); // len 1
        // 下标 5 越界
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Iconst5 as u8,
            Opcode::Iaload as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, arr).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/ArrayIndexOutOfBoundsException",
        );
    }

    // ===== Layer 4.3a:数组存储 =====

    #[test]
    fn iastore_then_iaload_round_trip() {
        // iconst_2(count); newarray int; astore_0(arr);
        // aload_0; iconst_1(idx); bipush 42(val); iastore;
        // aload_0; iconst_1; iaload; ireturn -> 42
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Newarray as u8, 10,
            Opcode::Astore0 as u8,
            Opcode::Aload0 as u8,
            Opcode::Iconst1 as u8,
            Opcode::Bipush as u8, 0x2A,
            Opcode::Iastore as u8,
            Opcode::Aload0 as u8,
            Opcode::Iconst1 as u8,
            Opcode::Iaload as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 3);
        let mut vm = Vm::default();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(42)
        );
    }

    #[test]
    fn bastore_baload_sign_extension() {
        // iconst_1; newarray byte(8); astore_0;
        // aload_0; iconst_0; sipush 200; bastore;
        // aload_0; iconst_0; baload; ireturn -> -56
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::Newarray as u8, 8,
            Opcode::Astore0 as u8,
            Opcode::Aload0 as u8,
            Opcode::Iconst0 as u8,
            Opcode::Sipush as u8, 0x00, 0xC8,
            Opcode::Bastore as u8,
            Opcode::Aload0 as u8,
            Opcode::Iconst0 as u8,
            Opcode::Baload as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 3);
        let mut vm = Vm::default();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(-56)
        );
    }

    #[test]
    fn iastore_out_of_bounds_is_aioobe() {
        // int[1]; 存到 index 5 越界 -> AIOOBE
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::Newarray as u8, 10,
            Opcode::Astore0 as u8,
            Opcode::Aload0 as u8,
            Opcode::Iconst5 as u8,
            Opcode::Bipush as u8, 0x09,
            Opcode::Iastore as u8,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 3);
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        let interp = Interpreter::new(&code, &cp);
        assert_throws(
            interp.interpret_with(&mut frame, &mut vm),
            &vm,
            "java/lang/ArrayIndexOutOfBoundsException",
        );
    }

    // ===== Layer 4.4:控制流(引用分支)=====

    #[test]
    fn ifnull_branches_on_null() {
        use crate::runtime::Reference;
        // local0 = null; aload_0; ifnull +7; iconst_0; ireturn; iconst_1; ireturn
        // null → 跳到 iconst_1 → 返回 1
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Ifnull as u8, 0x00, 0x05,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 1);
        frame.locals.set_reference(0, Reference::null()).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    #[test]
    fn ifnonnull_branches_on_nonnull() {
        use crate::runtime::Reference;
        // local0 = 非空引用; aload_0; ifnonnull +7 → iconst_1
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Ifnonnull as u8, 0x00, 0x05,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 1);
        frame.locals.set_reference(0, Reference::from_id(5)).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    #[test]
    fn if_acmpeq_equal_references_jumps() {
        use crate::runtime::Reference;
        // 同一引用; aload_0; aload_1; if_acmpeq +8 → iconst_1
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Aload1 as u8,
            Opcode::IfAcmpeq as u8, 0x00, 0x05,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(2, 2);
        frame.locals.set_reference(0, Reference::from_id(9)).unwrap();
        frame.locals.set_reference(1, Reference::from_id(9)).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    #[test]
    fn if_acmpne_distinct_references_jumps() {
        use crate::runtime::Reference;
        // 不同引用; if_acmpne 跳
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Aload1 as u8,
            Opcode::IfAcmpne as u8, 0x00, 0x05,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(2, 2);
        frame.locals.set_reference(0, Reference::from_id(1)).unwrap();
        frame.locals.set_reference(1, Reference::from_id(2)).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    // ===== Layer 4.4:goto_w =====

    #[test]
    fn goto_w_unconditionally_jumps() {
        // iconst_1; goto_w +6(4 字节,相对 opcode pc=1 → 落点 7=ireturn); iconst_2(跳过) -> 1
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::GotoW as u8, 0x00, 0x00, 0x00, 0x06,
            Opcode::Iconst2 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(0, 1);
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    // ===== Layer 4.4:tableswitch / lookupswitch =====

    /// 构造 tableswitch:iload_0 取 index,low=0 high=2;
    /// 命中 0→1, 1→2, 2→3,越界→0。
    fn tableswitch_code() -> Vec<u8> {
        let mut c = vec![Opcode::Iload0 as u8]; // index 从 local0
        let sw = c.len();                       // switch opcode 地址
        c.push(Opcode::Tableswitch as u8);
        let pad = (4 - ((sw + 1) % 4)) % 4;
        c.extend(std::iter::repeat_n(0u8, pad));
        // 落点紧跟数据之后;先记录预期绝对下标,再算相对偏移。
        // 数据 = default/low/high(3×4) + 3×offset(12) = 24 字节
        let data_start = c.len();
        c.extend_from_slice(&[0u8; 24]); // 占位,稍后回填
        let targets_base = c.len(); // 落点起始
        let at = |n: usize| targets_base + n; // 绝对下标
        c.push(Opcode::Iconst0 as u8); c.push(Opcode::Ireturn as u8); // default
        c.push(Opcode::Iconst1 as u8); c.push(Opcode::Ireturn as u8); // idx 0
        c.push(Opcode::Iconst2 as u8); c.push(Opcode::Ireturn as u8); // idx 1
        c.push(Opcode::Iconst3 as u8); c.push(Opcode::Ireturn as u8); // idx 2
        // 回填
        let default_off = (at(0) - sw) as i32;
        let low: i32 = 0;
        let high: i32 = 2;
        let j0 = (at(2) - sw) as i32;
        let j1 = (at(4) - sw) as i32;
        let j2 = (at(6) - sw) as i32;
        c[data_start..data_start + 4].copy_from_slice(&default_off.to_be_bytes());
        c[data_start + 4..data_start + 8].copy_from_slice(&low.to_be_bytes());
        c[data_start + 8..data_start + 12].copy_from_slice(&high.to_be_bytes());
        c[data_start + 12..data_start + 16].copy_from_slice(&j0.to_be_bytes());
        c[data_start + 16..data_start + 20].copy_from_slice(&j1.to_be_bytes());
        c[data_start + 20..data_start + 24].copy_from_slice(&j2.to_be_bytes());
        c
    }

    #[test]
    fn tableswitch_hits_each_slot() {
        let cp = empty_cp();
        for (idx, expect) in [(0, 1i32), (1, 2), (2, 3)] {
            let code = tableswitch_code();
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(1, 1);
            frame.locals.set_int(0, idx).unwrap();
            assert_eq!(
                interp.interpret(&mut frame).unwrap(),
                Value::Int(expect),
                "index {idx}"
            );
        }
    }

    #[test]
    fn tableswitch_out_of_range_hits_default() {
        let code = tableswitch_code();
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(1, 1);
        frame.locals.set_int(0, 99).unwrap();
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(0));
    }

    /// 构造 lookupswitch:稀疏 key=10→1, key=20→2,未命中→0。
    fn lookupswitch_code() -> Vec<u8> {
        let mut c = vec![Opcode::Iload0 as u8];
        let sw = c.len();
        c.push(Opcode::Lookupswitch as u8);
        let pad = (4 - ((sw + 1) % 4)) % 4;
        c.extend(std::iter::repeat_n(0u8, pad));
        let data_start = c.len();
        // default(4)+npairs(4)+2×(match,offset)(16)=24
        c.extend_from_slice(&[0u8; 24]);
        let targets_base = c.len();
        let at = |n: usize| targets_base + n;
        c.push(Opcode::Iconst0 as u8); c.push(Opcode::Ireturn as u8); // default
        c.push(Opcode::Iconst1 as u8); c.push(Opcode::Ireturn as u8); // key 10
        c.push(Opcode::Iconst2 as u8); c.push(Opcode::Ireturn as u8); // key 20
        let default_off = (at(0) - sw) as i32;
        let npairs: i32 = 2;
        let m0 = 10i32;
        let o0 = (at(2) - sw) as i32;
        let m1 = 20i32;
        let o1 = (at(4) - sw) as i32;
        c[data_start..data_start + 4].copy_from_slice(&default_off.to_be_bytes());
        c[data_start + 4..data_start + 8].copy_from_slice(&npairs.to_be_bytes());
        c[data_start + 8..data_start + 12].copy_from_slice(&m0.to_be_bytes());
        c[data_start + 12..data_start + 16].copy_from_slice(&o0.to_be_bytes());
        c[data_start + 16..data_start + 20].copy_from_slice(&m1.to_be_bytes());
        c[data_start + 20..data_start + 24].copy_from_slice(&o1.to_be_bytes());
        c
    }

    #[test]
    fn lookupswitch_matches_key() {
        let cp = empty_cp();
        for (key, expect) in [(10, 1i32), (20, 2)] {
            let code = lookupswitch_code();
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(1, 1);
            frame.locals.set_int(0, key).unwrap();
            assert_eq!(
                interp.interpret(&mut frame).unwrap(),
                Value::Int(expect),
                "key {key}"
            );
        }
    }

    #[test]
    fn lookupswitch_unmatched_hits_default() {
        let code = lookupswitch_code();
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(1, 1);
        frame.locals.set_int(0, 999).unwrap();
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(0));
    }
}
