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

/// `*aload`:弹 index、弹 arrayref,null + 越界检查,按种类压值(byte/char/short 扩展)。
pub(super) fn array_load(
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    kind: ArrayKind,
) -> Result<(), VmError> {
    let index = frame.operands.pop_int()?;
    let arrayref = frame.operands.pop_reference()?;
    if arrayref.is_null() {
        return Err(VmError::NullPointer);
    }
    let slot = match vm
        .heap()
        .get(arrayref)
        .ok_or(VmError::BadConstant("aload 引用悬空"))?
    {
        Oop::Array(a) => {
            if index < 0 || (index as usize) >= a.length() {
                return Err(VmError::ArrayIndexOutOfBounds);
            }
            a.element(index as usize)
        }
        Oop::Instance(_) => return Err(VmError::BadConstant("aload 目标非数组")),
    };
    push_array_value(frame, kind, slot)
}

/// 按种类把槽值压回操作数栈(byte/char/short 在此扩展;类型不符报错)。
fn push_array_value(frame: &mut Frame, kind: ArrayKind, slot: Slot) -> Result<(), VmError> {
    match kind {
        ArrayKind::Int => {
            let Slot::Int(v) = slot else {
                return Err(VmError::BadConstant("iaload 元素非 int"));
            };
            frame.operands.push_int(v)?;
        }
        ArrayKind::Long => {
            let Slot::Long(v) = slot else {
                return Err(VmError::BadConstant("laload 元素非 long"));
            };
            frame.operands.push_long(v)?;
        }
        ArrayKind::Float => {
            let Slot::Float(v) = slot else {
                return Err(VmError::BadConstant("faload 元素非 float"));
            };
            frame.operands.push_float(v)?;
        }
        ArrayKind::Double => {
            let Slot::Double(v) = slot else {
                return Err(VmError::BadConstant("daload 元素非 double"));
            };
            frame.operands.push_double(v)?;
        }
        ArrayKind::Ref => {
            let Slot::Reference(r) = slot else {
                return Err(VmError::BadConstant("aaload 元素非引用"));
            };
            frame.operands.push_reference(r)?;
        }
        ArrayKind::Byte => {
            let Slot::Int(v) = slot else {
                return Err(VmError::BadConstant("baload 元素非 int 槽"));
            };
            frame.operands.push_int((v as i8) as i32)?;
        }
        ArrayKind::Char => {
            let Slot::Int(v) = slot else {
                return Err(VmError::BadConstant("caload 元素非 int 槽"));
            };
            frame.operands.push_int((v as u16) as i32)?;
        }
        ArrayKind::Short => {
            let Slot::Int(v) = slot else {
                return Err(VmError::BadConstant("saload 元素非 int 槽"));
            };
            frame.operands.push_int((v as i16) as i32)?;
        }
    }
    Ok(())
}

/// `*astore`:弹 value、弹 index、弹 arrayref,null + 越界检查后写。
/// byte/char/short 存原始 int(扩展统一推迟到加载侧,与符号/零扩展等价)。
pub(super) fn array_store(
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    kind: ArrayKind,
) -> Result<(), VmError> {
    let value = pop_array_value(frame, kind)?;
    let index = frame.operands.pop_int()?;
    let arrayref = frame.operands.pop_reference()?;
    if arrayref.is_null() {
        return Err(VmError::NullPointer);
    }
    let idx = if index < 0 {
        return Err(VmError::ArrayIndexOutOfBounds);
    } else {
        index as usize
    };
    match vm
        .heap_mut()
        .get_mut(arrayref)
        .ok_or(VmError::BadConstant("astore 引用悬空"))?
    {
        Oop::Array(a) => {
            if idx >= a.length() {
                return Err(VmError::ArrayIndexOutOfBounds);
            }
            a.set_element(idx, value);
        }
        Oop::Instance(_) => return Err(VmError::BadConstant("astore 目标非数组")),
    }
    Ok(())
}

/// 按种类弹栈取值(byte/char/short 取 int;cat-2 取 long/double)。
fn pop_array_value(frame: &mut Frame, kind: ArrayKind) -> Result<Slot, VmError> {
    Ok(match kind {
        ArrayKind::Int | ArrayKind::Byte | ArrayKind::Char | ArrayKind::Short => {
            Slot::Int(frame.operands.pop_int()?)
        }
        ArrayKind::Long => Slot::Long(frame.operands.pop_long()?),
        ArrayKind::Float => Slot::Float(frame.operands.pop_float()?),
        ArrayKind::Double => Slot::Double(frame.operands.pop_double()?),
        ArrayKind::Ref => Slot::Reference(frame.operands.pop_reference()?),
    })
}
