//! 方法调用:`invokestatic` 的解析、实参传递与递归执行。
//!
//! 对应 HotSpot `interpreter/zero/bytecodeInterpreter.cpp` 的 `CASE(_invokestatic)`
//! 分支与 `Bytecode_invoke::static_target()`。本增量实现**同类内**(含递归与互调)
//! 的 `invokestatic`;`invokevirtual`/`invokespecial`/`invokeinterface` 需要对象模型,
//! 留待后续层。跨类调用只需在 [`ClassProvider`] 上扩展即可。
//!
//! **帧管理**:用 Rust 调用栈作为隐式调用栈(每次 `invokestatic` 递归 `interpret`)。
//! 这是"简易帧管理器":正确、安全、零额外结构。显式帧栈(用于深度上限 /
//! `StackOverflowError` 检测)留待对象模型层。

use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::{parse_method_descriptor, FieldType, ReturnDescriptor};
use crate::metadata::{ClassFile, MethodInfo};
use crate::runtime::{Frame, LocalVars};

use super::{Interpreter, Value, VmError};

/// 按类的内部名提供已加载的 [`ClassFile`],用于解析 `invokestatic` 的目标方法。
///
/// 本增量只需一个实现需求:返回当前已加载的同一个类(同类自调/递归)。
/// 后续类加载器实现该 trait 即可支持跨类调用。
pub trait ClassProvider {
    /// 按内部名(如 `java/lang/String`)取已加载的类;未加载返回 `None`。
    fn class_by_name(&self, name: &str) -> Option<&ClassFile>;
}

/// 解析 `Methodref` 常量池条目 → `(类内部名, 方法名, 描述符)`。
///
/// 返回 owned `String`,避免常量池借用与后续栈帧操作纠缠。
pub(super) fn resolve_methodref(
    cp: &ConstantPool,
    index: u16,
) -> Result<(String, String, String), VmError> {
    let ConstantPoolEntry::Methodref {
        class_index,
        name_and_type_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("invokestatic 操作数须为 Methodref"));
    };
    let class_name = class_name(cp, *class_index)?;
    let (name, desc) = name_and_type(cp, *name_and_type_index)?;
    Ok((class_name, name, desc))
}

/// 解析 `Class` 条目 → 类内部名。
fn class_name(cp: &ConstantPool, class_index: u16) -> Result<String, VmError> {
    let ConstantPoolEntry::Class { name_index } = cp.get(class_index)?
    else {
        return Err(VmError::BadConstant("Methodref.class 须为 Class"));
    };
    utf8(cp, *name_index)
}

/// 解析 `NameAndType` 条目 → `(方法名, 描述符)`。
fn name_and_type(cp: &ConstantPool, index: u16) -> Result<(String, String), VmError> {
    let ConstantPoolEntry::NameAndType {
        name_index,
        descriptor_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("Methodref 须含 NameAndType"));
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

/// 在类中按名 + 描述符查找方法;未命中返回错误。
fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> Result<&'a MethodInfo, VmError> {
    cf.methods
        .iter()
        .find(|m| method_matches(cf, m, name, desc))
        .ok_or(VmError::BadConstant("invokestatic 未找到目标方法"))
}

/// 方法名与描述符是否同时匹配。
fn method_matches(cf: &ClassFile, m: &MethodInfo, name: &str, desc: &str) -> bool {
    let name_ok = matches!(
        cf.constant_pool.get(m.name_index),
        Ok(ConstantPoolEntry::Utf8(n)) if n == name
    );
    let desc_ok = matches!(
        cf.constant_pool.get(m.descriptor_index),
        Ok(ConstantPoolEntry::Utf8(d)) if d == desc
    );
    name_ok && desc_ok
}

/// 从调用者操作数栈按**逆序**弹出单个实参(按字段类型决定弹出类型)。
///
/// JVM 栈上 `byte/char/short/boolean` 一律以 int 承载,故按 int 弹出。
/// 引用类型(对象/数组)本增量未实现,返回错误。
fn pop_arg(frame: &mut Frame, ft: &FieldType) -> Result<Value, VmError> {
    Ok(match ft {
        FieldType::Long => Value::Long(frame.operands.pop_long()?),
        FieldType::Double => Value::Double(frame.operands.pop_double()?),
        FieldType::Float => Value::Float(frame.operands.pop_float()?),
        FieldType::Int
        | FieldType::Byte
        | FieldType::Char
        | FieldType::Short
        | FieldType::Boolean => Value::Int(frame.operands.pop_int()?),
        FieldType::Class(_) | FieldType::Array(_) => {
            return Err(VmError::BadConstant("invokestatic 引用类型实参(对象模型未实现)"));
        }
    })
}

/// 把单个实参写入被调用者局部变量,返回其占用的槽位数(long/double = 2)。
fn store_arg(locals: &mut LocalVars, slot: u16, v: Value) -> Result<u16, VmError> {
    Ok(match v {
        Value::Int(x) => {
            locals.set_int(slot, x)?;
            1
        }
        Value::Long(x) => {
            locals.set_long(slot, x)?;
            2
        }
        Value::Float(x) => {
            locals.set_float(slot, x)?;
            1
        }
        Value::Double(x) => {
            locals.set_double(slot, x)?;
            2
        }
        Value::Void => return Err(VmError::BadConstant("void 不可作为实参")),
    })
}

/// 把返回值压回调用者操作数栈。
fn push_return(frame: &mut Frame, v: Value) -> Result<(), VmError> {
    match v {
        Value::Int(x) => frame.operands.push_int(x)?,
        Value::Long(x) => frame.operands.push_long(x)?,
        Value::Float(x) => frame.operands.push_float(x)?,
        Value::Double(x) => frame.operands.push_double(x)?,
        Value::Void => {}
    }
    Ok(())
}

/// 执行 `invokestatic`:解析目标方法、传递实参、递归解释、回填返回值。
///
/// 由分派循环读取 u2 索引后调用;返回后由调用方推进 `pc += 3`。
/// "帧管理"即 Rust 调用栈:此处构造被调用者栈帧并递归 `interpret`,
/// 返回后回到本帧继续执行。
pub(super) fn invoke_static(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    methodref_index: u16,
) -> Result<(), VmError> {
    let classes = interp.classes().ok_or(VmError::BadConstant(
        "invokestatic 需要类解析上下文(ClassProvider)",
    ))?;
    let (class_name, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    let target_cf = classes
        .class_by_name(&class_name)
        .ok_or(VmError::BadConstant("invokestatic 目标类未加载"))?;
    let target_method = find_method(target_cf, &method_name, &desc)?;
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("invokestatic 目标方法无 Code(抽象/原生)"))?;

    let md = parse_method_descriptor(&desc)?;

    // 实参在调用者栈上为正序(arg0 在底,argN 在顶);逆序弹出后翻转为正序,
    // 再按 JVM 调用约定写入被调用者局部变量 0..(long/double 占两槽)。
    let mut args: Vec<Value> = Vec::with_capacity(md.parameters.len());
    for ft in md.parameters.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    args.reverse();

    let mut callee = Frame::new(code.max_locals, code.max_stack);
    let mut slot: u16 = 0;
    for v in args {
        let advance = store_arg(&mut callee.locals, slot, v)?;
        slot = slot
            .checked_add(advance)
            .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
    }

    // 递归:用目标方法的字节码与常量池构造新解释器,沿用同一类解析上下文。
    let callee_interp = Interpreter::with_classes(&code.code, &target_cf.constant_pool, classes);
    let result = callee_interp.interpret(&mut callee)?;

    // 按描述符返回类型回填:void 不压栈;非 void 压返回值;类型不符报错。
    match (md.return_type, result) {
        (ReturnDescriptor::Void, Value::Void) => {}
        (ReturnDescriptor::FieldType(_), Value::Void) => {
            return Err(VmError::BadConstant("invokestatic 期望返回值,被调用者返回 void"));
        }
        (ReturnDescriptor::FieldType(_), v) => push_return(frame, v)?,
        (ReturnDescriptor::Void, _) => {
            return Err(VmError::BadConstant("invokestatic void 方法返回了值"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::classfile::Reader;
    use crate::constant_pool::ConstantPool;

    /// 构造常量池:
    /// `[1]`Utf8"MyClass" `[2]`Class{1} `[3]`Utf8"doThing" `[4]`Utf8"(IJ)I"
    /// `[5]`NameAndType{3,4} `[6]`Methodref{class=2, nat=5}
    fn cp_with_methodref() -> ConstantPool {
        let bytes = [
            0x00, 0x07, // count=7
            0x01, 0x00, 0x07, b'M', b'y', b'C', b'l', b'a', b's', b's', // [1] "MyClass"(7)
            0x07, 0x00, 0x01, // [2] Class{1}
            0x01, 0x00, 0x07, b'd', b'o', b'T', b'h', b'i', b'n', b'g', // [3] "doThing"
            0x01, 0x00, 0x05, b'(', b'I', b'J', b')', b'I', // [4] "(IJ)I"
            0x0C, 0x00, 0x03, 0x00, 0x04, // [5] NameAndType{3,4}
            0x0A, 0x00, 0x02, 0x00, 0x05, // [6] Methodref{class=2, nat=5}
        ];
        ConstantPool::parse(&mut Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn resolve_methodref_decodes_class_name_and_descriptor() {
        let cp = cp_with_methodref();
        let (class, name, desc) = super::resolve_methodref(&cp, 6).unwrap();
        assert_eq!(class, "MyClass");
        assert_eq!(name, "doThing");
        assert_eq!(desc, "(IJ)I");
    }
}
