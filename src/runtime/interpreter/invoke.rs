//! 方法调用:`invokestatic` 与 `invokespecial`(`<init>`)的解析、实参传递与递归执行。
//!
//! 对应 HotSpot `interpreter/zero/bytecodeInterpreter.cpp` 的 `CASE(_invokestatic)` /
//! `CASE(_invokespecial)` 与 `Bytecode_invoke::static_target()`。
//!
//! - `invokestatic`:同类内(含递归与互调);跨类调用只需加载更多类。
//! - `invokespecial`:4.1 仅用于**实例初始化** `<init>`(构造器)。对象已在 `new` 时默认
//!   初始化,此处运行构造器字节码(objref 为 local[0])。未加载的根类
//!   (如 `java/lang/Object`)的 `<init>()V` 视作空操作——其构造器无可观察副作用。
//!   `invokevirtual`/`invokeinterface`(虚分派)与 `invokespecial` 对私有/`super` 的完整
//!   语义留待 4.2(随类层次)。
//!
//! **帧管理**:用 Rust 调用栈作为隐式调用栈(每次调用递归 `interpret_with`)。
//! 这是"简易帧管理器":正确、安全、零额外结构。显式帧栈(用于深度上限 /
//! `StackOverflowError` 检测)留待对象模型层。

use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::{parse_method_descriptor, FieldType, ReturnDescriptor};
use crate::metadata::{ClassFile, MethodInfo};
use crate::oops::Oop;
use crate::runtime::{Frame, LocalVars, Reference, Vm};

use super::{clinit, exception, throw_exception, Interpreter, Value, VmError};

/// invoke 后调用者分派循环的流向。
pub(super) enum InvokeFlow {
    /// 正常返回(含 void);调用方推进 pc(`invokestatic`/`special`/`virtual` +3,
    /// `invokeinterface` +5)。
    Fallthrough,
    /// 捕获被调用者抛出的异常并已设好处理帧(清栈压异常);调用方跳 `handler_pc`(不推进)。
    Jump(usize),
}

/// 统一被调用者结果:正常则按返回类型回填(`Fallthrough`);抛异常则经**调用者帧**
/// 异常表(`interp.exception_table()` @ `caller_pc`)找处理者——命中清栈压异常(`Jump(h)`),
/// 未命中原样 `Err(ThrownException)` 上传(本层仅用户 `athrow` 异常可捕获)。
///
/// 取代各 invoke 末尾原 `match (return_type, result) { ... }` 块(消除 4 处重复),
/// 并把异常捕获单点化。
fn finish_invoke(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    caller_pc: usize,
    result: Result<Value, VmError>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    match result {
        Ok(v) => {
            match (return_type, v) {
                (ReturnDescriptor::Void, Value::Void) => {}
                (ReturnDescriptor::FieldType(_), Value::Void) => {
                    return Err(VmError::BadConstant("invoke 期望返回值,被调用者返回 void"));
                }
                (ReturnDescriptor::FieldType(_), val) => push_return(frame, val)?,
                (ReturnDescriptor::Void, _) => {
                    return Err(VmError::BadConstant("invoke void 方法返回了值"));
                }
            }
            Ok(InvokeFlow::Fallthrough)
        }
        Err(VmError::ThrownException(exc)) => match exception::find_handler(
            interp,
            vm,
            interp.exception_table(),
            caller_pc,
            exc,
        )? {
            Some(h) => {
                frame.operands.clear();
                frame.operands.push_reference(exc)?;
                Ok(InvokeFlow::Jump(h))
            }
            None => Err(VmError::ThrownException(exc)),
        },
        Err(e) => Err(e),
    }
}

/// 进入一帧:`frame_depth +1`,执行 `f`,返回前 `−1`(Ok/Err 两路对称)。
/// `frame_depth >= stack_limit` 时直接抛 `java/lang/StackOverflowError`
/// ([`VmError::ThrownException`]),不进入 `f`。
pub(crate) fn run_with_depth<R>(
    vm: &mut Vm<'_>,
    f: impl FnOnce(&mut Vm<'_>) -> Result<R, VmError>,
) -> Result<R, VmError> {
    if vm.frame_depth >= vm.stack_limit {
        return Err(throw_exception(vm, "java/lang/StackOverflowError"));
    }
    vm.frame_depth += 1;
    let r = f(vm);
    vm.frame_depth -= 1;
    r
}

/// 解析 `Methodref` / `InterfaceMethodref` 常量池条目 → `(类内部名, 方法名, 描述符)`。
///
/// `invokestatic`/`special`/`virtual` 指向 `Methodref`;`invokeinterface` 指向
/// `InterfaceMethodref`——两者结构相同,此处一并接受。返回 owned `String`,
/// 避免常量池借用与后续栈帧操作纠缠。
pub(super) fn resolve_methodref(
    cp: &ConstantPool,
    index: u16,
) -> Result<(String, String, String), VmError> {
    let (class_index, name_and_type_index) = match cp.get(index)? {
        ConstantPoolEntry::Methodref {
            class_index,
            name_and_type_index,
        }
        | ConstantPoolEntry::InterfaceMethodref {
            class_index,
            name_and_type_index,
        } => (*class_index, *name_and_type_index),
        _ => return Err(VmError::BadConstant("invoke 操作数须为 Methodref/InterfaceMethodref")),
    };
    let class_name = class_name(cp, class_index)?;
    let (name, desc) = name_and_type(cp, name_and_type_index)?;
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

/// 一个调用实参(含引用),用于在调用者栈与被调用者局部变量间传递。
enum Arg {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Reference(Reference),
}

/// 从调用者操作数栈弹出单个实参(按字段类型决定弹出类型)。
///
/// JVM 栈上 `byte/char/short/boolean` 一律以 int 承载,故按 int 弹出。
fn pop_arg(frame: &mut Frame, ft: &FieldType) -> Result<Arg, VmError> {
    Ok(match ft {
        FieldType::Long => Arg::Long(frame.operands.pop_long()?),
        FieldType::Double => Arg::Double(frame.operands.pop_double()?),
        FieldType::Float => Arg::Float(frame.operands.pop_float()?),
        FieldType::Int
        | FieldType::Byte
        | FieldType::Char
        | FieldType::Short
        | FieldType::Boolean => Arg::Int(frame.operands.pop_int()?),
        FieldType::Class(_) | FieldType::Array(_) => Arg::Reference(frame.operands.pop_reference()?),
    })
}

/// 把单个实参写入被调用者局部变量,返回其占用的槽位数(long/double = 2)。
fn store_arg(locals: &mut LocalVars, slot: u16, arg: Arg) -> Result<u16, VmError> {
    Ok(match arg {
        Arg::Int(x) => {
            locals.set_int(slot, x)?;
            1
        }
        Arg::Long(x) => {
            locals.set_long(slot, x)?;
            2
        }
        Arg::Float(x) => {
            locals.set_float(slot, x)?;
            1
        }
        Arg::Double(x) => {
            locals.set_double(slot, x)?;
            2
        }
        Arg::Reference(r) => {
            locals.set_reference(slot, r)?;
            1
        }
    })
}

/// 把返回值压回调用者操作数栈。
fn push_return(frame: &mut Frame, v: Value) -> Result<(), VmError> {
    match v {
        Value::Int(x) => frame.operands.push_int(x)?,
        Value::Long(x) => frame.operands.push_long(x)?,
        Value::Float(x) => frame.operands.push_float(x)?,
        Value::Double(x) => frame.operands.push_double(x)?,
        Value::Reference(r) => frame.operands.push_reference(r)?,
        Value::Void => {}
    }
    Ok(())
}

/// 执行 `invokestatic`:解析目标方法、传递实参、递归解释、回填返回值。
///
/// 由分派循环读取 u2 索引后调用;返回后由调用方推进 `pc += 3`。
/// "帧管理"即 Rust 调用栈:此处构造被调用者栈帧并递归 `interpret_with`,
/// 返回后回到本帧继续执行。
pub(super) fn invoke_static(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokestatic 需要类注册表"))?;
    let (class_name, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    // 首次静态调用 → 触发声明类初始化(<clinit> 先行)。
    clinit::ensure_class_initialized(vm, &class_name)?;
    let target_lc = registry
        .get(&class_name)
        .ok_or(VmError::BadConstant("invokestatic 目标类未加载"))?;
    let target_method = find_method(&target_lc.cf, &method_name, &desc)?;
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("invokestatic 目标方法无 Code(抽象/原生)"))?;

    let md = parse_method_descriptor(&desc)?;

    // 实参在调用者栈上为正序(arg0 在底,argN 在顶);逆序弹出后翻转为正序,
    // 再按 JVM 调用约定写入被调用者局部变量 0..(long/double 占两槽)。
    let mut args: Vec<Arg> = Vec::with_capacity(md.parameters.len());
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

    // 递归:用目标方法的字节码与常量池 + 异常表构造新解释器,沿用同一 Vm(堆 + 注册表)。
    let callee_interp =
        Interpreter::new(&code.code, &target_lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm));

    // 回填返回值 / 捕获被调用者抛出的异常(单点)。
    finish_invoke(interp, frame, vm, caller_pc, result, md.return_type)
}

/// 执行 `invokespecial`:4.1 仅 `<init>`(构造器)。
///
/// 栈布局:`... objref, arg0..argN`(argN 在顶)。逆序弹 args,再弹 objref。
/// 目标类已加载 → 运行其构造器(objref 为 local[0]);未加载的根类
/// (如 `java/lang/Object`)`<init>()V` → 空操作(其构造器无副作用)。
pub(super) fn invoke_special(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
    let (class_name, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    let md = parse_method_descriptor(&desc)?;

    let mut args: Vec<Arg> = Vec::with_capacity(md.parameters.len());
    for ft in md.parameters.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    let objref = frame.operands.pop_reference()?;

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokespecial 需要类注册表"))?;
    // 解析目标 (类, 方法):
    //   <init> → 声明类精确(未加载根类 ()V → 空操作,沿用 4.1);
    //   私有   → 声明类精确(私有不可继承,无需虚查);
    //   其余   → super 虚查(声明类 = 调用者直接超类,上行)。
    let (target_lc, target_method) = if method_name == "<init>" {
        match registry.get(&class_name) {
            None => {
                // 未加载类(根类 java/lang/Object 等):仅放行 <init>()V 空构造器。
                if matches!(md.return_type, ReturnDescriptor::Void) {
                    return Ok(InvokeFlow::Fallthrough);
                }
                return Err(VmError::BadConstant("invokespecial 目标类未加载"));
            }
            Some(lc) => (lc, find_method(&lc.cf, &method_name, &desc)?),
        }
    } else {
        match registry.find_exact_method(&class_name, &method_name, &desc) {
            Some((lc, m)) if m.access_flags.is_private() => (lc, m),
            _ => registry
                .find_virtual_method(&class_name, &method_name, &desc)
                .ok_or(VmError::BadConstant("invokespecial 未找到目标方法"))?,
        }
    };
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("invokespecial 目标方法无 Code(抽象/原生)"))?;

    let mut callee = Frame::new(code.max_locals, code.max_stack);
    callee.locals.set_reference(0, objref)?;
    let mut slot: u16 = 1;
    for a in args {
        let advance = store_arg(&mut callee.locals, slot, a)?;
        slot = slot
            .checked_add(advance)
            .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
    }

    let callee_interp =
        Interpreter::new(&code.code, &target_lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm));

    finish_invoke(interp, frame, vm, caller_pc, result, md.return_type)
}

/// 执行 `invokevirtual`:按对象**运行时实际类**沿超类链虚分派。
///
/// 栈布局:`... objref, arg0..argN`(argN 在顶)。逆序弹 args,再弹 objref;null →
/// `NullPointer`。运行时类取自对象本身(`InstanceOop.class_name`),沿超类链找首个
/// (name, desc) 方法执行。Methodref 的声明类仅用于校验,**不参与分派**。
pub(super) fn invoke_virtual(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
    let (_declared_class, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    let md = parse_method_descriptor(&desc)?;

    let mut args: Vec<Arg> = Vec::with_capacity(md.parameters.len());
    for ft in md.parameters.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }

    // 运行时类 = 对象实际类(owned String,释放堆借用)。
    let runtime_class = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("invokevirtual 引用悬空"))?;
    let runtime_class = match runtime_class {
        Oop::Instance(i) => i.class_name().to_string(),
        Oop::Array(_) => {
            return Err(VmError::BadConstant("invoke 目标为数组(数组方法 clone 等顺延)"))
        }
        Oop::String(_) => {
            return Err(VmError::BadConstant("invoke 目标为 String(String 方法顺延)"))
        }
    };

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokevirtual 需要类注册表"))?;
    // 类链先行,落空走接口 default(Java 8+ 类类型调用 default 亦走此路);
    // 命中抽象方法 → AbstractMethodError。
    let (target_lc, target_method) = match registry
        .resolve_dispatch(&runtime_class, &method_name, &desc)
    {
        Some(x) => x,
        None => return Err(throw_exception(vm, "java/lang/AbstractMethodError")),
    };
    let Some(code) = target_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/AbstractMethodError"));
    };

    let mut callee = Frame::new(code.max_locals, code.max_stack);
    callee.locals.set_reference(0, objref)?;
    let mut slot: u16 = 1;
    for a in args {
        let advance = store_arg(&mut callee.locals, slot, a)?;
        slot = slot
            .checked_add(advance)
            .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
    }

    let callee_interp =
        Interpreter::new(&code.code, &target_lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm));

    finish_invoke(interp, frame, vm, caller_pc, result, md.return_type)
}

/// 执行 `invokeinterface`:按对象运行时实际类分派。语义与 `invokevirtual` 一致
/// (类链先行 → 接口 default 兜底,经 `resolve_dispatch`),差别仅在操作数 5 字节
/// (由分派循环 `pc += 5` 处理)与命中抽象方法报 `AbstractMethodError`。Methodref
/// 声明接口仅参与解析校验,**不参与分派**。
pub(super) fn invoke_interface(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
    let (_declared_iface, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    let md = parse_method_descriptor(&desc)?;

    let mut args: Vec<Arg> = Vec::with_capacity(md.parameters.len());
    for ft in md.parameters.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }

    // 运行时类 = 对象实际类(owned String,释放堆借用)。
    let runtime_class = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("invokeinterface 引用悬空"))?;
    let runtime_class = match runtime_class {
        Oop::Instance(i) => i.class_name().to_string(),
        Oop::Array(_) => {
            return Err(VmError::BadConstant("invoke 目标为数组(数组方法 clone 等顺延)"))
        }
        Oop::String(_) => {
            return Err(VmError::BadConstant("invoke 目标为 String(String 方法顺延)"))
        }
    };

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokeinterface 需要类注册表"))?;
    // 类链先行,落空走接口 default;命中抽象方法 → AbstractMethodError。
    let (target_lc, target_method) = match registry
        .resolve_dispatch(&runtime_class, &method_name, &desc)
    {
        Some(x) => x,
        None => return Err(throw_exception(vm, "java/lang/AbstractMethodError")),
    };
    let Some(code) = target_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/AbstractMethodError"));
    };

    let mut callee = Frame::new(code.max_locals, code.max_stack);
    callee.locals.set_reference(0, objref)?;
    let mut slot: u16 = 1;
    for a in args {
        let advance = store_arg(&mut callee.locals, slot, a)?;
        slot = slot
            .checked_add(advance)
            .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
    }

    let callee_interp =
        Interpreter::new(&code.code, &target_lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm));

    finish_invoke(interp, frame, vm, caller_pc, result, md.return_type)
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

    #[test]
    fn run_with_depth_counts_symmetrically() {
        // Ok 路径:进入 +1、退出 −1(嵌套两层验证递增)。
        let mut vm = crate::runtime::Vm::default();
        let r = super::run_with_depth(&mut vm, |vm| {
            let d1 = vm.frame_depth;
            let inner = super::run_with_depth(vm, |vm| Ok(vm.frame_depth));
            assert_eq!(d1, 1);
            assert_eq!(inner.unwrap(), 2);
            Ok(())
        });
        assert!(r.is_ok());
        assert_eq!(vm.frame_depth, 0);
    }

    #[test]
    fn run_with_depth_overflow_throws_stackoverflow_error() {
        // limit=2:外层→depth1,中层→depth2,内层 depth>=limit → 抛 StackOverflowError;
        // 异常路径仍对称归零。
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg).with_stack_limit(2);
        let r = super::run_with_depth(&mut vm, |vm| {
            super::run_with_depth(vm, |vm| super::run_with_depth(vm, |_| Ok(())))
        });
        let super::VmError::ThrownException(exc) = r.unwrap_err() else {
            panic!("应抛 StackOverflowError(ThrownException)");
        };
        let Some(crate::oops::Oop::Instance(i)) = vm.heap().get(exc) else {
            panic!("StackOverflowError 应为由引导桩分配的实例");
        };
        assert_eq!(i.class_name(), "java/lang/StackOverflowError");
        assert_eq!(vm.frame_depth, 0);
    }
}
