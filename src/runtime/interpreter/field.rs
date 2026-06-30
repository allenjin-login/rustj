//! 对象与字段访问:`new` / `getfield` / `putfield` / `getstatic` / `putstatic` 的解析与执行。
//!
//! 对应 HotSpot `interpreter/zero/bytecodeInterpreter.cpp` 的 `CASE(_new)` /
//! `CASE(_getfield)` / `CASE(_putfield)` / `CASE(_getstatic)` / `CASE(_putstatic)`
//! 与 `LinkResolver::resolve_field`。4.1 仅本类字段(实例/静态);继承字段叠加
//! 留待 4.2(随类层次与虚分派)。
//!
//! 实例字段每字段一槽(见 [`crate::oops`]);cat-2(long/double)在 getfield/putfield
//! 边界做类型转换。静态字段经 [`LoadedClass::static_storage`](RefCell) 内部可变性写入——
//! 对应 HotSpot `InstanceKlass` 中就地持有的静态字段区。

use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::{parse_field_descriptor, FieldType};
use crate::oops::Oop;
use crate::runtime::{Frame, Slot, Vm};

use super::{clinit, throw_exception, Interpreter, VmError};

/// 解析 `Fieldref` 常量池条目 → `(类内部名, 字段名, 描述符)`。owned 字符串。
pub(super) fn resolve_fieldref(
    cp: &ConstantPool,
    index: u16,
) -> Result<(String, String, String), VmError> {
    let ConstantPoolEntry::Fieldref {
        class_index,
        name_and_type_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("字段指令操作数须为 Fieldref"));
    };
    let class_name = class_name(cp, *class_index)?;
    let (name, desc) = name_and_type(cp, *name_and_type_index)?;
    Ok((class_name, name, desc))
}

/// 解析 `Class` 条目 → 类内部名(`new` 的操作数)。
pub(super) fn resolve_class_name(cp: &ConstantPool, class_index: u16) -> Result<String, VmError> {
    let ConstantPoolEntry::Class { name_index } = cp.get(class_index)?
    else {
        return Err(VmError::BadConstant("new 操作数须为 Class"));
    };
    utf8(cp, *name_index)
}

/// 解析 `Class` 条目 → 类内部名。
fn class_name(cp: &ConstantPool, class_index: u16) -> Result<String, VmError> {
    let ConstantPoolEntry::Class { name_index } = cp.get(class_index)?
    else {
        return Err(VmError::BadConstant("Fieldref.class 须为 Class"));
    };
    utf8(cp, *name_index)
}

/// 解析 `NameAndType` 条目 → `(字段名, 描述符)`。
fn name_and_type(cp: &ConstantPool, index: u16) -> Result<(String, String), VmError> {
    let ConstantPoolEntry::NameAndType {
        name_index,
        descriptor_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("Fieldref 须含 NameAndType"));
    };
    Ok((utf8(cp, *name_index)?, utf8(cp, *descriptor_index)?))
}

/// 取 `Utf8` 条目的字符串(owned)。
fn utf8(cp: &ConstantPool, index: u16) -> Result<String, VmError> {
    match cp.get(index)? {
        ConstantPoolEntry::Utf8(s) => Ok(s.clone()),
        _ => Err(VmError::BadConstant("期望 Utf8 条目")),
    }
}

/// 按字段类型从操作数栈弹出一个值 → 槽(byte/char/short/boolean 以 int 承载)。
fn pop_field_value(frame: &mut Frame, ft: &FieldType) -> Result<Slot, VmError> {
    Ok(match ft {
        FieldType::Long => Slot::Long(frame.operands.pop_long()?),
        FieldType::Double => Slot::Double(frame.operands.pop_double()?),
        FieldType::Float => Slot::Float(frame.operands.pop_float()?),
        FieldType::Int
        | FieldType::Byte
        | FieldType::Char
        | FieldType::Short
        | FieldType::Boolean => Slot::Int(frame.operands.pop_int()?),
        FieldType::Class(_) | FieldType::Array(_) => Slot::Reference(frame.operands.pop_reference()?),
    })
}

/// 按字段类型把一个槽的值压回操作数栈(类型不符报错)。
fn push_field_value(frame: &mut Frame, ft: &FieldType, slot: Slot) -> Result<(), VmError> {
    match ft {
        FieldType::Long => {
            let Slot::Long(v) = slot else {
                return Err(VmError::BadConstant("字段类型 long 与值不符"));
            };
            frame.operands.push_long(v)?;
        }
        FieldType::Double => {
            let Slot::Double(v) = slot else {
                return Err(VmError::BadConstant("字段类型 double 与值不符"));
            };
            frame.operands.push_double(v)?;
        }
        FieldType::Float => {
            let Slot::Float(v) = slot else {
                return Err(VmError::BadConstant("字段类型 float 与值不符"));
            };
            frame.operands.push_float(v)?;
        }
        FieldType::Int
        | FieldType::Byte
        | FieldType::Char
        | FieldType::Short
        | FieldType::Boolean => {
            let Slot::Int(v) = slot else {
                return Err(VmError::BadConstant("字段类型 int 与值不符"));
            };
            frame.operands.push_int(v)?;
        }
        FieldType::Class(_) | FieldType::Array(_) => {
            let Slot::Reference(v) = slot else {
                return Err(VmError::BadConstant("字段类型引用与值不符"));
            };
            frame.operands.push_reference(v)?;
        }
    }
    Ok(())
}

/// `new`:解析类 → 扁平化默认实例 → 堆分配 → 压引用。由分派循环读 u2 后调用。
pub(super) fn new_instance(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    class_index: u16,
) -> Result<(), VmError> {
    let class_name = resolve_class_name(interp.cp(), class_index)?;
    // 首次实例化 → 触发类初始化(<clinit>),先于分配。
    clinit::ensure_class_initialized(vm, &class_name)?;
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("new 需要类注册表"))?;
    let lc = registry
        .get(&class_name)
        .ok_or(VmError::BadConstant("new 目标类未加载"))?;
    let oop = Oop::Instance(registry.new_instance(lc));
    let reference = vm.heap_mut().alloc(oop);
    frame.operands.push_reference(reference)?;
    Ok(())
}

/// `getfield`:解析 → 定位(扁平)序号 → 弹 objref → null 检查 → 读实例槽 → 按类型压值。
pub(super) fn get_field(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    fieldref_index: u16,
) -> Result<(), VmError> {
    let (class_name, field_name, desc) = resolve_fieldref(interp.cp(), fieldref_index)?;
    let ft = parse_field_descriptor(&desc)?;
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("字段指令需要类注册表"))?;
    let lc = registry
        .get(&class_name)
        .ok_or(VmError::BadConstant("目标类未加载"))?;
    let ordinal = registry
        .instance_field(lc, &field_name, &ft)
        .ok_or(VmError::BadConstant("getfield 未找到实例字段"))?;

    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    let slot = match vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("getfield 引用悬空"))?
    {
        Oop::Instance(i) => i.field(ordinal),
        Oop::Array(_) | Oop::Class(_) => {
            return Err(VmError::BadConstant("getfield 目标为数组/Class"))
        }
    };
    push_field_value(frame, &ft, slot)?;
    Ok(())
}

/// `putfield`:解析 → 定位(扁平)序号 → 弹值、弹 objref → null 检查 → 写实例槽。
///
/// 栈布局:`... objref, value`(value 在顶)。先弹值,后弹 objref。
pub(super) fn put_field(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    fieldref_index: u16,
) -> Result<(), VmError> {
    let (class_name, field_name, desc) = resolve_fieldref(interp.cp(), fieldref_index)?;
    let ft = parse_field_descriptor(&desc)?;
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("字段指令需要类注册表"))?;
    let lc = registry
        .get(&class_name)
        .ok_or(VmError::BadConstant("目标类未加载"))?;
    let ordinal = registry
        .instance_field(lc, &field_name, &ft)
        .ok_or(VmError::BadConstant("putfield 未找到实例字段"))?;

    let value = pop_field_value(frame, &ft)?;
    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    match vm
        .heap_mut()
        .get_mut(objref)
        .ok_or(VmError::BadConstant("putfield 引用悬空"))?
    {
        Oop::Instance(i) => i.set_field(ordinal, value),
        Oop::Array(_) | Oop::Class(_) => {
            return Err(VmError::BadConstant("putfield 目标为数组/Class"))
        }
    }
    Ok(())
}

/// `getstatic`:解析 → 读静态槽 → 按类型压值。
pub(super) fn get_static(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    fieldref_index: u16,
) -> Result<(), VmError> {
    let (class_name, field_name, desc) = resolve_fieldref(interp.cp(), fieldref_index)?;
    let ft = parse_field_descriptor(&desc)?;
    // 首次读静态字段 → 触发声明类初始化(<clinit> 先行写入;ensure 会先初始化超类)。
    clinit::ensure_class_initialized(vm, &class_name)?;
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("字段指令需要类注册表"))?;
    // 沿超类链找声明类(Fieldref 类可能为继承字段的子类)+ 其静态槽。
    let (lc, ordinal) = registry
        .resolve_static_field(&class_name, &field_name, &ft)
        .ok_or(VmError::BadConstant("getstatic 未找到静态字段"))?;
    let slot = *lc
        .static_storage
        .borrow()
        .get(ordinal)
        .ok_or(VmError::BadConstant("getstatic 静态槽越界"))?;
    push_field_value(frame, &ft, slot)?;
    Ok(())
}

/// `putstatic`:解析 → 弹值 → 写静态槽。
pub(super) fn put_static(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    fieldref_index: u16,
) -> Result<(), VmError> {
    let (class_name, field_name, desc) = resolve_fieldref(interp.cp(), fieldref_index)?;
    let ft = parse_field_descriptor(&desc)?;
    // 首次写静态字段 → 触发声明类初始化。
    clinit::ensure_class_initialized(vm, &class_name)?;
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("字段指令需要类注册表"))?;
    let (lc, ordinal) = registry
        .resolve_static_field(&class_name, &field_name, &ft)
        .ok_or(VmError::BadConstant("putstatic 未找到静态字段"))?;
    let value = pop_field_value(frame, &ft)?;
    lc.static_storage.borrow_mut()[ordinal] = value;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::classfile::Reader;
    use crate::constant_pool::ConstantPool;

    /// 构造常量池:
    /// `[1]`Utf8"Pt" `[2]`Class{1} `[3]`Utf8"x" `[4]`Utf8"I"
    /// `[5]`NameAndType{3,4} `[6]`Fieldref{class=2, nat=5}
    fn cp_with_fieldref() -> ConstantPool {
        let bytes = [
            0x00, 0x07, // count=7
            0x01, 0x00, 0x02, b'P', b't', // [1] "Pt"
            0x07, 0x00, 0x01, // [2] Class{1}
            0x01, 0x00, 0x01, b'x', // [3] "x"
            0x01, 0x00, 0x01, b'I', // [4] "I"
            0x0C, 0x00, 0x03, 0x00, 0x04, // [5] NameAndType{3,4}
            0x09, 0x00, 0x02, 0x00, 0x05, // [6] Fieldref{class=2, nat=5}
        ];
        ConstantPool::parse(&mut Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn resolve_fieldref_decodes_class_name_and_descriptor() {
        let cp = cp_with_fieldref();
        let (class, name, desc) = super::resolve_fieldref(&cp, 6).unwrap();
        assert_eq!(class, "Pt");
        assert_eq!(name, "x");
        assert_eq!(desc, "I");
    }

    #[test]
    fn resolve_class_name_decodes_class_entry() {
        let cp = cp_with_fieldref();
        assert_eq!(super::resolve_class_name(&cp, 2).unwrap(), "Pt");
    }
}
