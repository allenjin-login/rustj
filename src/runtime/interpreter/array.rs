//! 数组指令:`newarray` / `anewarray` / `arraylength` 及加载/存储。
//!
//! 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_newarray)` / `CASE(_anewarray)` /
//! `CASE(_arraylength)` / `CASE(_iaload)` … `CASE(_sastore)`。元素类型由指令决定,
//! [`ArrayOop`] 统一存 `Vec<Slot>`(不记组件类型;4.3a 不做 ArrayStoreException)。

use super::field::resolve_class_name;
use super::{throw_exception, Interpreter, VmError};
use crate::oops::{ArrayOop, Oop};
use crate::runtime::{Frame, Reference, Slot, VmThread};

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
pub(super) fn new_array(frame: &mut Frame, vm: &mut VmThread, atype: u8) -> Result<(), VmError> {
    let count = frame.operands.pop_int()?;
    if count < 0 {
        return Err(throw_exception(vm, "java/lang/NegativeArraySizeException"));
    }
    // atype(JVMS Table 6.5.newarray)→(默认槽, 数组描述符)。描述符即运行时类型,供 checkcast。
    let (default, desc) = match atype {
        4 => (Slot::Int(0), "[Z"), // boolean
        5 => (Slot::Int(0), "[C"), // char
        6 => (Slot::Float(0.0), "[F"), // float
        7 => (Slot::Double(0.0), "[D"), // double
        8 => (Slot::Int(0), "[B"), // byte
        9 => (Slot::Int(0), "[S"), // short
        10 => (Slot::Int(0), "[I"), // int
        11 => (Slot::Long(0), "[J"), // long
        _ => return Err(VmError::BadConstant("newarray 非法 atype")),
    };
    let elements = vec![default; count as usize];
    let r = vm
        .heap_mut()
        .alloc(Oop::Array(ArrayOop::new(desc.to_string(), elements)));
    frame.operands.push_reference(r)?;
    Ok(())
}

/// 组件内部名(类名或数组描述符)→ 数组描述符。`java/lang/String` → `[Ljava/lang/String;`;
/// `[I` → `[[I`(数组组件前补 `[`)。对应 HotSpot `arrayKlass` 之名合成。
fn array_descriptor(component: &str) -> String {
    if component.starts_with('[') {
        format!("[{component}")
    } else {
        format!("[L{component};")
    }
}

/// `anewarray`:解析 Class(取组件名),弹 count,造 null 引用数组,入堆,压引用。
pub(super) fn a_new_array(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut VmThread,
    class_index: u16,
) -> Result<(), VmError> {
    let component = resolve_class_name(interp.cp(), class_index)?;
    let count = frame.operands.pop_int()?;
    if count < 0 {
        return Err(throw_exception(vm, "java/lang/NegativeArraySizeException"));
    }
    let elements = vec![Slot::Reference(Reference::null()); count as usize];
    let r = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
        array_descriptor(&component),
        elements,
    )));
    frame.operands.push_reference(r)?;
    Ok(())
}

/// 解析数组类型描述符(`[[I` 等)→ (总维数, 叶子默认槽)。
/// 组件类型决定叶子默认值(基本零值 / 引用 null);非数组描述符报错。
fn parse_array_descriptor(desc: &str) -> Result<(usize, Slot), VmError> {
    let b = desc.as_bytes();
    let mut ndim = 0;
    while ndim < b.len() && b[ndim] == b'[' {
        ndim += 1;
    }
    if ndim == 0 {
        return Err(VmError::BadConstant("multianewarray 描述符非数组"));
    }
    let base = match b.get(ndim) {
        Some(b'I' | b'Z' | b'B' | b'C' | b'S') => Slot::Int(0),
        Some(b'J') => Slot::Long(0),
        Some(b'F') => Slot::Float(0.0),
        Some(b'D') => Slot::Double(0.0),
        Some(b'L') => Slot::Reference(Reference::null()),
        _ => return Err(VmError::BadConstant("multianewarray 非法组件类型")),
    };
    Ok((ndim, base))
}

/// 递归分配嵌套数组树。`counts[depth]` 为当前层长度。
/// 最后一层:`dims == ndim` 填叶子默认值;`dims < ndim` 填 null(余下维度未分配)。
/// `desc` 为全描述符(如 `[[I`);本层(第 `depth` 级)描述符 = 去掉 depth 个前导 `[`。
fn alloc_multi(
    vm: &mut VmThread,
    counts: &[i32],
    depth: usize,
    ndim: usize,
    base: Slot,
    desc: &str,
) -> Result<Reference, VmError> {
    let len = counts[depth] as usize;
    let last = depth + 1 == counts.len();
    let mut elements = Vec::with_capacity(len);
    for _ in 0..len {
        if last {
            if counts.len() < ndim {
                elements.push(Slot::Reference(Reference::null()));
            } else {
                elements.push(base);
            }
        } else {
            let child = alloc_multi(vm, counts, depth + 1, ndim, base, desc)?;
            elements.push(Slot::Reference(child));
        }
    }
    // 外层(depth=0)取全描述符;每深入一级剥掉一个前导 '['([[I → [I)。
    let this_desc = desc.get(depth..).unwrap_or(desc);
    Ok(vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
        this_desc.to_string(),
        elements,
    ))))
}

/// `multianewarray`:解析描述符 → 弹 dims 个 count → 递归分配 → 压外层引用。
pub(super) fn multi_new_array(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut VmThread,
    class_index: u16,
    dims: u8,
) -> Result<(), VmError> {
    let name = resolve_class_name(interp.cp(), class_index)?;
    let (ndim, base) = parse_array_descriptor(&name)?;
    if dims == 0 || dims as usize > ndim {
        return Err(VmError::BadConstant("multianewarray dims 与 ndim 不符"));
    }
    let mut counts: Vec<i32> = Vec::with_capacity(dims as usize);
    for _ in 0..dims {
        counts.push(frame.operands.pop_int()?);
    }
    counts.reverse(); // counts[0] = 最外层
    if counts.iter().any(|&c| c < 0) {
        return Err(throw_exception(vm, "java/lang/NegativeArraySizeException"));
    }
    let r = alloc_multi(vm, &counts, 0, ndim, base, &name)?;
    frame.operands.push_reference(r)?;
    Ok(())
}

/// `arraylength`:弹 arrayref,null 检查,压长度。
pub(super) fn array_length(frame: &mut Frame, vm: &mut VmThread) -> Result<(), VmError> {
    let arrayref = frame.operands.pop_reference()?;
    if arrayref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    let len = match vm
        .heap()
        .get(arrayref)
        .ok_or(VmError::BadConstant("arraylength 引用悬空"))?
    {
        Oop::Array(a) => a.length(),
        Oop::Instance(_) | Oop::Lambda(_) => {
            return Err(VmError::BadConstant("arraylength 目标非数组"))
        }
    };
    frame.operands.push_int(len as i32)?;
    Ok(())
}

/// `*aload`:弹 index、弹 arrayref,null + 越界检查,按种类压值(byte/char/short 扩展)。
pub(super) fn array_load(
    frame: &mut Frame,
    vm: &mut VmThread,
    kind: ArrayKind,
) -> Result<(), VmError> {
    let index = frame.operands.pop_int()?;
    let arrayref = frame.operands.pop_reference()?;
    if arrayref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    // 先取长度(借用释放后再抛 AIOOBE),再取元素——避免 `&mut vm`(抛异常)与
    // `&Oop`(取元素)的借用冲突。
    let length = match vm
        .heap()
        .get(arrayref)
        .ok_or(VmError::BadConstant("aload 引用悬空"))?
    {
        Oop::Array(a) => a.length(),
        Oop::Instance(_) | Oop::Lambda(_) => {
            return Err(VmError::BadConstant("aload 目标非数组"))
        }
    };
    if index < 0 || (index as usize) >= length {
        return Err(throw_exception(
            vm,
            "java/lang/ArrayIndexOutOfBoundsException",
        ));
    }
    let slot = match vm
        .heap()
        .get(arrayref)
        .ok_or(VmError::BadConstant("aload 引用悬空"))?
    {
        Oop::Array(a) => a.element(index as usize),
        Oop::Instance(_) | Oop::Lambda(_) => {
            return Err(VmError::BadConstant("aload 目标非数组"))
        }
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
    vm: &mut VmThread,
    kind: ArrayKind,
) -> Result<(), VmError> {
    let value = pop_array_value(frame, kind)?;
    let index = frame.operands.pop_int()?;
    let arrayref = frame.operands.pop_reference()?;
    if arrayref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    if index < 0 {
        return Err(throw_exception(
            vm,
            "java/lang/ArrayIndexOutOfBoundsException",
        ));
    }
    let idx = index as usize;
    // 先以不可变借用查长度(释放后再抛 AIOOBE),再以可变借用写——避免 `&mut vm`
    // (抛异常)与 `&mut Oop`(写)的借用冲突。
    let length = match vm
        .heap()
        .get(arrayref)
        .ok_or(VmError::BadConstant("astore 引用悬空"))?
    {
        Oop::Array(a) => a.length(),
        Oop::Instance(_) | Oop::Lambda(_) => {
            return Err(VmError::BadConstant("astore 目标非数组"))
        }
    };
    if idx >= length {
        return Err(throw_exception(
            vm,
            "java/lang/ArrayIndexOutOfBoundsException",
        ));
    }
    // 引用数组(aastore)组件类型可赋性检查:非 null 元素须可赋给数组组件类型,否则
    // ArrayStoreException(HotSpot `ObjArrayKlass::array_store`,4.10i 延后项,本层落地)。
    // 基本数组种类由 `pop_array_value` 的槽类型保证,不走此查。读/判/写分时:不可变借读组件
    // + 判定收敛进块(释放)后再以 `&mut vm` 抛 ASE / 写入,镜像 `arraycopy::copy_elements`。
    if let ArrayKind::Ref = kind
        && let Slot::Reference(elem) = &value
        && !elem.is_null()
    {
        let not_assignable = {
            let Some(reg) = vm.registry() else {
                return Err(VmError::BadConstant("aastore 组件可赋性检查需类注册表"));
            };
            let array_comp = match vm.heap().get(arrayref) {
                Some(Oop::Array(a)) => super::arraycopy::component_of(a.class_name()).to_string(),
                _ => return Err(VmError::BadConstant("aastore 目标非数组")),
            };
            let elem_comp = super::arraycopy::element_component(vm, *elem)?;
            !super::arraycopy::component_assignable(&elem_comp, &array_comp, &reg)
        };
        if not_assignable {
            return Err(throw_exception(vm, "java/lang/ArrayStoreException"));
        }
    }
    match vm
        .heap_mut()
        .get_mut(arrayref)
        .ok_or(VmError::BadConstant("astore 引用悬空"))?
    {
        Oop::Array(a) => a.set_element(idx, value),
        Oop::Instance(_) | Oop::Lambda(_) => {
            return Err(VmError::BadConstant("astore 目标非数组"))
        }
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

#[cfg(test)]
mod multi_tests {
    use super::*;
    use crate::runtime::Slot;

    #[test]
    fn parse_int_2d() {
        let (n, base) = parse_array_descriptor("[[I").unwrap();
        assert_eq!(n, 2);
        assert_eq!(base, Slot::Int(0));
    }

    #[test]
    fn parse_object_2d() {
        let (n, base) = parse_array_descriptor("[[Ljava/lang/Object;").unwrap();
        assert_eq!(n, 2);
        assert_eq!(base, Slot::Reference(crate::runtime::Reference::null()));
    }

    #[test]
    fn parse_long_1d() {
        let (n, base) = parse_array_descriptor("[J").unwrap();
        assert_eq!(n, 1);
        assert_eq!(base, Slot::Long(0));
    }

    #[test]
    fn parse_non_array_rejected() {
        assert!(parse_array_descriptor("Ljava/lang/String;").is_err());
    }
}
