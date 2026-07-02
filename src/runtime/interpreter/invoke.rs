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

use crate::classfile::attributes::CodeAttribute;
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::{parse_method_descriptor, FieldType, ReturnDescriptor};
use crate::metadata::{ClassFile, MethodInfo};
use crate::oops::lambda::{
    REF_INVOKE_INTERFACE, REF_INVOKE_SPECIAL, REF_INVOKE_STATIC, REF_INVOKE_VIRTUAL,
    REF_NEW_INVOKE_SPECIAL,
};
use crate::oops::{LoadedClass, LambdaOop, Oop};
use crate::runtime::{Frame, LocalVars, Reference, Vm};

use super::{clinit, exception, native, throw_exception, Interpreter, Value, VmError};

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

/// 取 `Utf8` 条目的 `&str`(零分配,借自常量池)——供栈轨迹 `with_identity`。
fn cp_utf8(cp: &ConstantPool, index: u16) -> Result<&str, VmError> {
    match cp.get(index)? {
        ConstantPoolEntry::Utf8(s) => Ok(s.as_str()),
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

/// 一个调用实参(含引用),用于在调用者栈与被调用者局部变量间传递。全变体 Copy。
#[derive(Clone, Copy)]
enum Arg {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Reference(Reference),
}

/// 实参 → 解释器值(native 分派用:native 方法不走被调用者帧,直接消费 `Value`)。
impl From<Arg> for Value {
    fn from(a: Arg) -> Self {
        match a {
            Arg::Int(x) => Value::Int(x),
            Arg::Long(x) => Value::Long(x),
            Arg::Float(x) => Value::Float(x),
            Arg::Double(x) => Value::Double(x),
            Arg::Reference(r) => Value::Reference(r),
        }
    }
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

/// 弹出全部实参并翻正序(`args[i]` ↔ `parameters[i]`)。调用者栈上正序(arg0 底、argN 顶),
/// 故逆序弹出后再翻转;long/double 经 [`pop_arg`] 占单个 `Arg`。static/special/virtual/
/// interface/invokedynamic 共用。
fn pop_args(frame: &mut Frame, params: &[FieldType]) -> Result<Vec<Arg>, VmError> {
    let mut args = Vec::with_capacity(params.len());
    for ft in params.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    args.reverse();
    Ok(args)
}

/// 内置 native 分派:`Vec<Arg>` → `Vec<Value>`,调 [`native::invoke`],再经 [`finish_invoke`]
/// 回填返回值 / 捕获异常。静态 native 传 `this = None`,实例 native 传 `Some(objref)`。
/// `class` 为 native 声明类(静态 = 解析类名;实例 = 目标类的 `name()`,借自 detached 注册表,
/// 故与 `&mut vm` 不冲突)。
///
/// 参数多系 4 处调用点统一 fan-in 的必然结果(调用点 4 元 + 方法标识 + this + args + 返回类型);
/// 收敛为多生命周期 struct 反更晦涩,故豁免 `too_many_arguments`。
#[allow(clippy::too_many_arguments)]
fn dispatch_native(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    caller_pc: usize,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: Vec<Arg>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    let nargs: Vec<Value> = args.into_iter().map(Value::from).collect();
    let result = native::invoke(vm, class, name, desc, this, &nargs);
    finish_invoke(interp, frame, vm, caller_pc, result, return_type)
}

/// 跑被调用者解释帧:造帧 →(实例)写 `local[0]=objref` → 实参按序写入局部变量 →
/// 构造解释器(目标字节码 + 常量池 + 异常表 + 身份)→ [`run_with_depth`] 递归 →
/// [`finish_invoke`] 回填 / 捕获。`objref=None`(静态)实参自 slot 0;`Some`(实例)
/// `local[0]=objref`、实参自 slot 1。static/special/virtual/interface 共用。
///
/// 参数多系 4 处调用点统一 fan-in 的必然结果;同 [`dispatch_native`] 豁免 `too_many_arguments`。
#[allow(clippy::too_many_arguments)]
fn run_callee(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    caller_pc: usize,
    target_lc: &LoadedClass,
    target_method: &MethodInfo,
    code: &CodeAttribute,
    objref: Option<Reference>,
    args: Vec<Arg>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    let mut callee = Frame::new(code.max_locals, code.max_stack);
    let mut slot: u16 = match objref {
        Some(r) => {
            callee.locals.set_reference(0, r)?;
            1
        }
        None => 0,
    };
    for a in args {
        let advance = store_arg(&mut callee.locals, slot, a)?;
        slot = slot
            .checked_add(advance)
            .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
    }
    let callee_interp = Interpreter::new(&code.code, &target_lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(
            target_lc.name(),
            cp_utf8(&target_lc.cf.constant_pool, target_method.name_index)?,
        );
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm));
    finish_invoke(interp, frame, vm, caller_pc, result, return_type)
}

/// SAM 调用派发到 lambda 实现方法(对应 `LambdaMetafactory` 生成的合成类实现 SAM)。
/// rustj 沿「按名特判」综合:闭包记实现方法身份 + 捕获;SAM 调用时**捕获前置 ++ SAM 实参**
/// (合称 combined)交给实现方法执行。
///
/// 按实现方法句柄 reference_kind 分两路:
/// - `REF_INVOKE_STATIC`(lambda 体 / 静态方法引用):实现为静态,combined 即其形参。
/// - `REF_INVOKE_VIRTUAL`/`SPECIAL`/`INTERFACE`(实例方法引用 `obj::method` / `Type::method`):
///   接收者隐含——combined 的**首位**为接收者(无绑定时来自 SAM 首参,绑定时来自捕获);
///   余下为实现形参。按接收者**运行时类虚分派**(尊重覆写,同 invokevirtual)。
///
/// 实例捕获 lambda(`x -> this.f + x`)的 `this` 经 javac 编为静态实现的首参,仍走静态路径。
/// 构造器引用(`REF_newInvokeSpecial`,`Foo::new`)见下方 ctor_ref 分支:分配 + <init> + 返新实例。
fn dispatch_lambda(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    caller_pc: usize,
    lambda: LambdaOop,
    args: Vec<Arg>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    let kind = lambda.impl_kind();
    let instance_ref = matches!(
        kind,
        REF_INVOKE_VIRTUAL | REF_INVOKE_SPECIAL | REF_INVOKE_INTERFACE
    );
    let ctor_ref = kind == REF_NEW_INVOKE_SPECIAL;
    if !instance_ref && !ctor_ref && kind != REF_INVOKE_STATIC {
        return Err(VmError::BadConstant(
            "lambda 实现方法句柄种类未支持(仅 invokeStatic/Virtual/Special/Interface/NewInvokeSpecial)",
        ));
    }
    let impl_class = lambda.impl_class().to_string();
    let impl_name = lambda.impl_name().to_string();
    let impl_desc = lambda.impl_desc().to_string();

    // combined = 捕获(按捕获类型序)前置 ++ SAM 实参。
    let mut combined: Vec<Arg> = lambda
        .captures()
        .iter()
        .copied()
        .map(arg_from_value)
        .collect::<Result<Vec<_>, _>>()?;
    combined.extend(args);

    // 解析实现类初始化(声明类;实例引用的虚分派按接收者类,但初始化仍触声明类)。
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("lambda 派发需要类注册表"))?;
    clinit::ensure_class_initialized(vm, &impl_class)?;

    // 构造器引用(`Foo::new`):combined = 构造器形参;分配新实例 + 跑 <init>(void 返回),
    // 再把新实例按 SAM 返回类型回填(<init> 返 void,不能直接复用 run_callee 的回填)。
    if ctor_ref {
        let target_lc = registry
            .get(&impl_class)
            .ok_or(VmError::BadConstant("lambda 构造类未加载"))?;
        let target_method = find_method(&target_lc.cf, &impl_name, &impl_desc)?;
        let code = target_method
            .code
            .as_ref()
            .ok_or(VmError::BadConstant("lambda 构造器无 Code"))?;
        let new_ref = vm
            .heap_mut()
            .alloc(Oop::Instance(registry.new_instance(target_lc)));
        return match run_callee(
            interp,
            frame,
            vm,
            caller_pc,
            target_lc,
            target_method,
            code,
            Some(new_ref),
            combined,
            ReturnDescriptor::Void,
        )? {
            InvokeFlow::Fallthrough => finish_invoke(
                interp,
                frame,
                vm,
                caller_pc,
                Ok(Value::Reference(new_ref)),
                return_type,
            ),
            jump @ InvokeFlow::Jump(_) => Ok(jump),
        };
    }

    // (objref, 实现形参, 目标类, 目标方法):实例引用剥首位接收者 + 按其类虚分派。
    let (objref, impl_args, target_lc, target_method) = if instance_ref {
        let first = combined
            .first()
            .copied()
            .ok_or(VmError::BadConstant("实例方法引用缺接收者"))?;
        combined.remove(0);
        let Arg::Reference(recv) = first else {
            return Err(VmError::BadConstant("实例方法引用的接收者须为引用"));
        };
        let recv_class = match vm.heap().get(recv) {
            Some(Oop::Instance(i)) => i.class_name().to_string(),
            _ => return Err(VmError::BadConstant("实例方法引用接收者须为实例")),
        };
        let (lc, m) = registry
            .resolve_dispatch(&recv_class, &impl_name, &impl_desc)
            .ok_or(VmError::BadConstant("lambda 实例方法引用未解析到方法(抽象?)"))?;
        (Some(recv), combined, lc, m)
    } else {
        let lc = registry
            .get(&impl_class)
            .ok_or(VmError::BadConstant("lambda 实现类未加载"))?;
        let m = find_method(&lc.cf, &impl_name, &impl_desc)?;
        (None, combined, lc, m)
    };

    // 实现为 native(方法引用到 native,如 Object::hashCode)→ 内置 native 分派。
    if target_method.access_flags.is_native() {
        return dispatch_native(
            interp, frame, vm, caller_pc, &impl_class, &impl_name, &impl_desc, objref, impl_args,
            return_type,
        );
    }
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("lambda 实现方法无 Code(抽象)"))?;
    run_callee(interp, frame, vm, caller_pc, target_lc, target_method, code, objref, impl_args, return_type)
}

/// `Value → Arg`(闭包捕获还原为实参)。void 不可能是捕获值。
fn arg_from_value(v: Value) -> Result<Arg, VmError> {
    Ok(match v {
        Value::Int(x) => Arg::Int(x),
        Value::Long(x) => Arg::Long(x),
        Value::Float(x) => Arg::Float(x),
        Value::Double(x) => Arg::Double(x),
        Value::Reference(r) => Arg::Reference(r),
        Value::Void => return Err(VmError::BadConstant("lambda 捕获值不可为 void")),
    })
}

/// 执行 `invokedynamic`:解析调用点 → 引导方法 → 按 (类,名) 特判综合目标。
///
/// JDK 9+ 默认把动态字符串拼接编为 `invokedynamic makeConcatWithConstants`
/// (引导方法 `java/lang/invoke/StringConcatFactory.makeConcatWithConstants`)。
/// 真实 HotSpot **运行**引导方法(返 `CallSite`,链入调用点);rustj 沿用「按语义移植」
/// 决策(同 native 表特判 `JVM_*`),**按引导方法 (类,名) 特判**,直接综合(详见 spec 4.10u)。
///
/// `index` 指向 `CONSTANT_InvokeDynamic`:其 name_and_type 给**动态调用点类型**
/// (实参类型 + 返回类型,**非**引导方法描述符)。由调用方推进 `pc += 5`。
pub(super) fn invoke_dynamic(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    index: u16,
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
    let cp = interp.cp();
    let (bsm_index, _linkage_name, desc) = resolve_invoke_dynamic(cp, index)?;
    let md = parse_method_descriptor(&desc)?;

    // 动态实参按调用点描述符的形参类型弹出并翻正序(args[i] ↔ parameters[i])。
    let args = pop_args(frame, &md.parameters)?;

    // 取声明类的 BootstrapMethods 表(经 identity → registry → cf.bootstrap_methods())。
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokedynamic 需要类注册表"))?;
    let this_class = interp
        .declaring_class()
        .ok_or(VmError::BadConstant("invokedynamic 需方法身份(声明类)"))?;
    let lc = registry
        .get(this_class)
        .ok_or(VmError::BadConstant("invokedynamic 声明类未加载"))?;
    let bsm_table = lc.cf.bootstrap_methods();
    let entry = bsm_table
        .get(usize::from(bsm_index))
        .ok_or(VmError::BadConstant("BootstrapMethod 索引越界"))?;

    // 解析引导方法句柄 → (引导方法类, 名);按 (类, 名) 特判综合。
    let (bsm_class, bsm_name) = resolve_method_handle(cp, entry.bootstrap_method_ref)?;
    if bsm_class == "java/lang/invoke/StringConcatFactory" && bsm_name == "makeConcatWithConstants"
    {
        let recipe = resolve_recipe(cp, &entry.bootstrap_arguments)?;
        let result = concat_with_recipe(vm, &recipe, &args, &md.parameters)?;
        finish_invoke(interp, frame, vm, caller_pc, Ok(result), md.return_type)
    } else if bsm_class == "java/lang/invoke/LambdaMetafactory" && bsm_name == "metafactory" {
        // lambda / 函数式接口:闭包 Oop 记实现方法身份 + 捕获;SAM 调用时转发实现体(见 spec 4.10aa)。
        let result = build_lambda(vm, cp, &entry.bootstrap_arguments, &md.return_type, args)?;
        finish_invoke(interp, frame, vm, caller_pc, Ok(result), md.return_type)
    } else {
        // 未识别的引导方法(如 LambdaMetafactory.metafactory)→ 未支持。诊断打印具体
        // (类,名)以便定位下一个待实现的引导方法;返回静态错误(BadConstant 取 &'static str)。
        eprintln!(
            "[invokedynamic] 未支持的引导方法:{bsm_class}.{bsm_name} \
             (仅 StringConcatFactory.makeConcatWithConstants 已实现)"
        );
        Err(VmError::BadConstant(
            "invokedynamic 引导方法未实现(详见诊断输出)",
        ))
    }
}

/// 解析 `CONSTANT_InvokeDynamic` 条目 → `(bootstrap_method_attr_index, 调用点名, 调用点描述符)`。
fn resolve_invoke_dynamic(cp: &ConstantPool, index: u16) -> Result<(u16, String, String), VmError> {
    let ConstantPoolEntry::InvokeDynamic {
        bootstrap_method_attr_index,
        name_and_type_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("invokedynamic 操作数须为 InvokeDynamic"));
    };
    let (name, desc) = name_and_type(cp, *name_and_type_index)?;
    Ok((*bootstrap_method_attr_index, name, desc))
}

/// 解析 `CONSTANT_MethodHandle`(引导方法引用)→ (声明类内部名, 方法名)。
/// `reference_index` 指向 `Methodref`/`InterfaceMethodref`,复用 `resolve_methodref`。
fn resolve_method_handle(cp: &ConstantPool, index: u16) -> Result<(String, String), VmError> {
    let ConstantPoolEntry::MethodHandle {
        reference_kind: _,
        reference_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("引导方法引用须为 MethodHandle"));
    };
    let (class, name, _desc) = resolve_methodref(cp, *reference_index)?;
    Ok((class, name))
}

/// 解析 lambda 实现方法句柄(`metafactory` 的 `bootstrap_arguments[1]`)→
/// `(声明类, 方法名, 描述符, reference_kind)`。`reference_kind` 判静态/虚/构造器
/// 引用(本层仅派发 `REF_INVOKE_STATIC`,见 [`dispatch_lambda`])。
fn resolve_impl_handle(
    cp: &ConstantPool,
    bsm_args: &[u16],
) -> Result<(String, String, String, u8), VmError> {
    let &idx = bsm_args
        .get(1)
        .ok_or(VmError::BadConstant("metafactory 缺实现方法句柄实参"))?;
    let ConstantPoolEntry::MethodHandle {
        reference_kind,
        reference_index,
    } = cp.get(idx)?
    else {
        return Err(VmError::BadConstant("metafactory 实现实参须为 MethodHandle"));
    };
    let (class, name, desc) = resolve_methodref(cp, *reference_index)?;
    Ok((class, name, desc, *reference_kind))
}

/// factoryType 返回类型 → 函数式接口内部名(`FieldType::Class` 存裸内部名,无需剥 `L;`)。
fn interface_name_of(ret: &ReturnDescriptor) -> Result<String, VmError> {
    match ret {
        ReturnDescriptor::FieldType(FieldType::Class(name)) => Ok(name.clone()),
        _ => Err(VmError::BadConstant("metafactory factoryType 返回须为引用(函数式接口)类型")),
    }
}

/// 取 `makeConcatWithConstants` 的 recipe(首个引导实参,`CONSTANT_String`)→ owned 文本。
/// recipe 用 `` 标动态实参占位、`` 标常量占位、其余字符为字面量。
fn resolve_recipe(cp: &ConstantPool, bsm_args: &[u16]) -> Result<String, VmError> {
    let &first = bsm_args
        .first()
        .ok_or(VmError::BadConstant("makeConcatWithConstants 缺 recipe"))?;
    let ConstantPoolEntry::String { string_index } = cp.get(first)?
    else {
        return Err(VmError::BadConstant("makeConcatWithConstants recipe 须为 String"));
    };
    utf8(cp, *string_index)
}

/// 按 recipe 拼接动态实参 → `String` 引用(对应 `StringConcatFactory` 链入的拼接语义)。
/// `` 占位取下一个实参按其类型字符串化;其它字符字面量拼入;``(常量占位)
/// 少见于简单拼接,本层 best-effort 跳过(记债)。结果经 `string::intern` 规范化。
fn concat_with_recipe(
    vm: &mut Vm<'_>,
    recipe: &str,
    args: &[Arg],
    param_types: &[FieldType],
) -> Result<Value, VmError> {
    let mut out = String::new();
    let mut ai: usize = 0;
    for c in recipe.chars() {
        if c == '\u{0001}' {
            let arg = args
                .get(ai)
                .ok_or(VmError::BadConstant("recipe 占位数超过动态实参数"))?;
            let ft = param_types
                .get(ai)
                .ok_or(VmError::BadConstant("recipe 占位数超过实参类型数"))?;
            stringify_arg(vm, arg, ft, &mut out);
            ai += 1;
        } else if c == '\u{0002}' {
            // 常量占位:顺延(后续 bootstrap 常量实参;少见于简单拼接,记债)。
        } else {
            out.push(c);
        }
    }
    let r = super::string::intern(vm, &out)?;
    Ok(Value::Reference(r))
}

/// 把单个动态实参按其字段类型字符串化,追加到 `out`(对应 Java `String.valueOf` 语义)。
/// float/double 用 Rust `{:?}` 格式(**非 Java 精确**:NaN/无穷/定点规则,独立债,后续)。
fn stringify_arg(vm: &Vm<'_>, arg: &Arg, ft: &FieldType, out: &mut String) {
    use std::fmt::Write;
    match (ft, arg) {
        // 引用:null → "null"(Java 语义);非 null String → 读文本(非 String 罕见,best-effort 跳过)。
        (FieldType::Class(_) | FieldType::Array(_), Arg::Reference(r)) => {
            if r.is_null() {
                out.push_str("null");
            } else if let Ok(Some(t)) = super::string::read_text(vm, *r) {
                out.push_str(&t);
            }
        }
        (FieldType::Boolean, Arg::Int(x)) => out.push_str(if *x != 0 { "true" } else { "false" }),
        (FieldType::Char, Arg::Int(x)) => {
            if let Some(ch) = char::from_u32(*x as u32) {
                out.push(ch);
            }
        }
        (FieldType::Int | FieldType::Byte | FieldType::Short, Arg::Int(x)) => {
            let _ = write!(out, "{x}");
        }
        (FieldType::Long, Arg::Long(x)) => {
            let _ = write!(out, "{x}");
        }
        (FieldType::Float, Arg::Float(f)) => {
            let _ = write!(out, "{f:?}");
        }
        (FieldType::Double, Arg::Double(d)) => {
            let _ = write!(out, "{d:?}");
        }
        _ => {}
    }
}

/// 综合闭包对象(对应 `LambdaMetafactory.metafactory` 链入调用点返 `CallSite` 的语义)。
/// 引导实参 `[0]`=SAM 方法类型、`[1]`=实现方法句柄、`[2]`=动态方法类型;本层只用 `[1]`
/// 取实现身份。捕获 = 已按 factoryType 形参弹出的动态实参(`pop_args` 结果)。
/// 结果为新分配的 `Oop::Lambda` 引用,按调用点返回类型(函数式接口)回填。
fn build_lambda(
    vm: &mut Vm<'_>,
    cp: &ConstantPool,
    bsm_args: &[u16],
    factory_return: &ReturnDescriptor,
    captures: Vec<Arg>,
) -> Result<Value, VmError> {
    let (impl_class, impl_name, impl_desc, impl_kind) = resolve_impl_handle(cp, bsm_args)?;
    let sam_type = interface_name_of(factory_return)?;
    let captured: Vec<Value> = captures.into_iter().map(Value::from).collect();
    let lambda = LambdaOop::new(impl_class, impl_name, impl_desc, impl_kind, sam_type, captured);
    let r = vm.heap_mut().alloc(Oop::Lambda(lambda));
    Ok(Value::Reference(r))
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
    let md = parse_method_descriptor(&desc)?;
    // 实参在调用者栈上正序(arg0 底、argN 顶);弹出并翻正序(args[i] ↔ parameters[i])。
    let args = pop_args(frame, &md.parameters)?;
    // ACC_NATIVE(无 Code)→ 内置 native 分派表(移植 prims/jvm.cpp 的 JVM_* 桥);静态 native 无 this。
    if target_method.access_flags.is_native() {
        return dispatch_native(
            interp,
            frame,
            vm,
            caller_pc,
            &class_name,
            &method_name,
            &desc,
            None,
            args,
            md.return_type,
        );
    }
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("invokestatic 目标方法无 Code(抽象)"))?;

    // 递归解释被调用者:静态无 objref,实参自 slot 0 写入。沿用同一 Vm(堆 + 注册表),
    // 返回值回填 / 异常捕获经 [`finish_invoke`] 单点。
    run_callee(
        interp,
        frame,
        vm,
        caller_pc,
        target_lc,
        target_method,
        code,
        None,
        args,
        md.return_type,
    )
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

    // 实参正序弹出(argN 在顶,逆序弹后翻正序);再弹 objref。下游 native 分派的 nargs
    // 亦取此正序声明序。
    let args = pop_args(frame, &md.parameters)?;
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
    // ACC_NATIVE → 内置 native 分派表(声明类 = 解析到的目标类)。
    if target_method.access_flags.is_native() {
        return dispatch_native(
            interp,
            frame,
            vm,
            caller_pc,
            target_lc.name(),
            &method_name,
            &desc,
            Some(objref),
            args,
            md.return_type,
        );
    }
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("invokespecial 目标方法无 Code(抽象)"))?;

    // 递归解释被调用者:objref 为 local[0],实参自 slot 1 写入。
    run_callee(
        interp,
        frame,
        vm,
        caller_pc,
        target_lc,
        target_method,
        code,
        Some(objref),
        args,
        md.return_type,
    )
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

    // 实参正序弹出(argN 在顶,逆序弹后翻正序);再弹 objref。下游 native 分派的 nargs
    // 亦取此正序声明序。
    let args = pop_args(frame, &md.parameters)?;
    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }

    // Class 镜像(Oop::Class)非注册表类,经 "java/lang/Class" 的 native 表分派
    // (desiredAssertionStatus 等)。先于 runtime_class 解析(其按类链查表,镜像无类链)。
    if matches!(vm.heap().get(objref), Some(Oop::Class(_))) {
        return dispatch_native(
            interp,
            frame,
            vm,
            caller_pc,
            "java/lang/Class",
            &method_name,
            &desc,
            Some(objref),
            args,
            md.return_type,
        );
    }

    // 数组 receiver:仅 `Object.clone()` 浅拷贝(同描述符 + 复制元素槽),解锁
    // `Throwable.getOurStackTrace().clone()`(StackTraceElement[])等;其余数组方法顺延。
    if let Some(Oop::Array(a)) = vm.heap().get(objref) {
        if method_name == "clone" {
            let copy = a.clone();
            let r = vm.heap_mut().alloc(Oop::Array(copy));
            return finish_invoke(
                interp,
                frame,
                vm,
                caller_pc,
                Ok(Value::Reference(r)),
                md.return_type,
            );
        }
        return Err(VmError::BadConstant("invoke 目标为数组(仅支持 Object.clone)"));
    }

    // Lambda 闭包 receiver:捕获 ++ SAM 实参交给实现方法(lambda$<caller>$0)静态执行。
    if let Some(Oop::Lambda(lambda)) = vm.heap().get(objref).cloned() {
        return dispatch_lambda(interp, frame, vm, caller_pc, lambda, args, md.return_type);
    }

    // 运行时类 = 对象实际类(owned String,释放堆借用)。
    let runtime_class = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("invokevirtual 引用悬空"))?;
    let runtime_class = match runtime_class {
        Oop::Instance(i) => i.class_name().to_string(),
        Oop::Array(_) => unreachable!("数组 receiver 已先行 clone/顺延分派"),
        Oop::Class(_) => unreachable!("Class 镜像已先行 native 分派"),
        Oop::Lambda(_) => unreachable!("闭包 receiver 已先行 SAM 派发"),
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
    // ACC_NATIVE → 内置 native 分派表(Object.hashCode 等虚方法 native 经此)。
    if target_method.access_flags.is_native() {
        return dispatch_native(
            interp,
            frame,
            vm,
            caller_pc,
            target_lc.name(),
            &method_name,
            &desc,
            Some(objref),
            args,
            md.return_type,
        );
    }
    let Some(code) = target_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/AbstractMethodError"));
    };

    // 递归解释被调用者:objref 为 local[0],实参自 slot 1 写入。
    run_callee(
        interp,
        frame,
        vm,
        caller_pc,
        target_lc,
        target_method,
        code,
        Some(objref),
        args,
        md.return_type,
    )
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

    // 实参正序弹出(argN 在顶,逆序弹后翻正序);再弹 objref。下游 native 分派的 nargs
    // 亦取此正序声明序。
    let args = pop_args(frame, &md.parameters)?;
    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }

    // Class 镜像经 "java/lang/Class" 的 native 表分派(同 invoke_virtual)。
    if matches!(vm.heap().get(objref), Some(Oop::Class(_))) {
        return dispatch_native(
            interp,
            frame,
            vm,
            caller_pc,
            "java/lang/Class",
            &method_name,
            &desc,
            Some(objref),
            args,
            md.return_type,
        );
    }

    // 数组 receiver:仅 `Object.clone()` 浅拷贝(同 invoke_virtual)。
    if let Some(Oop::Array(a)) = vm.heap().get(objref) {
        if method_name == "clone" {
            let copy = a.clone();
            let r = vm.heap_mut().alloc(Oop::Array(copy));
            return finish_invoke(
                interp,
                frame,
                vm,
                caller_pc,
                Ok(Value::Reference(r)),
                md.return_type,
            );
        }
        return Err(VmError::BadConstant("invoke 目标为数组(仅支持 Object.clone)"));
    }

    // Lambda 闭包 receiver:捕获 ++ SAM 实参交给实现方法(lambda$<caller>$0)静态执行。
    if let Some(Oop::Lambda(lambda)) = vm.heap().get(objref).cloned() {
        return dispatch_lambda(interp, frame, vm, caller_pc, lambda, args, md.return_type);
    }

    // 运行时类 = 对象实际类(owned String,释放堆借用)。
    let runtime_class = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("invokeinterface 引用悬空"))?;
    let runtime_class = match runtime_class {
        Oop::Instance(i) => i.class_name().to_string(),
        Oop::Array(_) => unreachable!("数组 receiver 已先行 clone/顺延分派"),
        Oop::Class(_) => unreachable!("Class 镜像已先行 native 分派"),
        Oop::Lambda(_) => unreachable!("闭包 receiver 已先行 SAM 派发"),
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
    // ACC_NATIVE → 内置 native 分派表。
    if target_method.access_flags.is_native() {
        return dispatch_native(
            interp,
            frame,
            vm,
            caller_pc,
            target_lc.name(),
            &method_name,
            &desc,
            Some(objref),
            args,
            md.return_type,
        );
    }
    let Some(code) = target_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/AbstractMethodError"));
    };

    // 递归解释被调用者:objref 为 local[0],实参自 slot 1 写入。
    run_callee(
        interp,
        frame,
        vm,
        caller_pc,
        target_lc,
        target_method,
        code,
        Some(objref),
        args,
        md.return_type,
    )
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
