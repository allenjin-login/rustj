//! 类型检查:`checkcast` / `instanceof`。
//!
//! 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_checkcast)` / `CASE(_instanceof)`。
//! 子类型判定经 `ClassRegistry::is_instance`(超类链 ∪ 接口闭包)。

use super::field::resolve_class_name;
use super::{throw_exception, Interpreter, VmError};
use crate::oops::Oop;
use crate::runtime::{Frame, Reference, Vm};

/// 取 objectref(非 null)的(是否数组, 运行时类名)。own 字符串避免借用纠缠。
/// 数组取其类型描述符(`[B` / `[Ljava/lang/String;`),实例取类内部名。
fn object_type(vm: &Vm<'_>, objref: Reference) -> Result<(bool, Option<String>), VmError> {
    let obj = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("checkcast/instanceof 引用悬空"))?;
    Ok(match obj {
        Oop::Instance(i) => (false, Some(i.class_name().to_string())),
        Oop::Array(a) => (true, Some(a.class_name().to_string())),
        // 闭包按其函数式接口类型上报(局部正确性;完整可赋值性顺延,见 spec 4.10aa §3.4)。
        Oop::Lambda(l) => (false, Some(l.sam_type().to_string())),
    })
}

/// `[Ljava/lang/String;` 的组件段(`Ljava/lang/String;`)→ 类名 `java/lang/String`;
/// 非引用组件(基本类型 / 仍含维度的数组)→ `None`。
fn class_of_obj_desc(component: &str) -> Option<&str> {
    let b = component.as_bytes();
    if b.first() == Some(&b'L') && b.last() == Some(&b';') {
        component.get(1..component.len() - 1)
    } else {
        None
    }
}

/// 数组 `instanceof` 判定(JVMS §6.5.instanceof;HotSpot `arrayKlass::is_java_subtype_of`)。
/// `sub` 为对象数组的运行时描述符;`target` 为 `instanceof`/`checkcast` 的右操作数解析名
/// (数组描述符,或 `Object`/`Cloneable`/`java/io/Serializable` 之类超类)。规则:
/// - 所有数组都是 `Object` / `Cloneable` / `java.io.Serializable` 的实例;
/// - 同描述符 → 真;
/// - `[Ljava/lang/Object;` 左值的组件为引用/数组时为真(基本类型数组非 `Object[]`);
/// - 同维数组递归剥维;引用组件 `[Lsub;` vs `[Lsup;` 走类层 `is_instance`。
///
/// `pub(super)` 供 `arraycopy` 复用:「组件 A 可赋给组件 B」⟺「`[A` instanceof `[B`」
/// (见 `arraycopy::component_assignable`)。
pub(super) fn array_instanceof(sub: &str, target: &str, reg: &crate::oops::ClassRegistry) -> bool {
    match target {
        "java/lang/Object" | "java/lang/Cloneable" | "java/io/Serializable" => return true,
        _ => {}
    }
    if sub == target {
        return true;
    }
    let Some(sub_comp) = sub.strip_prefix('[') else {
        return false;
    };
    let Some(target_comp) = target.strip_prefix('[') else {
        return false; // 非数组目标(且非上述超类)→ 不是数组的超类
    };
    // 目标是 Object[]:子数组组件为引用(L…)或数组([…)即成立;基本类型组件则否。
    if target_comp == "Ljava/lang/Object;" {
        return sub_comp.starts_with('[') || sub_comp.starts_with('L');
    }
    // 同为多维数组 → 递归剥一维。
    if sub_comp.starts_with('[') && target_comp.starts_with('[') {
        return array_instanceof(sub_comp, target_comp, reg);
    }
    // 引用组件 [Lsub; vs [Ltarget; → 组件类可赋值。
    if let (Some(s_name), Some(t_name)) =
        (class_of_obj_desc(sub_comp), class_of_obj_desc(target_comp))
    {
        return reg.is_instance(s_name, t_name);
    }
    false
}

/// 命中判定:objectref(非 null)是否 target 实例。数组走 [`array_instanceof`],实例走
/// 类注册表的 `is_instance`(超类链 ∪ 接口闭包)。
fn matches(
    interp: &Interpreter<'_>,
    vm: &Vm<'_>,
    objref: Reference,
    index: u16,
) -> Result<bool, VmError> {
    let target = resolve_class_name(interp.cp(), index)?;
    let (is_array, class_name) = object_type(vm, objref)?;
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("checkcast/instanceof 需类注册表"))?;
    Ok(if is_array {
        array_instanceof(class_name.as_deref().unwrap_or(""), &target, reg)
    } else {
        reg.is_instance(class_name.as_deref().unwrap_or(""), &target)
    })
}

/// `checkcast`:弹 objectref,判定,保留 objectref;不匹配 → ClassCastException。null 保留。
pub(super) fn check_cast(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    index: u16,
) -> Result<(), VmError> {
    let objref = frame.operands.pop_reference()?;
    let ok = if objref.is_null() {
        true
    } else {
        matches(interp, vm, objref, index)?
    };
    frame.operands.push_reference(objref)?;
    if ok {
        Ok(())
    } else {
        Err(throw_exception(vm, "java/lang/ClassCastException"))
    }
}

/// `instanceof`:弹 objectref,压 1/0。null → 0。
pub(super) fn instance_of(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    index: u16,
) -> Result<(), VmError> {
    let objref = frame.operands.pop_reference()?;
    let result = if objref.is_null() {
        0
    } else {
        i32::from(matches(interp, vm, objref, index)?)
    };
    frame.operands.push_int(result)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::bytecode::opcode::Opcode;
    use crate::classfile::Reader;
    use crate::constant_pool::ConstantPool;
    use crate::metadata::{AccessFlags, ClassFile};
    use crate::oops::{ClassRegistry, Oop};
    use crate::runtime::{Frame, Interpreter, Value, Vm};

    /// utf8: 1=Object 2=Shape 3=Square ; class: 4=Object 5=Shape 6=Square。
    fn cp_bytes() -> Vec<u8> {
        let mut b = vec![0x00, 0x07]; // count = 6 entries + 1
        for s in ["java/lang/Object", "Shape", "Square"] {
            b.push(0x01);
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        for idx in [1u16, 2, 3] {
            b.push(0x07);
            b.extend_from_slice(&idx.to_be_bytes());
        }
        b
    }

    fn build() -> (ClassRegistry, ConstantPool) {
        let bytes = cp_bytes();
        let mk_cp = || ConstantPool::parse(&mut Reader::new(&bytes)).unwrap();
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
        let mut reg = ClassRegistry::new();
        reg.load(mk_cf(6, 5)).unwrap(); // Square extends Shape
        reg.load(mk_cf(5, 4)).unwrap(); // Shape extends Object
        (reg, mk_cp())
    }

    #[test]
    fn instanceof_shape_on_square_returns_one() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let square_lc = reg.get("Square").unwrap();
        let inst = vm
            .heap_mut()
            .alloc(Oop::Instance(reg.new_instance(square_lc)));
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Instanceof as u8,
            0x00,
            0x05, // #5 = Shape
            Opcode::Ireturn as u8,
        ];
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn instanceof_null_returns_zero() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let code = [
            Opcode::AconstNull as u8,
            Opcode::Instanceof as u8,
            0x00,
            0x05,
            Opcode::Ireturn as u8,
        ];
        let mut frame = Frame::new(0, 2);
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn checkcast_shape_on_square_passes() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let square_lc = reg.get("Square").unwrap();
        let inst = vm
            .heap_mut()
            .alloc(Oop::Instance(reg.new_instance(square_lc)));
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Checkcast as u8,
            0x00,
            0x05, // #5 = Shape
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(1)
        );
    }

    #[test]
    fn checkcast_own_class_passes() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let square_lc = reg.get("Square").unwrap();
        let inst = vm
            .heap_mut()
            .alloc(Oop::Instance(reg.new_instance(square_lc)));
        // checkcast 自身类通过(objectref 保留、不抛);失败用例留给集成闸门(同级类 Rect)。
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Checkcast as u8,
            0x00,
            0x06, // #6 = Square(自身)
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(
            interp.interpret_with(&mut frame, &mut vm).unwrap(),
            Value::Int(1)
        );
    }
}
