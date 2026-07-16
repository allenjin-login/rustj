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
use crate::oops::{ClassRegistry, LoadedClass, ArrayOop, LambdaOop, Oop};
use crate::runtime::{Frame, LocalVars, Reference, Slot, VmThread};

use super::{clinit, exception, native, string, throw_exception, Interpreter, Value, VmError};

/// 字段引用类(`MethodHandleNatives.java:103-106`),编码于 MemberName.flags 的最高 4 位
///(`flags >>> 24 & 0x0F`)。B.5.2 MH 调用钩子按之分派 getfield/putfield/getstatic/putstatic。
const REF_GET_FIELD: u8 = 1;
const REF_GET_STATIC: u8 = 2;
const REF_PUT_FIELD: u8 = 3;
const REF_PUT_STATIC: u8 = 4;
// 方法引用类(REF_INVOKE_*)经 `crate::oops::lambda` 复用导入,G.2.2 linkTo / NamedFunction 节点派发。

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
    vm: &mut VmThread,
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

/// 数组 receiver 的 Object 继承法虚分派(clone/getClass/hashCode/equals)。数组类型在 rustj 无
/// 独立 LoadedClass,其 Object 继承法由 HotSpot 数组 Klass(typeArrayKlass/objArrayKlass)承载;
/// 此处短路为等价语义(同 `java/lang/Object` 各 native)。解锁 `LambdaForm$BasicType[].getClass()`
/// 等(DMH.makePreparedFieldLambdaForm LF 准备触发)。toString 等余法顺延。
fn dispatch_array_object_method(
    vm: &mut VmThread,
    objref: Reference,
    arr: &ArrayOop,
    method_name: &str,
    args: &[Value],
) -> Result<Value, VmError> {
    match method_name {
        // clone() —— 浅拷贝(同描述符 + 复制元素槽),对应 Object.clone native。
        "clone" => {
            let r = vm.heap_mut().alloc(Oop::Array(arr.clone()));
            Ok(Value::Reference(r))
        }
        // getClass() —— 数组类型的 Class 镜像(如 `[Ljava/lang/invoke/LambdaForm$BasicType;`)。
        "getClass" => Ok(Value::Reference(vm.intern_class_mirror(arr.class_name()))),
        // hashCode() —— 对象标识(句柄 id);Object.hashCode synchronizer mode 4。
        "hashCode" => Ok(Value::Int(objref.id().unwrap_or(0) as i32)),
        // equals(Object) —— 引用相等(Object.equals 默认语义;数组未覆盖)。
        "equals" => {
            let same = matches!(args.first().copied(), Some(Value::Reference(o)) if o == objref);
            Ok(Value::Int(if same { 1 } else { 0 }))
        }
        _ => Err(VmError::BadConstant("invoke 目标为数组(仅支持 Object 继承法)")),
    }
}

/// `runtime_class` 是否为 `java/lang/invoke/DirectMethodHandle` 的(子)类(B.5.2 MH 调用钩子前置)。
/// 沿超类链上行比对(owned 类名,避开 `&lc` 借 `reg` 的链式借用);无注册表/链顶 → false。
fn is_direct_method_handle(vm: &VmThread, runtime_class: &str) -> bool {
    let Some(reg) = vm.registry() else {
        return false;
    };
    let mut cur_name = Some(runtime_class.to_string());
    while let Some(name) = cur_name {
        match reg.get(&name) {
            Some(lc) => {
                if lc.name() == "java/lang/invoke/DirectMethodHandle" {
                    return true;
                }
                cur_name = lc.super_class_name().map(|s| s.to_string());
            }
            None => break,
        }
    }
    false
}

/// `runtime_class` 是否为 `java/lang/invoke/MethodHandle` 的(子)类(G.2 LF 解释前置)。
/// 同 [`is_direct_method_handle`] 沿超类链上行比对;到链顶仍未命中 → false。覆盖 DMH/BMH/
/// 转换 adapter(AsTypeInstance 等)等所有 MH 子类——皆须拦截 invoke 族走 LF 解释或字段 shortcut。
fn is_method_handle(vm: &VmThread, runtime_class: &str) -> bool {
    let Some(reg) = vm.registry() else {
        return false;
    };
    let mut cur_name = Some(runtime_class.to_string());
    while let Some(name) = cur_name {
        match reg.get(&name) {
            Some(lc) => {
                if lc.name() == "java/lang/invoke/MethodHandle" {
                    return true;
                }
                cur_name = lc.super_class_name().map(|s| s.to_string());
            }
            None => break,
        }
    }
    false
}

/// MethodHandle 签名多态调用钩子(B.5.2 字段 DMH 短路 + G.2 LambdaForm 解释):
/// receiver 为 `java/lang/invoke/MethodHandle`(子)类、方法名 ∈ {invoke, invokeExact,
/// invokeBasic} 时拦截:
/// - 字段 DirectMethodHandle(refKind 1-4)→ 直读 `member` 做字段访问(设计 §2 shortcut,B.5.2);
/// - 其余 MH(方法 DMH / BMH / 转换 adapter / identity 等)→ [`interpret_lambda_form`](G.2)。
///
/// `Ok(Some(value))` = 已处理(调用方 `finish_invoke` 回填);`Ok(None)` = 非 MH 或方法名非
/// invoke 族 → 调用方走正常虚分派。
fn try_method_handle_invoke_hook(
    vm: &mut VmThread,
    method_name: &str,
    runtime_class: &str,
    mh_ref: Reference,
    args: &[Arg],
) -> Result<Option<Value>, VmError> {
    if !matches!(method_name, "invoke" | "invokeExact" | "invokeBasic") {
        return Ok(None);
    }
    if !is_method_handle(vm, runtime_class) {
        return Ok(None);
    }
    // 字段 DMH 快路(refKind 1-4);非字段 refKind 返 None 落 LF 解释。
    if is_direct_method_handle(vm, runtime_class)
        && let Some(v) = dispatch_method_handle_field(vm, mh_ref, args)?
    {
        return Ok(Some(v));
    }
    // 任意非字段 MH → 解释其 LambdaForm(G.2)。
    Ok(Some(interpret_lambda_form(vm, mh_ref, args)?))
}

/// 解释 MethodHandle 的 LambdaForm(G.2)。读 `mh.form`(LambdaForm)的 `arity`/`result`/
/// `names(Name[])`,按拓扑序求值:先绑入口参数(param 0 = MH 本身,param i = args[i-1]),
/// 再遍历计算节点 `names[arity..]`(function != null),最后返 `names[result]`。
///
/// **G.2.1 骨架**:identity MH 的 LF 无计算节点(`names` = 仅 MH param + arg param,
/// arity = names.len(),result = arg 下标)→ 绑参数后直接返 `names[result]`。计算节点的
/// NamedFunction 分派(`invoke_named_function`)G.2.2+ 填;遇 function != null 暂抛错。
///
/// 入口参数 1:1 绑定(LF 每个 Name 占一位,与 JVM 栈 category-2 翻倍无关);`args` 不含
/// receiver(MH 经 `mh_ref` 单独传),故 param i ∈ 1..arity 对应 args[i-1]。
fn interpret_lambda_form(
    vm: &mut VmThread,
    mh_ref: Reference,
    args: &[Arg],
) -> Result<Value, VmError> {
    // form = MethodHandle.form(MethodHandle.java:460 final 字段)。
    let form = vm
        .instance_reference_field(mh_ref, "java/lang/invoke/MethodHandle", "form")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("LF 解释:mh.form 缺失"))?;
    // arity/result/names(LambdaForm.java:128/129/132)。
    let arity = vm
        .instance_int_field(form, "java/lang/invoke/LambdaForm", "arity")
        .ok_or(VmError::BadConstant("LF 解释:form.arity 缺失"))? as usize;
    let result = vm
        .instance_int_field(form, "java/lang/invoke/LambdaForm", "result")
        .unwrap_or(-1);
    let names_arr = vm
        .instance_reference_field(form, "java/lang/invoke/LambdaForm", "names")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("LF 解释:form.names 缺失"))?;
    let names_len = match vm.heap().get(names_arr) {
        Some(Oop::Array(a)) => a.length(),
        _ => return Err(VmError::BadConstant("LF 解释:form.names 非数组")),
    };
    // values[i] = 第 i 个 Name 的求值结果。先绑入口参数:param 0 = MH;param i = args[i-1]。
    let mut values: Vec<Slot> = vec![Slot::Top; names_len];
    values[0] = Slot::Reference(mh_ref);
    for (i, slot) in values.iter_mut().enumerate().take(arity).skip(1) {
        let arg_idx = i - 1;
        if arg_idx >= args.len() {
            return Err(VmError::BadConstant("LF 解释:入口参数不足"));
        }
        *slot = arg_to_slot(args.get(arg_idx))?;
    }
    // 计算节点 names[arity..](function != null)。G.2.1 identity 无计算节点。
    for idx in arity..names_len {
        let name_ref = match vm.heap().get(names_arr) {
            Some(Oop::Array(a)) => match a.element(idx) {
                Slot::Reference(r) if !r.is_null() => r,
                _ => return Err(VmError::BadConstant("LF 解释:Name[] 元素非引用")),
            },
            _ => return Err(VmError::BadConstant("LF 解释:form.names 非数组")),
        };
        let function = vm.instance_reference_field(
            name_ref,
            "java/lang/invoke/LambdaForm$Name",
            "function",
        );
        // names[arity..] 均为计算节点(function != null):按 NamedFunction.member 分派求值(G.2.2)。
        if function.is_some_and(|r| !r.is_null()) {
            values[idx] = eval_compute_node(vm, name_ref, &values)?;
        }
    }
    // result < 0(LambdaForm.VOID_RESULT)→ void;否则返 names[result]。
    if result < 0 {
        return Ok(Value::Void);
    }
    let r = result as usize;
    if r >= names_len {
        return Err(VmError::BadConstant("LF 解释:result 下标越界"));
    }
    Ok(slot_to_value(values[r]))
}

/// 求值一个 LF 计算节点(`Name.function != null`):读 `NamedFunction.member` 并按成员身份分派
/// (G.2.2)。节点实参(`Name.arguments`)是 `Object[]`:元素为 `Name`(引用先前节点 → values[index])
/// 或字面量(装箱)。成员分派:
/// - `DirectMethodHandle.internalMemberName(mh)` → 返 `mh.member`(MemberName 引用);
/// - `MethodHandle.linkTo*` / `invokeBasic` → MH 链接器:末参为 MemberName,以其派发前置实参;
/// - 其余方法/字段成员 → 按 refKind 调用/访问(复用 DMH 字段访问逻辑)。
fn eval_compute_node(
    vm: &mut VmThread,
    name_ref: Reference,
    values: &[Slot],
) -> Result<Slot, VmError> {
    let nf = vm
        .instance_reference_field(name_ref, "java/lang/invoke/LambdaForm$Name", "function")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("LF 节点:function 缺失"))?;
    let member = vm
        .instance_reference_field(nf, "java/lang/invoke/LambdaForm$NamedFunction", "member")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("LF 节点:NamedFunction.member 缺失"))?;
    let mname = read_member_name_string(vm, member, "name");
    let mclazz = vm
        .instance_reference_field(member, "java/lang/invoke/MemberName", "clazz")
        .and_then(|c| vm.mirror_internal_name(c))
        .unwrap_or_default();
    let arg_slots = resolve_name_arguments(vm, name_ref, values)?;

    // MemberName.flags 高 4 位 = refKind(直接节点用;linkTo/internalMemberName 不读)。
    let flags = vm
        .instance_int_field(member, "java/lang/invoke/MemberName", "flags")
        .unwrap_or(0);
    let ref_kind = ((flags as u32) >> 24) as u8 & 0x0F;

    match (mclazz.as_str(), mname.as_str()) {
        // DirectMethodHandle.internalMemberName(mh) → 返 mh.member(LF 取 DMH 尾 MemberName 用)。
        ("java/lang/invoke/DirectMethodHandle", "internalMemberName") => {
            let target = match arg_slots.first() {
                Some(Slot::Reference(r)) => *r,
                _ => return Err(VmError::BadConstant("internalMemberName:缺 receiver")),
            };
            let m = vm
                .instance_reference_field(target, "java/lang/invoke/DirectMethodHandle", "member")
                .ok_or(VmError::BadConstant("internalMemberName:receiver 无 member 字段"))?;
            Ok(Slot::Reference(m))
        }
        // invokeBasic(targetMH, args...):签名多态原语 —— arg[0] 为目标 MethodHandle,语义 = 递归
        // 解释目标 MH 的 LambdaForm,arg[1..] 为其入口参数(无 MemberName、无类型检查)。对应 HotSpot
        // LF 解释 `Name(invokeBasic,[mh,a1,...])` = `mh.invokeBasic(a1,...)`。转换 BMH 的 LF 即用此
        // 调所绑定的底层 DMH(`Name(argL1)` 取出 → `invokeBasic(DMH, receiver)`)。
        ("java/lang/invoke/MethodHandle", "invokeBasic") => {
            let target = match arg_slots.first() {
                Some(Slot::Reference(r)) if !r.is_null() => *r,
                _ => return Err(VmError::BadConstant("invokeBasic:缺目标 MH")),
            };
            let rest: Vec<Arg> = arg_slots[1..]
                .iter()
                .map(|s| slot_to_arg(*s))
                .collect::<Result<_, _>>()?;
            Ok(value_to_slot(interpret_lambda_form(vm, target, &rest)?))
        }
        // linkTo*:末参为 MemberName,以其派发前置实参(HotSpot linkTo 内联)。
        ("java/lang/invoke/MethodHandle", m) if m.starts_with("linkTo") => {
            link_to_member(vm, &arg_slots)
        }
        // 直接节点:getterFunction/方法 NamedFunction —— member 自身的 refKind 决定字段读/方法调。
        // 此为 LF 求值的核心:常量 LF 的 `Name(getterFunction, carrier)` 即此路径(refKind=getField)。
        _ => {
            let args: Vec<Arg> = arg_slots
                .iter()
                .map(|s| slot_to_arg(*s))
                .collect::<Result<_, _>>()?;
            invoke_member_name(vm, member, ref_kind, &args).map(value_to_slot)
        }
    }
}

/// 读 MemberName 的字符串型字段(`name` / `type` 等)→ 解码 String 池文本。
fn read_member_name_string(vm: &mut VmThread, member: Reference, field: &str) -> String {
    vm.instance_reference_field(member, "java/lang/invoke/MemberName", field)
        .and_then(|r| string::read_text(vm, r).ok().flatten())
        .unwrap_or_default()
}

/// 解析 LF 计算节点实参 `Name.arguments`(Object[])→ 槽位向量。元素为 `Name`(引用先前节点 →
/// values[index])或字面量(装箱对象 → 拆箱)。Name.index(short)定位 values。
fn resolve_name_arguments(
    vm: &mut VmThread,
    name_ref: Reference,
    values: &[Slot],
) -> Result<Vec<Slot>, VmError> {
    let arr = vm
        .instance_reference_field(name_ref, "java/lang/invoke/LambdaForm$Name", "arguments")
        .filter(|r| !r.is_null());
    let Some(arr) = arr else {
        return Ok(Vec::new());
    };
    let len = match vm.heap().get(arr) {
        Some(Oop::Array(a)) => a.length(),
        _ => return Ok(Vec::new()),
    };
    let mut out = Vec::with_capacity(len);
    for k in 0..len {
        let elem = match vm.heap().get(arr) {
            Some(Oop::Array(a)) => a.element(k),
            _ => return Err(VmError::BadConstant("LF 节点:arguments 非数组")),
        };
        let slot = match elem {
            Slot::Reference(r) if !r.is_null() => {
                let is_name = matches!(
                    vm.heap().get(r),
                    Some(Oop::Instance(i)) if i.class_name() == "java/lang/invoke/LambdaForm$Name"
                );
                if is_name {
                    let ni = vm
                        .instance_int_field(r, "java/lang/invoke/LambdaForm$Name", "index")
                        .ok_or(VmError::BadConstant("LF 节点:Name.index 缺失"))?
                        as usize;
                    values
                        .get(ni)
                        .copied()
                        .ok_or(VmError::BadConstant("LF 节点:Name.index 越界"))?
                } else {
                    // 字面量装箱 → 拆箱(Integer/Long/Float/Double 的 value 字段)。
                    unbox_literal(vm, r)?
                }
            }
            other => other,
        };
        out.push(slot);
    }
    Ok(out)
}

/// 拆箱字面量实参(Integer/Long/Float/Double/Short/Byte/Boolean/Character → value 槽位)。
fn unbox_literal(vm: &mut VmThread, r: Reference) -> Result<Slot, VmError> {
    let (class_name, value_ord) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("拆箱需类注册表"))?;
        let heap = vm.heap();
        let inst = match heap.get(r) {
            Some(Oop::Instance(i)) => i,
            _ => return Err(VmError::BadConstant("拆箱:非 Instance")),
        };
        let cn = inst.class_name().to_string();
        let lc = reg
            .get(&cn)
            .ok_or(VmError::BadConstant("拆箱:类未加载"))?;
        let ord = reg
            .flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == "value")
            .ok_or(VmError::BadConstant("拆箱:无 value 字段"))?;
        (cn, ord)
    };
    let slot = match vm.heap().get(r) {
        Some(Oop::Instance(i)) => i.field(value_ord),
        _ => return Err(VmError::BadConstant("拆箱:非 Instance")),
    };
    // 仅校验声明类是已知装箱类型(值槽位已按 JVM 承载类型存)。
    match class_name.as_str() {
        "java/lang/Integer" | "java/lang/Long" | "java/lang/Float" | "java/lang/Double"
        | "java/lang/Short" | "java/lang/Byte" | "java/lang/Boolean" | "java/lang/Character" => Ok(slot),
        _ => Err(VmError::BadConstant("拆箱:非已知装箱类型")),
    }
}

/// `MethodHandle.linkTo*` / `invokeBasic` 链接器(G.2.2):末参为 MemberName,据其 refKind 派发
/// 前置实参(字段读/写、方法调用)。对应 HotSpot `MethodHandles::linkTo*` 内联:跳转到 MemberName
/// 的 vmtarget/vmindex。rustj 直读 MemberName.{clazz,name,flags,type} 派发(同 DMH 字段 shortcut)。
fn link_to_member(vm: &mut VmThread, arg_slots: &[Slot]) -> Result<Slot, VmError> {
    let mname_ref = match arg_slots.last() {
        Some(Slot::Reference(r)) if !r.is_null() => *r,
        _ => return Err(VmError::BadConstant("linkTo:末参非 MemberName")),
    };
    let flags = vm
        .instance_int_field(mname_ref, "java/lang/invoke/MemberName", "flags")
        .ok_or(VmError::BadConstant("linkTo:MemberName.flags 缺失"))?;
    let ref_kind = ((flags as u32) >> 24) as u8 & 0x0F;
    let call_args: Vec<Arg> = arg_slots[..arg_slots.len() - 1]
        .iter()
        .map(|s| slot_to_arg(*s))
        .collect::<Result<_, _>>()?;
    invoke_member_name(vm, mname_ref, ref_kind, &call_args).map(value_to_slot)
}

/// 按 MemberName 的 refKind 派发字段访问 / 方法调用(G.2.2 核心)。linkTo 链接器与直接
/// NamedFunction 节点(getterFunction 等)共用此路径——二者仅 MemberName 来源不同(末参 vs
/// `function.member`),派发逻辑一致。字段 refKind 复用 DMH 字段访问;方法 refKind 解析目标方法
/// 后 `interpret_with` 跑真字节码(invokeStatic=物种工厂 make / invokeVirtual=虚分派 / newInvokeSpecial
/// =构造器先分配再跑 `<init>`)。
fn invoke_member_name(
    vm: &mut VmThread,
    mname: Reference,
    ref_kind: u8,
    args: &[Arg],
) -> Result<Value, VmError> {
    match ref_kind {
        REF_GET_FIELD | REF_PUT_FIELD | REF_GET_STATIC | REF_PUT_STATIC => {
            let declaring = vm
                .instance_reference_field(mname, "java/lang/invoke/MemberName", "clazz")
                .and_then(|c| vm.mirror_internal_name(c))
                .ok_or(VmError::BadConstant("MemberName.clazz 非镜像"))?;
            let field_name = read_member_name_string(vm, mname, "name");
            match ref_kind {
                REF_GET_FIELD | REF_PUT_FIELD => {
                    access_instance_field(vm, &declaring, &field_name, ref_kind, args)
                }
                REF_GET_STATIC | REF_PUT_STATIC => {
                    access_static_field(vm, &declaring, &field_name, ref_kind, args)
                }
                _ => unreachable!(),
            }
        }
        REF_INVOKE_STATIC
        | REF_INVOKE_VIRTUAL
        | REF_INVOKE_SPECIAL
        | REF_NEW_INVOKE_SPECIAL
        | REF_INVOKE_INTERFACE => invoke_method_ref(vm, mname, ref_kind, args),
        _ => Err(VmError::BadConstant("MemberName:未知 refKind")),
    }
}

/// 按 MemberName 调用方法(G.2.2):解析目标方法(MemberName.{clazz,name,type→描述符})、建帧、
/// `interpret_with`。invokeStatic 在声明类解析;invokeVirtual/Interface 按接收者运行时类虚分派;
/// invokeSpecial 在声明类非虚解析;newInvokeSpecial 先分配新实例(构造器 `<init>`)。
fn invoke_method_ref(
    vm: &mut VmThread,
    mname: Reference,
    ref_kind: u8,
    args: &[Arg],
) -> Result<Value, VmError> {
    let declaring = vm
        .instance_reference_field(mname, "java/lang/invoke/MemberName", "clazz")
        .and_then(|c| vm.mirror_internal_name(c))
        .ok_or(VmError::BadConstant("MemberName.clazz 非镜像"))?;
    let method_name = read_member_name_string(vm, mname, "name");
    let desc = member_name_method_descriptor(vm, mname)?;

    // 解析目标方法(返回 owned Arc<LoadedClass> + 方法下标,后续 &mut vm 须先释)。
    let (is_static, is_constructor, receiver, resolve_class, method_args): (
        bool,
        bool,
        Option<Reference>,
        String,
        &[Arg],
    ) = match ref_kind {
        REF_INVOKE_STATIC => {
            clinit::ensure_class_initialized(vm, &declaring)?;
            (true, false, None, declaring.clone(), args)
        }
        REF_NEW_INVOKE_SPECIAL => {
            // 构造器:先分配新实例作 locals[0],其余实参为 <init> 参数。
            clinit::ensure_class_initialized(vm, &declaring)?;
            let reg = vm
                .registry()
                .ok_or(VmError::BadConstant("linkTo 方法调用需类注册表"))?;
            let lc = reg
                .get(&declaring)
                .ok_or(VmError::BadConstant("linkTo:声明类未加载"))?;
            let inst = reg.new_instance(&lc);
            let new_ref = vm.heap_mut().alloc(Oop::Instance(inst));
            (true, true, Some(new_ref), declaring.clone(), args)
        }
        REF_INVOKE_VIRTUAL | REF_INVOKE_INTERFACE => {
            // 虚分派:按接收者(arg0)运行时类解析。
            let recv = match args.first() {
                Some(Arg::Reference(r)) if !r.is_null() => *r,
                _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
            };
            let runtime_class = match vm.heap().get(recv) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                _ => return Err(VmError::BadConstant("虚分派:接收者非 Instance")),
            };
            (false, false, Some(recv), runtime_class, args)
        }
        REF_INVOKE_SPECIAL => {
            // 非虚:在声明类解析;接收者 = arg0。
            let recv = match args.first() {
                Some(Arg::Reference(r)) if !r.is_null() => *r,
                _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
            };
            (false, false, Some(recv), declaring.clone(), args)
        }
        _ => unreachable!(),
    };

    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("linkTo 方法调用需类注册表"))?;
    let (target_lc, target_idx) = match reg.resolve_dispatch(&resolve_class, &method_name, &desc) {
        Some(x) => x,
        None => {
            return Err(throw_exception(vm, "java/lang/NoSuchMethodError"));
        }
    };
    let target_method = &target_lc.cf.methods[target_idx];
    // ACC_NATIVE(无 Code;如 `Unsafe.getInt(Object,J)`)→ 内置 native 分派表(同正常 invoke 路径):
    // 静态 this=None nargs=全部实参;实例 this=receiver、nargs=实参去 receiver。声明类 = 解析目标类。
    if target_method.access_flags.is_native() {
        let this = if is_static { None } else { receiver };
        let nargs: Vec<Value> = if is_static {
            method_args.iter().copied().map(Value::from).collect()
        } else {
            method_args
                .iter()
                .skip(1)
                .copied()
                .map(Value::from)
                .collect()
        };
        return native::invoke(vm, &resolve_class, &method_name, &desc, this, &nargs);
    }
    let Some(code) = target_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/AbstractMethodError"));
    };
    let interp = Interpreter::new(&code.code, &target_lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    // 写 locals:实例方法/构造器 locals[0]=接收者;静态从 0 起。实参按 category-2 占槽(long/double=2)。
    let mut slot: u16 = 0;
    if !is_static {
        let recv = receiver.ok_or(VmError::BadConstant("实例调用缺接收者"))?;
        frame.locals.set_reference(slot, recv)?;
        slot += 1;
    }
    let first_arg = if is_static { 0 } else { 1 };
    for (i, arg) in method_args.iter().enumerate().skip(first_arg) {
        slot += store_arg(&mut frame.locals, slot, *arg)?;
        let _ = i;
    }
    let result = interp.interpret_with(&mut frame, vm)?;
    if is_constructor {
        // 构造器返新实例(<init> 返 void,实例经 locals[0] 持有)。
        return Ok(Value::Reference(
            receiver.ok_or(VmError::BadConstant("构造器调用缺新实例"))?,
        ));
    }
    Ok(result)
}

/// 读 MemberName.type → 方法描述符 `(...)R`。type 为 String(直接描述符)或 MethodType
/// (rtype:Class + ptypes:Class[] → 拼描述符)。字段 MemberName 不调此(其 type 为 Class)。
fn member_name_method_descriptor(vm: &mut VmThread, mname: Reference) -> Result<String, VmError> {
    let type_ref = vm
        .instance_reference_field(mname, "java/lang/invoke/MemberName", "type")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("MemberName.type 缺失"))?;
    // type 为 String → 直接描述符。
    let type_class = match vm.heap().get(type_ref) {
        Some(Oop::Instance(i)) => i.class_name().to_string(),
        _ => return Err(VmError::BadConstant("MemberName.type 非 Instance")),
    };
    if type_class == "java/lang/String" {
        return string::read_text(vm, type_ref)?
            .ok_or(VmError::BadConstant("MemberName.type 非字符串"));
    }
    if !type_class.contains("MethodType") {
        return Err(VmError::BadConstant("MemberName.type 非 MethodType/String"));
    }
    // MethodType:rtype(Class) + ptypes(Class[]) → 描述符。
    let rtype_mirror = vm
        .instance_reference_field(type_ref, "java/lang/invoke/MethodType", "rtype")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("MethodType.rtype 缺失"))?;
    let ptypes_arr = vm
        .instance_reference_field(type_ref, "java/lang/invoke/MethodType", "ptypes")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("MethodType.ptypes 缺失"))?;
    let rdesc = class_mirror_descriptor(vm, rtype_mirror)?;
    let plen = match vm.heap().get(ptypes_arr) {
        Some(Oop::Array(a)) => a.length(),
        _ => return Err(VmError::BadConstant("MethodType.ptypes 非数组")),
    };
    let mut pdescs = Vec::with_capacity(plen);
    for k in 0..plen {
        let pmirror = match vm.heap().get(ptypes_arr) {
            Some(Oop::Array(a)) => match a.element(k) {
                Slot::Reference(r) if !r.is_null() => r,
                _ => return Err(VmError::BadConstant("ptypes 元素非引用")),
            },
            _ => return Err(VmError::BadConstant("MethodType.ptypes 非数组")),
        };
        pdescs.push(class_mirror_descriptor(vm, pmirror)?);
    }
    Ok(format!("({}){rdesc}", pdescs.join("")))
}

/// Class 镜像 → 字段/方法描述符中的类型编码。原语("int"→"I" 等)、数组(内部名即描述符)、
/// 其余 → `Lname;`。原语镜像经 `mirror_internal_name` 返 "int"/"long"/...;数组返 "[..."。
fn class_mirror_descriptor(vm: &VmThread, mirror: Reference) -> Result<String, VmError> {
    let internal = vm
        .mirror_internal_name(mirror)
        .ok_or(VmError::BadConstant("Class 镜像:无内部名"))?;
    Ok(match internal.as_str() {
        "int" => "I".into(),
        "long" => "J".into(),
        "float" => "F".into(),
        "double" => "D".into(),
        "boolean" => "Z".into(),
        "byte" => "B".into(),
        "char" => "C".into(),
        "short" => "S".into(),
        "void" => "V".into(),
        s if s.starts_with('[') => s.to_string(),
        other => format!("L{other};"),
    })
}

/// 槽位 → 调用实参(字段/方法调用用;与 arg_to_slot 互逆)。
fn slot_to_arg(slot: Slot) -> Result<Arg, VmError> {
    Ok(match slot {
        Slot::Int(v) => Arg::Int(v),
        Slot::Long(v) => Arg::Long(v),
        Slot::Float(v) => Arg::Float(v),
        Slot::Double(v) => Arg::Double(v),
        Slot::Reference(r) => Arg::Reference(r),
        Slot::Top | Slot::ReturnAddress(_) => {
            return Err(VmError::BadConstant("LF:Top/ReturnAddress 不可作实参"))
        }
    })
}

/// 解释器值 → 槽位(计算节点结果回填用)。
fn value_to_slot(v: Value) -> Slot {
    match v {
        Value::Int(x) => Slot::Int(x),
        Value::Long(x) => Slot::Long(x),
        Value::Float(x) => Slot::Float(x),
        Value::Double(x) => Slot::Double(x),
        Value::Reference(r) => Slot::Reference(r),
        Value::Void => Slot::Top,
    }
}

/// 读 DMH.`member` → MemberName.{clazz,name,flags} → 按 refKind 做字段访问。
/// MemberName 字段布局:clazz=声明类镜像 / name=字段名 String / flags=mods|MN_IS_FIELD|(refKind<<24)
/// (B.5.1 init_from_field 置 clazz+flags;Java 侧构造器再置 name+type;makeSetter 经 changeReferenceKind
/// 把 getter refKind 转 putter)。
fn dispatch_method_handle_field(
    vm: &mut VmThread,
    mh_ref: Reference,
    args: &[Arg],
) -> Result<Option<Value>, VmError> {
    // member = DirectMethodHandle.member(DirectMethodHandle.java:55 final 字段)。
    let member = vm
        .instance_reference_field(mh_ref, "java/lang/invoke/DirectMethodHandle", "member")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("MH 钩子:DMH.member 缺失"))?;
    // flags → refKind(MemberName.java:242 getReferenceKind:flags>>>24 & MN_REFERENCE_KIND_MASK=0x0F)。
    let flags = vm
        .instance_int_field(member, "java/lang/invoke/MemberName", "flags")
        .ok_or(VmError::BadConstant("MH 钩子:MemberName.flags 缺失"))?;
    let ref_kind = ((flags as u32) >> 24) as u8 & 0x0F;
    // 字段 refKind(1-4)→ 字段访问;方法 refKind(5-9)→ None 交 LF 解释(G.2)。
    if !matches!(
        ref_kind,
        REF_GET_FIELD | REF_PUT_FIELD | REF_GET_STATIC | REF_PUT_STATIC
    ) {
        return Ok(None);
    }
    // clazz → 声明类内部名(镜像经 ClassMirrors 反查 ref→name)。
    let clazz_mirror = vm
        .instance_reference_field(member, "java/lang/invoke/MemberName", "clazz")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("MH 钩子:MemberName.clazz 缺失"))?;
    let declaring = vm
        .mirror_internal_name(clazz_mirror)
        .ok_or(VmError::BadConstant("MH 钩子:clazz 非镜像"))?;
    // name → 字段名(String 池解码)。
    let name_ref = vm
        .instance_reference_field(member, "java/lang/invoke/MemberName", "name")
        .filter(|r| !r.is_null())
        .ok_or(VmError::BadConstant("MH 钩子:MemberName.name 缺失"))?;
    let field_name = string::read_text(vm, name_ref)?
        .ok_or(VmError::BadConstant("MH 钩子:MemberName.name 非字符串"))?;

    let v = match ref_kind {
        REF_GET_FIELD | REF_PUT_FIELD => access_instance_field(vm, &declaring, &field_name, ref_kind, args)?,
        REF_GET_STATIC | REF_PUT_STATIC => access_static_field(vm, &declaring, &field_name, ref_kind, args)?,
        _ => unreachable!(),
    };
    Ok(Some(v))
}

/// 实例字段访问:getField 读 obj.field;putField 写 obj.field=value。obj 经 args[0],value 经 args[1]。
/// 字段序号在**声明类**扁平布局中定位——其布局是运行时子类布局的前缀(超类链置前),故同一序号
/// 对子类实例同样有效(同 getfield/putfield 语义)。
fn access_instance_field(
    vm: &mut VmThread,
    declaring: &str,
    field_name: &str,
    ref_kind: u8,
    args: &[Arg],
) -> Result<Value, VmError> {
    let ord = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("MH 钩子需类注册表"))?;
        let lc = reg
            .get(declaring)
            .ok_or(VmError::BadConstant("MH 钩子:声明类未加载"))?;
        reg.flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == field_name)
            .ok_or(VmError::BadConstant("MH 钩子:实例字段未找到"))?
    };
    let obj = match args.first() {
        Some(Arg::Reference(r)) => *r,
        _ => return Err(VmError::BadConstant("MH 钩子:实例字段访问缺 obj 实参")),
    };
    if obj.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    match ref_kind {
        REF_GET_FIELD => {
            let slot = match vm.heap().get(obj) {
                Some(Oop::Instance(i)) => i.field(ord),
                _ => return Err(VmError::BadConstant("MH 钩子:obj 非 Instance")),
            };
            Ok(slot_to_value(slot))
        }
        REF_PUT_FIELD => {
            let value = arg_to_slot(args.get(1))?;
            match vm.heap_mut().get_mut(obj) {
                Some(Oop::Instance(i)) => i.set_field(ord, value),
                _ => return Err(VmError::BadConstant("MH 钩子:obj 非 Instance")),
            }
            Ok(Value::Void)
        }
        _ => unreachable!(),
    }
}

/// 静态字段访问:getStatic 读 declaring.field;putStatic 写 =value。value 经 args[0]。
/// 沿超类链按名定位(声明类即 member.clazz,保守沿链兼容继承静态字段);首次访问触发 `<clinit>`
/// (同 getstatic/putstatic)。
fn access_static_field(
    vm: &mut VmThread,
    declaring: &str,
    field_name: &str,
    ref_kind: u8,
    args: &[Arg],
) -> Result<Value, VmError> {
    clinit::ensure_class_initialized(vm, declaring)?;
    let (lc_name, ord) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("MH 钩子需类注册表"))?;
        resolve_static_field_by_name(&reg, declaring, field_name)
            .ok_or(VmError::BadConstant("MH 钩子:静态字段未找到"))?
    };
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("MH 钩子需类注册表"))?;
    let lc = reg
        .get(&lc_name)
        .ok_or(VmError::BadConstant("MH 钩子:静态字段声明类未加载"))?;
    match ref_kind {
        REF_GET_STATIC => {
            let slot = lc
                .static_storage
                .lock()
                .unwrap()
                .get(ord)
                .copied()
                .ok_or(VmError::BadConstant("MH 钩子:静态槽越界"))?;
            Ok(slot_to_value(slot))
        }
        REF_PUT_STATIC => {
            let value = arg_to_slot(args.first())?;
            lc.static_storage.lock().unwrap()[ord] = value;
            Ok(Value::Void)
        }
        _ => unreachable!(),
    }
}

/// 沿超类链按名定位静态字段 → (声明类名, 序号)。owned 返回(避开 &lc 借 &reg 链式借用)。
fn resolve_static_field_by_name(
    reg: &ClassRegistry,
    start: &str,
    name: &str,
) -> Option<(String, usize)> {
    let mut cur_name = Some(start.to_string());
    while let Some(class_name) = cur_name {
        if let Some(lc) = reg.get(&class_name) {
            if let Some(ord) = lc.static_fields().iter().position(|f| f.name == name) {
                return Some((lc.name().to_string(), ord));
            }
            cur_name = lc.super_class_name().map(|s| s.to_string());
        } else {
            break;
        }
    }
    None
}

/// `invokestatic` 方法解析(JVMS §5.4.3.4):沿超类链 → 再遍历超接口,按 (名,描述符) 找**声明类**
/// 的内部名。编译器生成码(如 Class-File API 的 impl 类)常把 `invokestatic` 引用指向继承该静态法的
/// 子类(JLS 源码层调超类静态法,javac 仍可能经桥/合成路径下标到子类);故解析须上行,而非只查引用类
/// 自身。owned 返回(避开 `&lc` 借 `&reg` 链式借用)。未命中 → None(调用方报"未找到目标方法")。
fn find_static_method_owner(
    reg: &ClassRegistry,
    start: &str,
    name: &str,
    desc: &str,
) -> Option<String> {
    // 1. 超类链(含 start 自身)。
    let mut cur_name = Some(start.to_string());
    while let Some(class_name) = cur_name {
        if let Some(lc) = reg.get(&class_name) {
            if find_method(&lc.cf, name, desc).is_ok() {
                return Some(class_name);
            }
            cur_name = lc.super_class_name().map(|s| s.to_string());
        } else {
            break;
        }
    }
    // 2. 超接口(传递性,声明序):递归收集后逐个查。
    let mut ifaces = Vec::new();
    collect_interfaces(reg, start, &mut ifaces);
    for iface in ifaces {
        if let Some(lc) = reg.get(&iface)
            && find_method(&lc.cf, name, desc).is_ok()
        {
            return Some(iface);
        }
    }
    None
}

/// 递归收集 `start`(沿超类链各类)的直接 + 传递超接口,保留声明顺序、去重。
fn collect_interfaces(reg: &ClassRegistry, start: &str, out: &mut Vec<String>) {
    let mut cur_name = Some(start.to_string());
    while let Some(class_name) = cur_name {
        if let Some(lc) = reg.get(&class_name) {
            for iface in lc.interface_names() {
                if !out.contains(&iface) {
                    out.push(iface.clone());
                    collect_interfaces(reg, &iface, out);
                }
            }
            cur_name = lc.super_class_name().map(|s| s.to_string());
        } else {
            break;
        }
    }
}

/// 槽位 → 解释器值(字段读取;Top/ReturnAddress 不会出现在字段值中,映射为 Void 由调用方类型校验兜底)。
fn slot_to_value(slot: Slot) -> Value {
    match slot {
        Slot::Int(v) => Value::Int(v),
        Slot::Long(v) => Value::Long(v),
        Slot::Float(v) => Value::Float(v),
        Slot::Double(v) => Value::Double(v),
        Slot::Reference(r) => Value::Reference(r),
        Slot::Top | Slot::ReturnAddress(_) => Value::Void,
    }
}

/// 实参 → 槽位(字段写入;按 JVM 栈承载类型 1:1 映射,byte/char/short/boolean 均以 int 承载)。
fn arg_to_slot(arg: Option<&Arg>) -> Result<Slot, VmError> {
    Ok(match arg {
        Some(Arg::Int(v)) => Slot::Int(*v),
        Some(Arg::Long(v)) => Slot::Long(*v),
        Some(Arg::Float(v)) => Slot::Float(*v),
        Some(Arg::Double(v)) => Slot::Double(*v),
        Some(Arg::Reference(r)) => Slot::Reference(*r),
        None => return Err(VmError::BadConstant("MH 钩子:setter 缺 value 实参")),
    })
}

/// 进入一帧:`frame_depth +1`,执行 `f`,返回前 `−1`(Ok/Err 两路对称)。
/// `frame_depth >= stack_limit` 时直接抛 `java/lang/StackOverflowError`
/// ([`VmError::ThrownException`]),不进入 `f`。
pub(crate) fn run_with_depth<R>(
    vm: &mut VmThread,
    f: impl FnOnce(&mut VmThread) -> Result<R, VmError>,
) -> Result<R, VmError> {
    if vm.thread.frame_depth >= vm.thread.stack_limit {
        return Err(throw_exception(vm, "java/lang/StackOverflowError"));
    }
    vm.thread.frame_depth += 1;
    let r = f(vm);
    vm.thread.frame_depth -= 1;
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

/// 同 [`find_method`] 但返回方法在 `cf.methods` 中的下标(供返回 `Arc<LoadedClass>` 的解析路径
/// 解构为 `(Arc, usize)` 后再下标取 `&MethodInfo`,避免自引用元组)。
fn find_method_index(cf: &ClassFile, name: &str, desc: &str) -> Result<usize, VmError> {
    cf.methods
        .iter()
        .position(|m| method_matches(cf, m, name, desc))
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
    vm: &mut VmThread,
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
/// 跑被调用者解释帧至返回,得原始 `Value`(**不** `finish_invoke`)。供 [`run_callee`] 与
/// [`dispatch_lambda`] 共用——后者须在返回前做 lambda 装箱/拆箱适配(G.4.1),故拆出原始值。
fn run_callee_to_value(
    vm: &mut VmThread,
    target_lc: &LoadedClass,
    target_method: &MethodInfo,
    code: &CodeAttribute,
    objref: Option<Reference>,
    args: Vec<Arg>,
) -> Result<Value, VmError> {
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
    run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm))
}

/// 跑被调用者解释帧 + `finish_invoke` 回填/捕获(正常 invoke 系列直接调用点)。
/// lambda 派发(`dispatch_lambda`)**不**经此法——须做返回装箱适配(G.4.1),改走
/// [`run_callee_to_value`] + [`adapt_lambda_return`] + [`finish_invoke`]。
#[allow(clippy::too_many_arguments)]
fn run_callee(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut VmThread,
    caller_pc: usize,
    target_lc: &LoadedClass,
    target_method: &MethodInfo,
    code: &CodeAttribute,
    objref: Option<Reference>,
    args: Vec<Arg>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    let result = run_callee_to_value(vm, target_lc, target_method, code, objref, args);
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
    vm: &mut VmThread,
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
            .alloc(Oop::Instance(registry.new_instance(&target_lc)));
        return match run_callee(
            interp,
            frame,
            vm,
            caller_pc,
            &target_lc,
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

    // (objref, 实现形参, 目标类, 目标方法下标):实例引用剥首位接收者 + 按其类虚分派。
    // 元组用 `(Arc, usize)` 而非 `(Arc, &MethodInfo)`——后者 `&MethodInfo` 借自 `Arc` 自引用,
    // 无法与 `Arc` 同存于元组(move 出 Arc 即悬垂)。下标在块外统一取 `&MethodInfo`。
    let (objref, impl_args, target_lc, target_method_idx) = if instance_ref {
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
        let (lc, idx) = registry
            .resolve_dispatch(&recv_class, &impl_name, &impl_desc)
            .ok_or(VmError::BadConstant("lambda 实例方法引用未解析到方法(抽象?)"))?;
        (Some(recv), combined, lc, idx)
    } else {
        let lc = registry
            .get(&impl_class)
            .ok_or(VmError::BadConstant("lambda 实现类未加载"))?;
        let idx = find_method_index(&lc.cf, &impl_name, &impl_desc)?;
        (None, combined, lc, idx)
    };
    let target_method = &target_lc.cf.methods[target_method_idx];

    // G.4.1 lambda 适配器(签名适配):按 impl 形参类型对 SAM 实参拆箱。`Integer::sum` 为
    // `(II)I`,SAM `BiFunction.apply` 传装箱 `Integer` 引用 → 读 `value` 字段拆箱为 int,
    // 否则 `iload` 在 Reference 槽 `get_int` → `Frame(TypeMismatch)`。类型已匹配(引用→引用 /
    // 原语→原语)→ 直传;捕获前置按 factoryType 类型,同适配无害(引用形参+引用实参→直传)。
    let impl_md = parse_method_descriptor(&impl_desc)?;
    let impl_args = adapt_lambda_args(vm, impl_args, &impl_md.parameters)?;

    // 实现为 native(方法引用到 native,如 Object::hashCode)→ 内置 native 分派 + 返回装箱。
    if target_method.access_flags.is_native() {
        let nargs: Vec<Value> = impl_args.into_iter().map(Value::from).collect();
        let raw = native::invoke(vm, &impl_class, &impl_name, &impl_desc, objref, &nargs);
        let adapted = match raw {
            Ok(v) => adapt_lambda_return(vm, v, &impl_md.return_type, &return_type),
            Err(e) => Err(e),
        };
        return finish_invoke(interp, frame, vm, caller_pc, adapted, return_type);
    }
    let code = target_method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("lambda 实现方法无 Code(抽象)"))?;
    let raw = run_callee_to_value(vm, &target_lc, target_method, code, objref, impl_args)?;
    let adapted = adapt_lambda_return(vm, raw, &impl_md.return_type, &return_type)?;
    finish_invoke(interp, frame, vm, caller_pc, Ok(adapted), return_type)
}

/// G.4.1 lambda 适配器:按 impl 形参类型对 SAM 实参做拆箱。SAM 传装箱引用、impl 形参为原语 →
/// 读包装类 `value` 字段拆箱(I/Z/B/C/S→Int、J→Long、F→Float、D→Double,null→NPE);类型已匹配
/// → 直传。对应 LambdaForm 的 unbox 节点(`LambdaForm$NamedFunction` 适配)。
fn adapt_lambda_args(
    vm: &mut VmThread,
    args: Vec<Arg>,
    impl_params: &[FieldType],
) -> Result<Vec<Arg>, VmError> {
    args.into_iter()
        .enumerate()
        .map(|(i, arg)| match (arg, impl_params.get(i)) {
            (Arg::Reference(r), Some(pt)) if native::primitive_wrapper(pt).is_some() => {
                let slot = native::unbox_arg(vm, r, pt)?;
                slot_to_arg(slot)
            }
            (other, _) => Ok(other),
        })
        .collect()
}

/// G.4.1 lambda 适配器:按 SAM 返回类型对 impl 返回装箱/丢弃。impl 返原语、SAM 返引用 → 分配包装
/// 实例置 `value`(int→Integer 等);impl 返值、SAM 返 void → 丢弃(方法引用到 void SAM,如
/// `List::add`(boolean)作 `BiConsumer.accept` void);类型已匹配(引用→引用 / 原语→原语 /
/// void→void)→ 直传。对应 LambdaForm 的 box / void 节点。
fn adapt_lambda_return(
    vm: &mut VmThread,
    raw: Value,
    impl_ret: &ReturnDescriptor,
    sam_ret: &ReturnDescriptor,
) -> Result<Value, VmError> {
    match (impl_ret, sam_ret) {
        (
            ReturnDescriptor::FieldType(prim),
            ReturnDescriptor::FieldType(FieldType::Class(_) | FieldType::Array(_)),
        ) if native::primitive_wrapper(prim).is_some() => {
            let wrapper = native::primitive_wrapper(prim).expect("原语已判");
            let r = native::alloc_wrapper(vm, wrapper, value_to_slot(raw))?;
            Ok(Value::Reference(r))
        }
        // impl 返值(原语/引用)、SAM 返 void → 丢弃返回(方法引用到 void SAM,如 List::add)。
        (ReturnDescriptor::FieldType(_), ReturnDescriptor::Void) => Ok(Value::Void),
        _ => Ok(raw),
    }
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
    vm: &mut VmThread,
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
    vm: &mut VmThread,
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
fn stringify_arg(vm: &VmThread, arg: &Arg, ft: &FieldType, out: &mut String) {
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
    vm: &mut VmThread,
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
    vm: &mut VmThread,
    methodref_index: u16,
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokestatic 需要类注册表"))?;
    let (class_name, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    // 首次静态调用 → 触发声明类初始化(<clinit> 先行)。
    clinit::ensure_class_initialized(vm, &class_name)?;
    // invokestatic 方法解析(JVMS §5.4.3.4):引用类可能仅继承该静态法(编译器生成码常见),
    // 须沿超类链 → 超接口定位**声明类**;未命中回退到引用类自身(find_method 报"未找到")。
    let owner = find_static_method_owner(&registry, &class_name, &method_name, &desc)
        .unwrap_or_else(|| class_name.clone());
    let target_lc = registry
        .get(&owner)
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
        &target_lc,
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
    vm: &mut VmThread,
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
    let (target_lc, target_method_idx) = if method_name == "<init>" {
        match registry.get(&class_name) {
            None => {
                // 未加载类(根类 java/lang/Object 等):仅放行 <init>()V 空构造器。
                if matches!(md.return_type, ReturnDescriptor::Void) {
                    return Ok(InvokeFlow::Fallthrough);
                }
                return Err(VmError::BadConstant("invokespecial 目标类未加载"));
            }
            Some(lc) => {
                let idx = find_method_index(&lc.cf, &method_name, &desc)?;
                (lc, idx)
            }
        }
    } else {
        match registry.find_exact_method(&class_name, &method_name, &desc) {
            Some((lc, idx)) if lc.cf.methods[idx].access_flags.is_private() => (lc, idx),
            _ => registry
                .find_virtual_method(&class_name, &method_name, &desc)
                .ok_or(VmError::BadConstant("invokespecial 未找到目标方法"))?,
        }
    };
    let target_method = &target_lc.cf.methods[target_method_idx];
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
        &target_lc,
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
    vm: &mut VmThread,
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

    // 接收者取 owned(clone):其后 alloc/dispatch 需 &mut vm,持 heap guard 会 E0502(B.2.3b)。
    let recv = vm.heap().get(objref).cloned();
    let runtime_class = match recv {
        // 数组 receiver:Object 继承法(clone/getClass/hashCode/equals)短路;解锁 LF 准备等
        // 数组上的 Object 法分派。其余数组法顺延。
        Some(Oop::Array(a)) => {
            let argv: Vec<Value> = args.into_iter().map(Value::from).collect();
            let result = dispatch_array_object_method(vm, objref, &a, &method_name, &argv);
            return finish_invoke(interp, frame, vm, caller_pc, result, md.return_type);
        }
        // Lambda 闭包 receiver:捕获 ++ SAM 实参交给实现方法(lambda$<caller>$0)静态执行。
        Some(Oop::Lambda(lambda)) => {
            return dispatch_lambda(interp, frame, vm, caller_pc, lambda, args, md.return_type);
        }
        Some(Oop::Instance(i)) => i.class_name().to_string(),
        None => return Err(VmError::BadConstant("invokevirtual 引用悬空")),
    };

    // MethodHandle 签名多态短路(B.5.2):receiver 为字段 DirectMethodHandle → 直读 member 做
    // getfield/putfield/getstatic/putstatic。非字段 DMH / 非 invoke 族 → None,走下方正常虚分派。
    if let Some(v) = try_method_handle_invoke_hook(vm, &method_name, &runtime_class, objref, &args)? {
        return finish_invoke(interp, frame, vm, caller_pc, Ok(v), md.return_type);
    }

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokevirtual 需要类注册表"))?;
    // 类链先行,落空走接口 default(Java 8+ 类类型调用 default 亦走此路);
    // 命中抽象方法 → AbstractMethodError。
    let (target_lc, target_method_idx) = match registry
        .resolve_dispatch(&runtime_class, &method_name, &desc)
    {
        Some(x) => x,
        None => return Err(throw_exception(vm, "java/lang/AbstractMethodError")),
    };
    let target_method = &target_lc.cf.methods[target_method_idx];
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
        &target_lc,
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
    vm: &mut VmThread,
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

    // 接收者取 owned(clone):其后 alloc/dispatch 需 &mut vm,持 heap guard 会 E0502(B.2.3b)。
    let recv = vm.heap().get(objref).cloned();
    let runtime_class = match recv {
        // 数组 receiver:Object 继承法短路(同 invoke_virtual)。
        Some(Oop::Array(a)) => {
            let argv: Vec<Value> = args.into_iter().map(Value::from).collect();
            let result = dispatch_array_object_method(vm, objref, &a, &method_name, &argv);
            return finish_invoke(interp, frame, vm, caller_pc, result, md.return_type);
        }
        // Lambda 闭包 receiver:捕获 ++ SAM 实参交给实现方法静态执行。
        Some(Oop::Lambda(lambda)) => {
            return dispatch_lambda(interp, frame, vm, caller_pc, lambda, args, md.return_type);
        }
        Some(Oop::Instance(i)) => i.class_name().to_string(),
        None => return Err(VmError::BadConstant("invokeinterface 引用悬空")),
    };

    // MethodHandle 签名多态短路(B.5.2):同 invoke_virtual;DMH 经接口类型调用亦走此路。
    if let Some(v) = try_method_handle_invoke_hook(vm, &method_name, &runtime_class, objref, &args)? {
        return finish_invoke(interp, frame, vm, caller_pc, Ok(v), md.return_type);
    }

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokeinterface 需要类注册表"))?;
    // 类链先行,落空走接口 default;命中抽象方法 → AbstractMethodError。
    let (target_lc, target_method_idx) = match registry
        .resolve_dispatch(&runtime_class, &method_name, &desc)
    {
        Some(x) => x,
        None => return Err(throw_exception(vm, "java/lang/AbstractMethodError")),
    };
    let target_method = &target_lc.cf.methods[target_method_idx];
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
        &target_lc,
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
        let mut vm = crate::runtime::VmThread::default();
        let r = super::run_with_depth(&mut vm, |vm| {
            let d1 = vm.thread.frame_depth;
            let inner = super::run_with_depth(vm, |vm| Ok(vm.thread.frame_depth));
            assert_eq!(d1, 1);
            assert_eq!(inner.unwrap(), 2);
            Ok(())
        });
        assert!(r.is_ok());
        assert_eq!(vm.thread.frame_depth, 0);
    }

    #[test]
    fn run_with_depth_overflow_throws_stackoverflow_error() {
        // limit=2:外层→depth1,中层→depth2,内层 depth>=limit → 抛 StackOverflowError;
        // 异常路径仍对称归零。
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::VmThread::new(reg).with_stack_limit(2);
        let r = super::run_with_depth(&mut vm, |vm| {
            super::run_with_depth(vm, |vm| super::run_with_depth(vm, |_| Ok(())))
        });
        let super::VmError::ThrownException(exc) = r.unwrap_err() else {
            panic!("应抛 StackOverflowError(ThrownException)");
        };
        let heap = vm.heap();
        let Some(crate::oops::Oop::Instance(i)) = heap.get(exc) else {
            panic!("StackOverflowError 应为由引导桩分配的实例");
        };
        assert_eq!(i.class_name(), "java/lang/StackOverflowError");
        assert_eq!(vm.thread.frame_depth, 0);
    }
}
