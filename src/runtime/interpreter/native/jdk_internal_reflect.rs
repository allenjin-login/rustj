//! `jdk/internal/reflect/Reflection` 的 native 桥。
//!
//! 语义移植自 `src/java.base/share/classes/jdk/internal/reflect/Reflection.java` +
//! HotSpot `prims/jvm.cpp` 的 `JVM_GetCallerClass`。
//!
//! **`Reflection.getCallerClass()Ljava/lang/Class;`**(Reflection.java:73,native):
//! `@CallerSensitive` 基础设施——返回"调用 getCallerClass 的那个方法"的**调用者**的 Class。
//! 典型用法:被 `@CallerSensitive` 标注的方法 M 在体内调 `getCallerClass()` 取"谁调了 M"。
//! 真实第一缺口:`ClassLoader.registerAsParallelCapable()`(ClassLoader.java:1596)调它取
//! 调用者 Class 以登记为并行可加载 → 解锁 `SecureClassLoader.<clinit>` →
//! `ClassLoaders.<clinit>`(构造内置三大 loader)→ `ClassLoader.getSystemClassLoader()`。
//!
//! **栈帧语义**:`native::invoke` 已为本 native 推入自身帧(栈顶)。自顶向下:
//! 1. `Reflection.getCallerClass`(native 自身帧)—— 跳过;
//! 2. 调用 getCallerClass 的方法(`@CallerSensitive` 方法 M,如 `registerAsParallelCapable`)—— 跳过;
//! 3. **M 的调用者**——返回其 Class 镜像(`frame_class_at(2)`)。
//!
//! 由 [`super::dispatch`] 按声明类路由至此(`jdk/internal/reflect/Reflection`)。

use crate::oops::Oop;
use crate::runtime::{Frame, Interpreter, LocalVars, Reference, Slot, Value, VmThread, VmError};

use super::super::throw_exception;

/// `jdk/internal/reflect/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // Reflection.getCallerClass()Ljava/lang/Class; —— Reflection.java:73 native,
        // @CallerSensitive 基础设施。`native::invoke` 已推自身帧(栈顶);自顶第 2 层 =
        // "调用 getCallerClass 的方法"的调用者 → intern 其 Class 镜像。栈深不足 → null
        //(真实 HotSpot 抛 InternalError;rustj 取 null 最小安全语义,调用方据语境处理)。
        ("jdk/internal/reflect/Reflection", "getCallerClass", "()Ljava/lang/Class;") => {
            // 拥有 caller 名(frame_class_at 借 &vm;intern_class_mirror 需 &mut vm)。
            match vm.frame_class_at(2).map(|s| s.to_string()) {
                Some(caller) => Ok(Value::Reference(vm.intern_class_mirror(&caller))),
                None => Ok(Value::Reference(Reference::null())),
            }
        }
        // Reflection.getClassAccessFlags(Ljava/lang/Class;)I —— jmod(jdk-25.0.2)javap 确认
        // 为 `public static native`(jdk-master 源码已改字节码委派 Class.getClassFileAccessFlags,
        // 版本错位,以本机 jmod 实测为准)。返回 Class 的 class-file access flags 低 13 位。
        ("jdk/internal/reflect/Reflection", "getClassAccessFlags", "(Ljava/lang/Class;)I") => {
            Ok(Value::Int(get_class_access_flags(vm, args)?))
        }
        // DirectMethodHandleAccessor$NativeAccessor.invoke0(Method, Object, Object[])Object ——
        // = HotSpot `JVM_InvokeMethod`(prims/jvm.cpp:3282)→ `Reflection::invoke_method` →
        // `Reflection::invoke`(runtime/reflection.cpp)。Layer 4.15b 反射调用主交付物。绕过
        // MethodHandle 直接调用墙:NativeAccessor 路径(`useNativeAccessor` 在 rustj 不跑 initPhase3
        // 时恒 true)。语义:读 Method.clazz/slot/modifiers → cf.methods[slot] 解析目标 → 实例虚分派 /
        // 静态直调 → interpret_with → 返回装箱(void→null);目标异常包 InvocationTargetException。
        (
            "jdk/internal/reflect/DirectMethodHandleAccessor$NativeAccessor",
            "invoke0",
            "(Ljava/lang/reflect/Method;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;",
        ) => invoke_method_native(vm, args),
        // DirectConstructorHandleAccessor$NativeAccessor.newInstance0(Constructor, Object[])Object ——
        // = HotSpot `JVM_NewInstanceFromConstructor`(jvm.cpp:3306)→ `Reflection::invoke_constructor`。
        // 语义:读 Constructor.clazz/slot → new_instance 分配裸实例(不跑 <init>)→ cf.methods[slot]
        // 须为 <init> → locals[0]=新实例, [1..]=拆箱实参 → interpret_with 跑 <init> → 返新实例;
        // 目标异常包 InvocationTargetException。抽象类 → InstantiationException。
        (
            "jdk/internal/reflect/DirectConstructorHandleAccessor$NativeAccessor",
            "newInstance0",
            "(Ljava/lang/reflect/Constructor;[Ljava/lang/Object;)Ljava/lang/Object;",
        ) => new_instance_native(vm, args),
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

/// `Reflection.getClassAccessFlags(Class)I` native 语义移植(对应 `Class.getClassFileAccessFlags`,
/// `Class.java:4141` javadoc;`Reflection.java:78-82` 注释保证仅低 13 位 `0x1FFF` 有效):
/// - **普通类** → `cf.access_flags.bits() & 0x1FFF`(class 文件 access_flags 低 13 位);
/// - **数组**(`[...`)→ 0(javadoc:数组 → 0;`VerifyAccess.getClassModifiers` 对数组走
///   `c.getModifiers()` 不调本 native,此分支防御性);
/// - **原语**(`int`/`long`/…)→ `ACC_PUBLIC|ACC_ABSTRACT|ACC_FINAL` = 0x0411(javadoc:原语 → 此组合)。
///
/// null 参 / 非 Class 镜像 → `NullPointerException`(`JVM_GetClassAccessFlags` 对 null Class 的处置)。
fn get_class_access_flags(vm: &mut VmThread, args: &[Value]) -> Result<i32, VmError> {
    // class_arg_name 借 &vm 返 owned String,出 match 即释放 → 后续 throw_exception(&mut vm)/
    // registry() 无借用冲突。
    let internal = match super::class_arg_name(vm, args) {
        Some(n) => n,
        None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    if internal.starts_with('[') {
        return Ok(0);
    }
    if super::is_primitive_name(&internal) {
        // ACC_PUBLIC(0x0001)|ACC_FINAL(0x0010)|ACC_ABSTRACT(0x0400) = 0x0411。
        return Ok(0x0411);
    }
    // 普通类:读 class-file access_flags 低 13 位。类未加载(异常态)→ 0 兜底。
    // `.map` 须嵌在 `and_then(|r| …)` 内:`r`(owned Arc)仅闭包内活,`&LoadedClass` 借之;
    // 嵌套则 `.map` 在 `r` 存活时产 owned i32,避免返引用悬垂(B.3.0 Arc 局部寿命)。
    let bits = vm
        .registry()
        .and_then(|r| {
            r.get(&internal)
                .map(|lc| lc.cf.access_flags.bits() as i32 & 0x1FFF)
        })
        .unwrap_or(0);
    Ok(bits)
}

// ============================================================================
// Layer 4.15b — 反射调用(invoke0 / newInstance0)。移植自 HotSpot
// `JVM_InvokeMethod` / `JVM_NewInstanceFromConstructor`(prims/jvm.cpp:3282/3306)
// → `Reflection::invoke_method` / `invoke_constructor` → `Reflection::invoke`
// (runtime/reflection.cpp)。NativeAccessor 路径(rustj 不跑 initPhase3 →
// `useNativeAccessor` 恒 true)→ 绕过 MethodHandle 直接调用墙。
// ============================================================================

/// 读 `Method`/`Constructor` 镜像的 `clazz`(Class 镜像)/`slot`(i32)/`modifiers`(i32)。
/// 单次 heap 锁取 owned(meta 出块即释,后续可 `&mut vm`)。两类(Executable 子类)字段同名。
fn read_executable_meta(
    vm: &VmThread,
    exec_ref: Reference,
    class_name: &str,
) -> Result<(Reference, i32, i32), VmError> {
    let (clazz_ord, slot_ord, mod_ord) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("反射调用需类注册表"))?;
        let lc = reg
            .get(class_name)
            .ok_or(VmError::BadConstant("Executable 类未加载"))?;
        let flat = reg.flattened_instance_fields(&lc);
        let find = |n: &str| {
            flat.iter().position(|f| f.name == n)
                .ok_or(VmError::BadConstant("Executable 缺 clazz/slot/modifiers 字段"))
        };
        (find("clazz")?, find("slot")?, find("modifiers")?)
    };
    let heap = vm.heap();
    let inst = match heap.get(exec_ref) {
        Some(Oop::Instance(i)) => i,
        _ => return Err(VmError::BadConstant("Executable 引用非 Instance")),
    };
    let clazz = match inst.field(clazz_ord) {
        Slot::Reference(r) => r,
        _ => return Err(VmError::BadConstant("clazz 字段非引用")),
    };
    let slot = match inst.field(slot_ord) {
        Slot::Int(v) => v,
        _ => return Err(VmError::BadConstant("slot 字段非 int")),
    };
    let modifiers = match inst.field(mod_ord) {
        Slot::Int(v) => v,
        _ => return Err(VmError::BadConstant("modifiers 字段非 int")),
    };
    Ok((clazz, slot, modifiers))
}

/// 读 Object[] 元素为 owned `Vec<Reference>`(null 数组 → 空)。持 heap 锁仅块内。
fn read_object_array(vm: &VmThread, arr_ref: Reference) -> Result<Vec<Reference>, VmError> {
    if arr_ref.is_null() {
        return Ok(Vec::new());
    }
    let heap = vm.heap();
    match heap.get(arr_ref) {
        Some(Oop::Array(a)) => (0..a.length())
            .map(|i| match a.element(i) {
                Slot::Reference(r) => Ok(r),
                _ => Err(VmError::BadConstant("Object[] 元素须 Reference")),
            })
            .collect(),
        _ => Err(VmError::BadConstant("实参数组非 Array")),
    }
}

/// 原语 `FieldType` → 其包装类内部名(供拆箱/装箱);非原语 → `None`。
/// `pub(crate)`:G.4.1 lambda 适配器复用。
pub(crate) fn primitive_wrapper(ft: &crate::metadata::descriptor::FieldType) -> Option<&'static str> {
    use crate::metadata::descriptor::FieldType;
    match ft {
        FieldType::Boolean => Some("java/lang/Boolean"),
        FieldType::Byte => Some("java/lang/Byte"),
        FieldType::Char => Some("java/lang/Character"),
        FieldType::Short => Some("java/lang/Short"),
        FieldType::Int => Some("java/lang/Integer"),
        FieldType::Long => Some("java/lang/Long"),
        FieldType::Float => Some("java/lang/Float"),
        FieldType::Double => Some("java/lang/Double"),
        _ => None,
    }
}

/// 按 `param_type` 拆箱一个实参:引用/数组类型 → 原引用(null 保留);原语类型 → 读包装实例 `value`
/// 字段(I/Z/B/C/S→Int、J→Long、F→Float、D→Double)。null 拆箱原语 → NPE(JLS 拆箱语义)。
/// `pub(crate)`:G.4.1 lambda 适配器(`dispatch_lambda`)对 SAM 装箱实参拆箱复用。
pub(crate) fn unbox_arg(vm: &mut VmThread, arg: Reference, param_type: &crate::metadata::descriptor::FieldType) -> Result<Slot, VmError> {
    use crate::metadata::descriptor::FieldType;
    match param_type {
        FieldType::Class(_) | FieldType::Array(_) => Ok(Slot::Reference(arg)),
        prim => {
            if arg.is_null() {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            }
            let wrapper = primitive_wrapper(prim).expect("非原语已分流");
            let ord = {
                let reg = vm
                    .registry()
                    .ok_or(VmError::BadConstant("拆箱需类注册表"))?;
                let lc = reg
                    .get(wrapper)
                    .ok_or(VmError::BadConstant("包装类未加载"))?;
                reg.flattened_instance_fields(&lc)
                    .iter()
                    .position(|f| f.name == "value")
                    .ok_or(VmError::BadConstant("包装类无 value 字段"))?
            };
            let heap = vm.heap();
            match heap.get(arg) {
                Some(Oop::Instance(i)) => Ok(i.field(ord)),
                _ => Err(VmError::BadConstant("拆箱目标非 Instance")),
            }
        }
    }
}

/// 装箱反射返回值(void → null;引用 → 原 Reference;原语 → 分配包装实例置 `value`)。
fn box_return(
    vm: &mut VmThread,
    ret: &crate::metadata::descriptor::ReturnDescriptor,
    value: Value,
) -> Result<Reference, VmError> {
    use crate::metadata::descriptor::{FieldType, ReturnDescriptor};
    match ret {
        ReturnDescriptor::Void => Ok(Reference::null()),
        ReturnDescriptor::FieldType(ft) => match ft {
            FieldType::Class(_) | FieldType::Array(_) => match value {
                Value::Reference(r) => Ok(r),
                Value::Void => Ok(Reference::null()),
                _ => Err(VmError::BadConstant("引用返回值类型不符")),
            },
            prim => {
                let wrapper = primitive_wrapper(prim).expect("非原语已分流");
                let slot = match prim {
                    FieldType::Boolean
                    | FieldType::Byte
                    | FieldType::Char
                    | FieldType::Short
                    | FieldType::Int => match value {
                        Value::Int(v) => Slot::Int(v),
                        _ => return Err(VmError::BadConstant("原语返回值类型不符")),
                    },
                    FieldType::Long => match value {
                        Value::Long(v) => Slot::Long(v),
                        _ => return Err(VmError::BadConstant("原语返回值类型不符")),
                    },
                    FieldType::Float => match value {
                        Value::Float(v) => Slot::Float(v),
                        _ => return Err(VmError::BadConstant("原语返回值类型不符")),
                    },
                    FieldType::Double => match value {
                        Value::Double(v) => Slot::Double(v),
                        _ => return Err(VmError::BadConstant("原语返回值类型不符")),
                    },
                    // 数组/Class 已在上一臂分流;此处不可达。
                    _ => return Err(VmError::BadConstant("原语返回值类型不符")),
                };
                alloc_wrapper(vm, wrapper, slot)
            }
        },
    }
}

/// 分配包装类实例并置 `value` 字段(供 `box_return`)。`new_instance` 不跑 `<init>`,
/// 直接写字段(对应 HotSpot `box()` 经 `ReflectionFactory` 的反射装箱;Integer 等已 <clinit>)。
/// `pub(crate)`:G.4.1 lambda 适配器对 impl 原语返回装箱复用。
pub(crate) fn alloc_wrapper(vm: &mut VmThread, wrapper: &str, value: Slot) -> Result<Reference, VmError> {
    let (inst_ref, ord) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("装箱需类注册表"))?;
        let lc = reg
            .get(wrapper)
            .ok_or(VmError::BadConstant("包装类未加载"))?;
        let ord = reg
            .flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == "value")
            .ok_or(VmError::BadConstant("包装类无 value 字段"))?;
        let inst_ref = vm.heap_mut().alloc(Oop::Instance(reg.new_instance(&lc)));
        (inst_ref, ord)
    };
    if let Some(Oop::Instance(i)) = vm.heap_mut().get_mut(inst_ref) {
        i.set_field(ord, value);
    }
    Ok(inst_ref)
}

/// 写一个 `Slot` 到 locals[idx],返回下一可用索引(long/double 占双槽,其余单槽)。
fn set_local_slot(locals: &mut LocalVars, index: u16, slot: Slot) -> Result<u16, VmError> {
    Ok(match slot {
        Slot::Long(v) => {
            locals
                .set_long(index, v)
                .map_err(|_| VmError::BadConstant("locals 写 Long 失败"))?;
            index + 2
        }
        Slot::Double(v) => {
            locals
                .set_double(index, v)
                .map_err(|_| VmError::BadConstant("locals 写 Double 失败"))?;
            index + 2
        }
        Slot::Int(v) => {
            locals
                .set_int(index, v)
                .map_err(|_| VmError::BadConstant("locals 写 Int 失败"))?;
            index + 1
        }
        Slot::Float(v) => {
            locals
                .set_float(index, v)
                .map_err(|_| VmError::BadConstant("locals 写 Float 失败"))?;
            index + 1
        }
        Slot::Reference(v) => {
            locals
                .set_reference(index, v)
                .map_err(|_| VmError::BadConstant("locals 写 Reference 失败"))?;
            index + 1
        }
        Slot::ReturnAddress(_) | Slot::Top => index + 1,
    })
}

/// 把目标异常 `cause` 包进 `InvocationTargetException`(设其 `target` 字段 = cause;
/// `ITE.getCause()` 返 `target`,Throwable.java:557 / ITE.java)。对应 `Reflection::invoke`
/// 的 `THROW_ARG(InvocationTargetException, &target_exception)`(reflection.cpp:1110)。
fn wrap_in_invocation_target_exception(vm: &mut VmThread, cause: Reference) -> VmError {
    let err = throw_exception(vm, "java/lang/reflect/InvocationTargetException");
    let VmError::ThrownException(ite) = err else {
        return err;
    };
    // target 字段在 ITE 自身(非 Throwable 基类)→ 经 ITE 扁平字段查序号写。
    let ord = {
        let Some(reg) = vm.registry() else {
            return VmError::ThrownException(ite);
        };
        let Some(lc) = reg.get("java/lang/reflect/InvocationTargetException") else {
            return VmError::ThrownException(ite);
        };
        let Some(ord) = reg
            .flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == "target")
        else {
            return VmError::ThrownException(ite);
        };
        ord
    };
    if let Some(Oop::Instance(i)) = vm.heap_mut().get_mut(ite) {
        i.set_field(ord, Slot::Reference(cause));
    }
    VmError::ThrownException(ite)
}

/// `DirectMethodHandleAccessor$NativeAccessor.invoke0`(= `JVM_InvokeMethod`)实现。
fn invoke_method_native(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    use crate::constant_pool::ConstantPoolEntry;
    use crate::metadata::access_flags::ACC_STATIC;
    use crate::metadata::descriptor::parse_method_descriptor;

    const METHOD: &str = "java/lang/reflect/Method";
    // 三参(Method, Object receiver, Object[] args)。
    let method_ref = match args.first().copied().unwrap_or(Value::Void) {
        Value::Reference(r) if !r.is_null() => r,
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let receiver = match args.get(1).copied().unwrap_or(Value::Void) {
        Value::Reference(r) => r,
        _ => Reference::null(),
    };
    let args_ref = match args.get(2).copied().unwrap_or(Value::Void) {
        Value::Reference(r) if !r.is_null() => r,
        _ => Reference::null(),
    };

    let (clazz_mirror, slot, modifiers) = read_executable_meta(vm, method_ref, METHOD)?;
    let slot = slot as usize;
    let internal = vm
        .mirror_internal_name(clazz_mirror)
        .ok_or(VmError::BadConstant("invoke0:Method.clazz 非 Class 镜像"))?;

    // 目标方法 name+desc(cf.methods[slot];CP 取 Utf8)。
    let (name, desc) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("invoke0 需类注册表"))?;
        let lc = reg
            .get(&internal)
            .ok_or(VmError::BadConstant("invoke0:声明类未加载"))?;
        let m = lc
            .cf
            .methods
            .get(slot)
            .ok_or(VmError::BadConstant("invoke0:slot 越界"))?;
        let name = match lc.cf.constant_pool.get(m.name_index) {
            Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
            _ => return Err(VmError::BadConstant("invoke0:方法名解析失败")),
        };
        let desc = match lc.cf.constant_pool.get(m.descriptor_index) {
            Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
            _ => return Err(VmError::BadConstant("invoke0:描述符解析失败")),
        };
        (name, desc)
    };
    let method_desc = parse_method_descriptor(&desc)
        .map_err(|_| VmError::BadConstant("invoke0:方法描述符非法"))?;
    let is_static = (modifiers as u16 & ACC_STATIC) != 0;

    let arg_refs = read_object_array(vm, args_ref)?;
    // 实参计数检查(Reflection::invoke:"wrong number of arguments")。
    if method_desc.parameters.len() != arg_refs.len() {
        return Err(throw_exception(vm, "java/lang/IllegalArgumentException"));
    }
    let mut arg_slots: Vec<Slot> = Vec::with_capacity(arg_refs.len());
    for (a, t) in arg_refs.iter().zip(&method_desc.parameters) {
        arg_slots.push(unbox_arg(vm, *a, t)?);
    }

    // 分派目标:静态 = 声明类 cf.methods[slot];实例 = 虚分派 receiver 运行时类。
    // `reg`(owned Arc)独立于 &mut vm → interpret_with(&mut vm) 可与 &LoadedClass 共存(§6 NLL)。
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("invoke0 需类注册表"))?;
    let (target_lc, target_method_idx) = if is_static {
        let lc = reg
            .get(&internal)
            .ok_or(VmError::BadConstant("invoke0:声明类未加载"))?;
        lc.cf
            .methods
            .get(slot)
            .ok_or(VmError::BadConstant("invoke0:slot 越界"))?;
        (lc, slot)
    } else {
        if receiver.is_null() {
            return Err(throw_exception(vm, "java/lang/NullPointerException"));
        }
        // 先在块内持 heap 取 owned 类名(出块释 guard),再于外层 match 调 throw_exception(&mut vm)。
        let runtime_class_opt: Option<String> = {
            let heap = vm.heap();
            match heap.get(receiver) {
                Some(Oop::Instance(i)) => Some(i.class_name().to_string()),
                Some(Oop::Array(a)) => Some(a.class_name().to_string()),
                _ => None,
            }
        };
        let runtime_class = match runtime_class_opt {
            Some(c) => c,
            None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
        };
        match reg.resolve_dispatch(&runtime_class, &name, &desc) {
            Some(x) => x,
            None => return Err(throw_exception(vm, "java/lang/AbstractMethodError")),
        }
    };
    let target_method = &target_lc.cf.methods[target_method_idx];
    let Some(code) = target_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/AbstractMethodError"));
    };

    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let mut idx = 0u16;
    if !is_static {
        idx = set_local_slot(&mut frame.locals, idx, Slot::Reference(receiver))?;
    }
    for s in arg_slots {
        idx = set_local_slot(&mut frame.locals, idx, s)?;
    }

    let interp = Interpreter::new(&code.code, &target_lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    let result = match interp.interpret_with(&mut frame, vm) {
        Ok(v) => v,
        Err(VmError::ThrownException(t)) => {
            return Err(wrap_in_invocation_target_exception(vm, t));
        }
        Err(e) => return Err(e),
    };
    let ret_ref = box_return(vm, &method_desc.return_type, result)?;
    Ok(Value::Reference(ret_ref))
}

/// `DirectConstructorHandleAccessor$NativeAccessor.newInstance0`(= `JVM_NewInstanceFromConstructor`)
/// 实现:分配裸实例 + 跑 `<init>`。
fn new_instance_native(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    use crate::constant_pool::ConstantPoolEntry;
    use crate::metadata::access_flags::ACC_ABSTRACT;
    use crate::metadata::descriptor::parse_method_descriptor;

    const CTOR: &str = "java/lang/reflect/Constructor";
    let ctor_ref = match args.first().copied().unwrap_or(Value::Void) {
        Value::Reference(r) if !r.is_null() => r,
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let args_ref = match args.get(1).copied().unwrap_or(Value::Void) {
        Value::Reference(r) if !r.is_null() => r,
        _ => Reference::null(),
    };

    let (clazz_mirror, slot, _modifiers) = read_executable_meta(vm, ctor_ref, CTOR)?;
    let slot = slot as usize;
    let internal = vm
        .mirror_internal_name(clazz_mirror)
        .ok_or(VmError::BadConstant("newInstance0:Constructor.clazz 非 Class 镜像"))?;

    // 抽象类 → InstantiationException(JVM_NewInstanceFromConstructor 前置检)。
    let is_abstract = vm
        .registry()
        .and_then(|r| {
            r.get(&internal)
                .map(|lc| lc.cf.access_flags.bits() & ACC_ABSTRACT != 0)
        })
        .unwrap_or(false);
    if is_abstract {
        return Err(throw_exception(vm, "java/lang/InstantiationException"));
    }

    // <init> 描述符(cf.methods[slot])。
    let desc = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("newInstance0 需类注册表"))?;
        let lc = reg
            .get(&internal)
            .ok_or(VmError::BadConstant("newInstance0:声明类未加载"))?;
        let m = lc
            .cf
            .methods
            .get(slot)
            .ok_or(VmError::BadConstant("newInstance0:slot 越界"))?;
        match lc.cf.constant_pool.get(m.descriptor_index) {
            Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
            _ => return Err(VmError::BadConstant("newInstance0:描述符解析失败")),
        }
    };
    let method_desc = parse_method_descriptor(&desc)
        .map_err(|_| VmError::BadConstant("newInstance0:方法描述符非法"))?;

    let arg_refs = read_object_array(vm, args_ref)?;
    if method_desc.parameters.len() != arg_refs.len() {
        return Err(throw_exception(vm, "java/lang/IllegalArgumentException"));
    }
    let mut arg_slots: Vec<Slot> = Vec::with_capacity(arg_refs.len());
    for (a, t) in arg_refs.iter().zip(&method_desc.parameters) {
        arg_slots.push(unbox_arg(vm, *a, t)?);
    }

    // 分配裸实例(不跑 <init>)+ 解析 <init> 目标。reg owned Arc → 与 &mut vm 共存。
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("newInstance0 需类注册表"))?;
    let lc = reg
        .get(&internal)
        .ok_or(VmError::BadConstant("newInstance0:声明类未加载"))?;
    let new_inst = vm
        .heap_mut()
        .alloc(Oop::Instance(reg.new_instance(&lc)));
    let init_method = lc
        .cf
        .methods
        .get(slot)
        .ok_or(VmError::BadConstant("newInstance0:slot 越界"))?;
    let Some(code) = init_method.code.as_ref() else {
        return Err(throw_exception(vm, "java/lang/InstantiationError"));
    };

    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let mut idx = 0u16;
    idx = set_local_slot(&mut frame.locals, idx, Slot::Reference(new_inst))?;
    for s in arg_slots {
        idx = set_local_slot(&mut frame.locals, idx, s)?;
    }

    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(_) => Ok(Value::Reference(new_inst)),
        Err(VmError::ThrownException(t)) => Err(wrap_in_invocation_target_exception(vm, t)),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use crate::constant_pool::ConstantPoolEntry;
    use crate::oops::{ArrayOop, ClassRegistry, Oop};
    use crate::runtime::class_loader::class_path::ClassPath;
    use crate::runtime::class_loader::loader::load_closure;
    use crate::runtime::{Reference, Slot, Value, VmThread, VmError};

    use std::path::{Path, PathBuf};

    fn find_javabase_jmod() -> Option<PathBuf> {
        for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
            let p = Path::new("C:/Program Files/Java")
                .join(ver)
                .join("jmods/java.base.jmod");
            if p.exists() {
                return Some(p);
            }
        }
        std::env::var("JAVA_HOME")
            .ok()
            .map(|jh| PathBuf::from(jh).join("jmods/java.base.jmod"))
            .filter(|p| p.exists())
    }

    /// **RED→GREEN**:Reflection.getCallerClass 返回"调用 getCallerClass 的方法"的**调用者** Class。
    ///
    /// 模拟真实链(如 `SecureClassLoader.<clinit>` → `ClassLoader.registerAsParallelCapable`
    /// → `Reflection.getCallerClass`):手推两帧——底帧 = 调用者(期望返回的 Class),
    /// 顶帧 = 调用 getCallerClass 的 @CallerSensitive 方法。`super::super::invoke` 再为
    /// getCallerClass 自身推一帧,故自顶第 2 层 = 底帧 = `java/lang/Object`。
    #[test]
    fn get_caller_class_returns_caller_of_caller() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        // 闭包预载 Object(传递性载 Class)→ intern_class_mirror 可分配真 Class Instance。
        load_closure(&mut registry, &cp, "java/lang/Object").unwrap();

        let mut vm = VmThread::new(registry);
        // 底帧:调用者(期望返回其 Class)。顶帧:调用 getCallerClass 的方法。
        vm.push_frame("java/lang/Object", "testCaller");
        vm.push_frame("java/lang/String", "run");

        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "getCallerClass",
            "()Ljava/lang/Class;",
            None,
            &[],
        )
        .expect("getCallerClass 应返回调用者的 Class 镜像,非抛异常");
        let Value::Reference(mirror) = r else {
            panic!("getCallerClass 须返 Class 镜像引用,得 {r:?}");
        };
        assert!(!mirror.is_null(), "getCallerClass 不得返 null(栈深足够)");
        assert_eq!(
            vm.mirror_internal_name(mirror).as_deref(),
            Some("java/lang/Object"),
            "getCallerClass 须返底帧(调用者的调用者)的 Class"
        );
    }

    /// 栈深不足(< 3:无调用者的调用者)→ 返 null(不抛 InternalError,最小安全语义)。
    #[test]
    fn get_caller_class_insufficient_depth_returns_null() {
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(find_javabase_jmod().expect("须有 jmod")).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Object").unwrap();

        let mut vm = VmThread::new(registry);
        // 仅一帧(无调用者的调用者)→ invoke 推 getCallerClass 后栈深 = 2 < 3 → null。
        vm.push_frame("java/lang/String", "run");
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "getCallerClass",
            "()Ljava/lang/Class;",
            None,
            &[],
        )
        .expect("栈深不足应返 null,非内部错误");
        match r {
            Value::Reference(r) => assert!(r.is_null(), "栈深不足须返 null Class"),
            other => panic!("须 null 引用,得 {other:?}"),
        }
    }

    /// **RED→GREEN**(Layer 4.23):`Reflection.getClassAccessFlags(Class)I` native 返回 Class 的
    /// class-file access flags(低 13 位 `0x1FFF` 有效,`Reflection.java:78-82` 注释)。jmod
    /// (jdk-25.0.2)javap 确认此法为 `public static native`(jdk-master 源码已改字节码委派
    /// `Class.getClassFileAccessFlags`——版本错位,以本机 jmod 实测为准)。`Integer` =
    /// `public final class` → ACC_PUBLIC|ACC_FINAL|ACC_SUPER;返回须 = `cf.access_flags.bits() & 0x1FFF`。
    #[test]
    fn get_class_access_flags_regular_class() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

        let mut vm = VmThread::new(registry);
        let mirror = vm.intern_class_mirror("java/lang/Integer");
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "getClassAccessFlags",
            "(Ljava/lang/Class;)I",
            None,
            &[Value::Reference(mirror)],
        )
        .expect("getClassAccessFlags 应返 int,非抛异常");
        let Value::Int(flags) = r else {
            panic!("getClassAccessFlags 须返 int,得 {r:?}");
        };
        // `.map` 嵌在 `and_then(|r| …)` 内:`r`(owned Arc)仅闭包内活,`&LoadedClass` 借之。
        let expected = vm
            .registry()
            .and_then(|r| {
                r.get("java/lang/Integer")
                    .map(|lc| lc.cf.access_flags.bits() as i32 & 0x1FFF)
            })
            .expect("Integer 须已加载");
        assert_eq!(
            flags, expected,
            "getClassAccessFlags 须 = cf.access_flags.bits() & 0x1FFF"
        );
        // 卫生:Integer 为 public → ACC_PUBLIC(0x0001)位须置(防实现偷返 0)。
        assert_eq!(flags & 0x0001, 1, "Integer 须 ACC_PUBLIC");
    }

    /// **RED→GREEN**(Layer 4.23):数组 Class → 0(`Class.getClassFileAccessFlags` javadoc:
    /// 数组 → 0;`VerifyAccess.getClassModifiers` 对数组走 `c.getModifiers()` 不调本 native,
    /// 此分支为防御性正确)。
    #[test]
    fn get_class_access_flags_array_returns_zero() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Object").unwrap();

        let mut vm = VmThread::new(registry);
        let mirror = vm.intern_class_mirror("[B");
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "getClassAccessFlags",
            "(Ljava/lang/Class;)I",
            None,
            &[Value::Reference(mirror)],
        )
        .expect("数组 Class 须返 0,非抛异常");
        let Value::Int(flags) = r else {
            panic!("getClassAccessFlags 须返 int,得 {r:?}");
        };
        assert_eq!(flags, 0, "数组 Class 的 access flags 须为 0");
    }

    /// **RED→GREEN**(Layer 4.23):原语 Class → PUBLIC|ABSTRACT|FINAL = 0x0411
    ///(`Class.getClassFileAccessFlags` javadoc:原语 → PUBLIC|ABSTRACT|FINAL;防御性)。
    #[test]
    fn get_class_access_flags_primitive_returns_modifiers() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Object").unwrap();

        let mut vm = VmThread::new(registry);
        let mirror = vm.intern_class_mirror("int");
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "getClassAccessFlags",
            "(Ljava/lang/Class;)I",
            None,
            &[Value::Reference(mirror)],
        )
        .expect("原语 Class 须返 0x0411,非抛异常");
        let Value::Int(flags) = r else {
            panic!("getClassAccessFlags 须返 int,得 {r:?}");
        };
        assert_eq!(flags, 0x0411, "原语 Class 须 PUBLIC|ABSTRACT|FINAL = 0x0411");
    }

    /// **RED→GREEN**(Layer 4.23):null 参 → NullPointerException(对应 HotSpot
    /// `JVM_GetClassAccessFlags` 对 null Class 的处置)。
    #[test]
    fn get_class_access_flags_null_arg_throws_npe() {
        let registry = ClassRegistry::new();
        let mut vm = VmThread::new(registry);
        let err = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "getClassAccessFlags",
            "(Ljava/lang/Class;)I",
            None,
            &[Value::Reference(Reference::null())],
        )
        .unwrap_err();
        match err {
            VmError::ThrownException(r) => match vm.heap().get(r) {
                Some(crate::oops::Oop::Instance(i)) => {
                    assert_eq!(i.class_name(), "java/lang/NullPointerException")
                }
                o => panic!("须 Instance,得 {o:?}"),
            },
            e => panic!("须 ThrownException,得 {e:?}"),
        }
    }

    /// 收尾:确使未登记路径仍抛 ULE(防 dispatch 误吞)。
    #[test]
    fn unbound_reflection_native_throws_ule() {
        let registry = ClassRegistry::new();
        let mut vm = VmThread::new(registry);
        let err = super::super::invoke(
            &mut vm,
            "jdk/internal/reflect/Reflection",
            "unknownNative",
            "()V",
            None,
            &[],
        )
        .unwrap_err();
        match err {
            VmError::ThrownException(r) => match vm.heap().get(r) {
                Some(crate::oops::Oop::Instance(i)) => {
                    assert_eq!(i.class_name(), "java/lang/UnsatisfiedLinkError")
                }
                o => panic!("须 Instance,得 {o:?}"),
            },
            e => panic!("须 ThrownException,得 {e:?}"),
        }
    }

    // ========================================================================
    // Layer 4.15b — invoke0 / newInstance0 lib 闸门(直接调 native,绕开
    // Method.invoke/setAccessible 字节码的 Module.descriptor 访问检查——该检查
    // 顺延至 Module 描述符填充子层)。证明 JVM_InvokeMethod/JVM_NewInstance 语义:
    // slot 解析、参数拆箱、静态/实例虚分派、返回装箱、InvocationTargetException 包装。
    // ========================================================================

    const NATIVE_ACCESSOR: &str =
        "jdk/internal/reflect/DirectMethodHandleAccessor$NativeAccessor";
    const CTOR_NATIVE_ACCESSOR: &str =
        "jdk/internal/reflect/DirectConstructorHandleAccessor$NativeAccessor";
    const INVOKE0_DESC: &str =
        "(Ljava/lang/reflect/Method;Ljava/lang/Object;[Ljava/lang/Object;)Ljava/lang/Object;";
    const NEWINSTANCE0_DESC: &str =
        "(Ljava/lang/reflect/Constructor;[Ljava/lang/Object;)Ljava/lang/Object;";

    /// 找 `class.name(desc)` 在 `cf.methods` 的(原始下标 slot, access_flags)。4.15a
    /// `getDeclaredMethods0(false)` 无过滤 → slot == 原始下标;故 invoke0/newInstance0
    /// 据 slot 直索引 `cf.methods[slot]`。
    fn find_method(vm: &VmThread, class: &str, name: &str, desc: &str) -> (i32, i32) {
        let reg = vm.registry().expect("注册表");
        let lc = reg.get(class).unwrap_or_else(|| panic!("{class} 须加载"));
        lc.cf
            .methods
            .iter()
            .enumerate()
            .find_map(|(i, m)| {
                let n = matches!(
                    lc.cf.constant_pool.get(m.name_index),
                    Ok(ConstantPoolEntry::Utf8(s)) if s == name
                );
                let d = matches!(
                    lc.cf.constant_pool.get(m.descriptor_index),
                    Ok(ConstantPoolEntry::Utf8(s)) if s == desc
                );
                (n && d).then(|| (i as i32, m.access_flags.bits() as i32))
            })
            .unwrap_or_else(|| panic!("{class}.{name}{desc} 须存在"))
    }

    /// 构造 Executable(Method/Constructor)镜像:分配裸实例 + 按**字段名**写
    /// clazz/slot/modifiers(invoke0/newInstance0 的 read_executable_meta 据此三名读取)。
    fn build_executable_mirror(
        vm: &mut VmThread,
        class_name: &str,
        clazz_mirror: Reference,
        slot: i32,
        modifiers: i32,
    ) -> Reference {
        let (inst, clazz_ord, slot_ord, mod_ord) = {
            let reg = vm.registry().expect("注册表");
            let lc = reg
                .get(class_name)
                .unwrap_or_else(|| panic!("{class_name} 须加载"));
            let flat = reg.flattened_instance_fields(&lc);
            let find = |n: &str| {
                flat.iter()
                    .position(|f| f.name == n)
                    .unwrap_or_else(|| panic!("{class_name} 缺 {n} 字段"))
            };
            (reg.new_instance(&lc), find("clazz"), find("slot"), find("modifiers"))
        };
        let inst_ref = vm.heap_mut().alloc(Oop::Instance(inst));
        if let Some(Oop::Instance(i)) = vm.heap_mut().get_mut(inst_ref) {
            i.set_field(clazz_ord, Slot::Reference(clazz_mirror));
            i.set_field(slot_ord, Slot::Int(slot));
            i.set_field(mod_ord, Slot::Int(modifiers));
        }
        inst_ref
    }

    /// 分配 `wrapper` 实例并置 `value`(供反射调用的原语实参装箱)。
    fn box_primitive(vm: &mut VmThread, wrapper: &str, value: Slot) -> Reference {
        let (inst, ord) = {
            let reg = vm.registry().expect("注册表");
            let lc = reg
                .get(wrapper)
                .unwrap_or_else(|| panic!("{wrapper} 须加载"));
            let ord = reg
                .flattened_instance_fields(&lc)
                .iter()
                .position(|f| f.name == "value")
                .unwrap_or_else(|| panic!("{wrapper} 缺 value"));
            (reg.new_instance(&lc), ord)
        };
        let inst_ref = vm.heap_mut().alloc(Oop::Instance(inst));
        if let Some(Oop::Instance(i)) = vm.heap_mut().get_mut(inst_ref) {
            i.set_field(ord, value);
        }
        inst_ref
    }

    /// 读包装实例的 `value` int 字段(供反射返回值断言)。
    fn read_int_value(vm: &VmThread, r: Reference, wrapper: &str) -> i32 {
        let ord = {
            let reg = vm.registry().expect("注册表");
            let lc = reg.get(wrapper).expect("包装类须加载");
            reg.flattened_instance_fields(&lc)
                .iter()
                .position(|f| f.name == "value")
                .expect("value 字段")
        };
        match vm.heap().get(r) {
            Some(Oop::Instance(i)) => match i.field(ord) {
                Slot::Int(v) => v,
                s => panic!("{wrapper}.value 须 int,得 {s:?}"),
            },
            o => panic!("须 Instance,得 {o:?}"),
        }
    }

    /// 构造 `Object[]`(元素引用;空 → 长 0 数组)。null 数组由 invoke0 自身归一。
    fn object_array(vm: &mut VmThread, elems: Vec<Reference>) -> Reference {
        let slots: Vec<Slot> = elems.into_iter().map(Slot::Reference).collect();
        vm.heap_mut()
            .alloc(Oop::Array(ArrayOop::new("[Ljava/lang/Object;".to_string(), slots)))
    }

    /// 预载反射调用所需的最小真 java.base 类簇(Integer/Method/Constructor/ITE +
    /// 传递性依赖)。lib 闸门直接调 native,绕开 Method.invoke 字节码,故无需 Phase1/2。
    fn reflection_vm() -> VmThread {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            // 返回空 Vm;调用方用 find_javabase_jmod 守卫提前 return。
            return VmThread::new(ClassRegistry::new());
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        for c in [
            "java/lang/Integer",
            "java/lang/String",
            "java/lang/reflect/Method",
            "java/lang/reflect/Constructor",
            "java/lang/reflect/InvocationTargetException",
        ] {
            load_closure(&mut registry, &cp, c).unwrap();
        }
        VmThread::new(registry)
    }

    fn has_jmod() -> bool {
        find_javabase_jmod().is_some()
    }

    /// **RED→GREEN**:invoke0 静态法 `Integer.parseInt("42")` → 装箱 Integer(42)。
    /// 钉:slot 解析 + 静态直调 + String 参(无拆箱)+ Int 返回装箱。
    #[test]
    fn invoke0_static_parseint_returns_42() {
        if !has_jmod() {
            return;
        }
        let mut vm = reflection_vm();
        let int_mirror = vm.intern_class_mirror("java/lang/Integer");
        let (slot, modifiers) =
            find_method(&vm, "java/lang/Integer", "parseInt", "(Ljava/lang/String;)I");
        let method = build_executable_mirror(&mut vm, "java/lang/reflect/Method", int_mirror, slot, modifiers);

        let s42 = crate::runtime::interpreter::string::intern(&mut vm, "42").unwrap();
        let args = object_array(&mut vm, vec![s42]);

        let r = super::super::invoke(
            &mut vm,
            NATIVE_ACCESSOR,
            "invoke0",
            INVOKE0_DESC,
            None,
            &[Value::Reference(method), Value::Reference(Reference::null()), Value::Reference(args)],
        )
        .expect("invoke0 parseInt(\"42\") 应成功");
        let Value::Reference(boxed) = r else {
            panic!("invoke0 须返装箱 Integer 引用,得 {r:?}");
        };
        assert_eq!(read_int_value(&vm, boxed, "java/lang/Integer"), 42);
    }

    /// **RED→GREEN**:invoke0 实例虚分派 `"hello".length()` → 装箱 Integer(5)。
    /// 钉:实例 receiver 运行时类虚分派(resolve_dispatch)+ receiver null 守卫 + 无参。
    #[test]
    fn invoke0_instance_string_length_returns_5() {
        if !has_jmod() {
            return;
        }
        let mut vm = reflection_vm();
        let str_mirror = vm.intern_class_mirror("java/lang/String");
        let (slot, modifiers) = find_method(&vm, "java/lang/String", "length", "()I");
        let method = build_executable_mirror(&mut vm, "java/lang/reflect/Method", str_mirror, slot, modifiers);

        let receiver = crate::runtime::interpreter::string::intern(&mut vm, "hello").unwrap();
        let args = object_array(&mut vm, vec![]);

        let r = super::super::invoke(
            &mut vm,
            NATIVE_ACCESSOR,
            "invoke0",
            INVOKE0_DESC,
            None,
            &[Value::Reference(method), Value::Reference(receiver), Value::Reference(args)],
        )
        .expect("invoke0 String.length 应成功");
        let Value::Reference(boxed) = r else {
            panic!("invoke0 须返装箱 Integer 引用,得 {r:?}");
        };
        assert_eq!(read_int_value(&vm, boxed, "java/lang/Integer"), 5);
    }

    /// **RED→GREEN**:invoke0 重载 + 拆箱 `Integer.parseInt("ff", 16)` → 255。
    /// 钉:形参 int 的包装类(Integer)拆箱(unbox_arg 读 value)+ 双参实参计数检查。
    #[test]
    fn invoke0_overload_unbox_returns_255() {
        if !has_jmod() {
            return;
        }
        let mut vm = reflection_vm();
        let int_mirror = vm.intern_class_mirror("java/lang/Integer");
        let (slot, modifiers) = find_method(
            &vm,
            "java/lang/Integer",
            "parseInt",
            "(Ljava/lang/String;I)I",
        );
        let method = build_executable_mirror(&mut vm, "java/lang/reflect/Method", int_mirror, slot, modifiers);

        let s_ff = crate::runtime::interpreter::string::intern(&mut vm, "ff").unwrap();
        let radix16 = box_primitive(&mut vm, "java/lang/Integer", Slot::Int(16));
        let args = object_array(&mut vm, vec![s_ff, radix16]);

        let r = super::super::invoke(
            &mut vm,
            NATIVE_ACCESSOR,
            "invoke0",
            INVOKE0_DESC,
            None,
            &[Value::Reference(method), Value::Reference(Reference::null()), Value::Reference(args)],
        )
        .expect("invoke0 parseInt(\"ff\",16) 应成功");
        let Value::Reference(boxed) = r else {
            panic!("invoke0 须返装箱 Integer 引用,得 {r:?}");
        };
        assert_eq!(read_int_value(&vm, boxed, "java/lang/Integer"), 255);
    }

    /// **RED→GREEN**:invoke0 目标方法抛异常 → 包成 InvocationTargetException(其
    /// `target` 字段 = 目标异常)。`Integer.parseInt("xyz")` → NumberFormatException。
    /// 对应 `Reflection::invoke` 的 `THROW_ARG(InvocationTargetException, &target_exception)`。
    #[test]
    fn invoke0_wraps_target_exception_in_ite() {
        if !has_jmod() {
            return;
        }
        let mut vm = reflection_vm();
        let int_mirror = vm.intern_class_mirror("java/lang/Integer");
        let (slot, modifiers) =
            find_method(&vm, "java/lang/Integer", "parseInt", "(Ljava/lang/String;)I");
        let method = build_executable_mirror(&mut vm, "java/lang/reflect/Method", int_mirror, slot, modifiers);

        let s_bad = crate::runtime::interpreter::string::intern(&mut vm, "xyz").unwrap();
        let args = object_array(&mut vm, vec![s_bad]);

        let err = super::super::invoke(
            &mut vm,
            NATIVE_ACCESSOR,
            "invoke0",
            INVOKE0_DESC,
            None,
            &[Value::Reference(method), Value::Reference(Reference::null()), Value::Reference(args)],
        )
        .unwrap_err();
        let VmError::ThrownException(ite) = err else {
            panic!("须 ThrownException(ITE),得 {err:?}");
        };
        // ITE 本身。
        match vm.heap().get(ite) {
            Some(Oop::Instance(i)) => assert_eq!(i.class_name(), "java/lang/reflect/InvocationTargetException"),
            o => panic!("ITE 须 Instance,得 {o:?}"),
        }
        // ITE.target = NumberFormatException。
        let target_ord = {
            let reg = vm.registry().expect("注册表");
            let lc = reg.get("java/lang/reflect/InvocationTargetException").unwrap();
            reg.flattened_instance_fields(&lc)
                .iter()
                .position(|f| f.name == "target")
                .expect("ITE.target 字段")
        };
        let target_ref = match vm.heap().get(ite) {
            Some(Oop::Instance(i)) => match i.field(target_ord) {
                Slot::Reference(r) => r,
                s => panic!("ITE.target 须引用,得 {s:?}"),
            },
            _ => unreachable!(),
        };
        match vm.heap().get(target_ref) {
            Some(Oop::Instance(i)) => {
                assert_eq!(i.class_name(), "java/lang/NumberFormatException")
            }
            o => panic!("target 须 Instance,得 {o:?}"),
        }
    }

    /// **RED→GREEN**:newInstance0 分配裸实例 + 跑 `<init>`。`Integer(int)` 构造器
    /// newInstance(7) → Integer 实例 value=7。钉:slot 解析 `<init>` + locals[0]=新实例
    /// + 拆箱 int 参 + 跑 `<init>` 字节码(super + 设 value)。
    #[test]
    fn new_instance0_runs_init_sets_value() {
        if !has_jmod() {
            return;
        }
        let mut vm = reflection_vm();
        let int_mirror = vm.intern_class_mirror("java/lang/Integer");
        let (slot, modifiers) = find_method(&vm, "java/lang/Integer", "<init>", "(I)V");
        let ctor = build_executable_mirror(
            &mut vm,
            "java/lang/reflect/Constructor",
            int_mirror,
            slot,
            modifiers,
        );

        let arg7 = box_primitive(&mut vm, "java/lang/Integer", Slot::Int(7));
        let args = object_array(&mut vm, vec![arg7]);

        let r = super::super::invoke(
            &mut vm,
            CTOR_NATIVE_ACCESSOR,
            "newInstance0",
            NEWINSTANCE0_DESC,
            None,
            &[Value::Reference(ctor), Value::Reference(args)],
        )
        .expect("newInstance0 Integer(7) 应成功");
        let Value::Reference(new_inst) = r else {
            panic!("newInstance0 须返新实例引用,得 {r:?}");
        };
        assert_eq!(read_int_value(&vm, new_inst, "java/lang/Integer"), 7);
    }
}
