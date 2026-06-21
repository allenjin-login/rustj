//! 数组指令:`newarray` / `anewarray` / `arraylength` 及加载/存储。
//!
//! 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_newarray)` / `CASE(_anewarray)` /
//! `CASE(_arraylength)` / `CASE(_iaload)` … `CASE(_sastore)`。元素类型由指令决定,
//! [`ArrayOop`] 统一存 `Vec<Slot>`(不记组件类型;4.3a 不做 ArrayStoreException)。

use super::field::resolve_class_name;
use super::{Interpreter, VmError};
use crate::oops::{ArrayOop, Oop};
use crate::runtime::{Frame, Reference, Slot, Vm};

/// 数组访问种类:把 8 条加载 / 8 条存储各收敛到单函数。
#[allow(dead_code)] // Byte/Char/Short/Long/... 在加载/存储任务接入前暂未全用
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ArrayKind {
    Int,
    Long,
    Float,
    Double,
    Ref,
    Byte,
    Char,
    Short,
}

/// `newarray`:弹 count,按 atype 造默认元素数组,入堆,压引用。
pub(super) fn new_array(frame: &mut Frame, vm: &mut Vm<'_>, atype: u8) -> Result<(), VmError> {
    let count = frame.operands.pop_int()?;
    if count < 0 {
        return Err(VmError::NegativeArraySize);
    }
    let default = match atype {
        4 | 5 | 8 | 9 | 10 => Slot::Int(0), // boolean/char/byte/short/int
        6 => Slot::Float(0.0),
        7 => Slot::Double(0.0),
        11 => Slot::Long(0),
        _ => return Err(VmError::BadConstant("newarray 非法 atype")),
    };
    let elements = vec![default; count as usize];
    let r = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(elements)));
    frame.operands.push_reference(r)?;
    Ok(())
}

/// `anewarray`:解析 Class(校验组件类型;4.3a 不存储),弹 count,造 null 引用数组,入堆,压引用。
pub(super) fn a_new_array(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    class_index: u16,
) -> Result<(), VmError> {
    let _component = resolve_class_name(interp.cp(), class_index)?;
    let count = frame.operands.pop_int()?;
    if count < 0 {
        return Err(VmError::NegativeArraySize);
    }
    let elements = vec![Slot::Reference(Reference::null()); count as usize];
    let r = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(elements)));
    frame.operands.push_reference(r)?;
    Ok(())
}

/// `arraylength`:弹 arrayref,null 检查,压长度。
pub(super) fn array_length(frame: &mut Frame, vm: &mut Vm<'_>) -> Result<(), VmError> {
    let arrayref = frame.operands.pop_reference()?;
    if arrayref.is_null() {
        return Err(VmError::NullPointer);
    }
    let len = match vm
        .heap()
        .get(arrayref)
        .ok_or(VmError::BadConstant("arraylength 引用悬空"))?
    {
        Oop::Array(a) => a.length(),
        Oop::Instance(_) => return Err(VmError::BadConstant("arraylength 目标非数组")),
    };
    frame.operands.push_int(len as i32)?;
    Ok(())
}
