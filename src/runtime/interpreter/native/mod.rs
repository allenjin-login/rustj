//! 内置 native 方法分派表(按声明类**包**拆子模块)。
//!
//! 对应 HotSpot 的 native 解析与桥接链:
//! - `prims/nativeLookup.cpp`:`Java_<class>_<method>` 符号查找 / 显式 `registerNatives`
//!   (`JNINativeMethod[]`)——查找失败抛 `UnsatisfiedLinkError`。
//! - `prims/jvm.cpp`:VM 侧的 native 桥(`JVM_CurrentTimeMillis` / `JVM_NanoTime` 等),
//!   非 JDK 代码;JDK 的 `registerNatives` 把这些 `Java_*` / `JVM_*` 符号登记进方法槽。
//!
//! rustj 用**编译期**分派表替代运行期符号查找——等价于"符号在编译期已绑定",故 JDK 各类的
//! `registerNatives()V` 在此为**空操作**(native 恒已"注册");未登记的 native →
//! `UnsatisfiedLinkError`(`ThrownException`)。
//!
//! **结构**(解决"一堵大 match"的可维护性,CLAUDE.md §6):[`invoke`] 仅做栈帧 push/pop +
//! 委托 [`invoke_inner`],后者**先查 [`NativeRegistry`]**(fn 指针表,各子模块经 [`natives!`]
//! 宏声明式登记)→ 命中即调 fn 指针;miss 走 [`dispatch`] 前缀路由 fallback(渐进迁移期,
//! Task 4–10 逐模块退役;Task 11 全删)。新增 native 只在对应子模块的 `natives! { ... }` 加一行
//!(并 [`register_all`] 登记),不再扫全表。
//!
//! **Step 0 源码依据**:
//! - `Object.hashCode` 的地址模式 = `synchronizer.cpp` `get_next_hash` mode 4(对象地址/标识);
//!   rustj 以句柄 id(堆槽号)为对象唯一标识。
//! - `System.currentTimeMillis/nanoTime` = `jvm.cpp` `JVM_CurrentTimeMillis` / `JVM_NanoTime`。

use crate::runtime::{Reference, VmThread};

use super::{Value, VmError};

/// 声明式登记一个模块的全部 native(替代手写 `match` + `dispatch` 路由)。生成该模块的
/// `pub(super) fn register(&mut NativeRegistry)`;每条 `(class,name,desc) => <闭包>`,闭包须
/// **非捕获**(在 `register(..., f: NativeFn)` 位协变为零成本 fn 指针;捕获即编译错——护栏)。
/// 须定义于子模块声明**之前**(文本作用域:子 mod 方能用裸 `natives!`)。用法见各 `native/<pkg>.rs`。
macro_rules! natives {
    ( $( ($class:literal, $name:literal, $desc:literal $(,)?) => $body:expr );* $(;)? ) => {
        pub(super) fn register(reg: &mut $crate::runtime::interpreter::native::NativeRegistry) {
            $(
                reg.register($class, $name, $desc, $body);
            )*
        }
    };
}

mod java_io;
mod java_lang;
mod java_lang_invoke;
mod jdk_internal;
mod jdk_internal_loader;
mod jdk_internal_reflect;
mod registry;
mod sun_nio_fs;

/// Native fn 指针注册表(Layer 4.17):替代 4.10c 编译期 `match`。
pub(crate) use registry::{NativeFn, NativeRegistry};

/// 反射装箱/拆箱原语族(G.4.1 lambda 适配器复用):`unbox_arg`(引用→原语读 value 字段)、
/// `alloc_wrapper`(原语→包装实例)、`primitive_wrapper`(原语→包装类名)。
pub(crate) use jdk_internal_reflect::{alloc_wrapper, primitive_wrapper, unbox_arg};

/// native 方法分派入口(对应 HotSpot `prims/jvm.cpp` 的 `JVM_*` 桥 + `nativeLookup.cpp`
/// 解析到的 JDK 侧 `Java_*` 桥)。
///
/// - `class` = 声明该 native 的类内部名(注册键);
/// - `name` / `desc` = 方法名 / 描述符;
/// - `this` = 实例方法的接收者(静态方法为 `None`);
/// - `args` = 实参正序(`args[0]` = 第 0 形参)。
///
/// 推一个 native 栈帧(未登记 → `UnsatisfiedLinkError` 时栈轨迹含此帧,直答"缺哪个
/// native"),委托 [`invoke_inner`] 解析+执行,出口**配对 pop**(覆盖所有 Ok/Err 路径)。
/// 返回值须匹配 `desc` 返回类型(void → [`Value::Void`])。
pub(super) fn invoke(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    vm.push_frame(class, name);
    let result = invoke_inner(vm, class, name, desc, this, args);
    vm.pop_frame();
    result
}

/// `invoke` 内核(已 push_frame):(1) 任意类的 `registerNatives()V` 空操作(rustj 编译期表,
/// native 恒已注册——JDK 侧 registerNatives 把 `Java_*`/`JVM_*` 登记进方法槽,rustj 无此
/// 运行期步骤);(2) 命中 `NativeRegistry` → 调 fn 指针;(3) miss → 旧 [`dispatch`] 前缀路由
/// fallback(渐进迁移期;全部模块迁完后删除,Task 11)。
fn invoke_inner(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    if name == "registerNatives" && desc == "()V" {
        return Ok(Value::Void);
    }
    if let Some(f) = vm.native_resolve(class, name, desc) {
        return f(vm, this, args);
    }
    dispatch(vm, class, name, desc, this, args)
}

/// 迁移期 fallback(`java/lang/` 已上 [`NativeRegistry`] 表,本函数恒未命中 → ULE)。
/// **Task 11 将删除本函数**:`invoke_inner` 直调 [`throw_unsatisfied_link_error`]
/// (registry 命中或 ULE,二选一)。保留至 Task 11 以隔离「最后一模块迁移」与「fallback 删除」两步。
fn dispatch(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    _args: &[Value],
) -> Result<Value, VmError> {
    Err(throw_unsatisfied_link_error(vm, class, name, desc))
}

/// 未登记 native → `UnsatisfiedLinkError`(带 `class.name desc` 诊断串,对应 HotSpot
/// `nativeLookup.cpp` 解析失败的报错)。`super::throw_exception_with_message` 写真 Throwable
/// 的 `detailMessage` 字段(4.x 异常桥),故诊断串经异常实例携带至 Java 侧可读。
fn throw_unsatisfied_link_error(vm: &mut VmThread, class: &str, name: &str, desc: &str) -> VmError {
    let msg = format!("{}.{} {}", class.replace('/', "."), name, desc);
    super::throw_exception_with_message(vm, "java/lang/UnsatisfiedLinkError", &msg)
}

/// 把所有内置 native 模块的 `register` 串调,填满 `NativeRegistry`(`Vm::new` 时一次性调)。
/// 渐进迁移:每迁一个模块,在此加一行 `<module>::register(reg);`(模块迁移见各 Task 4–10)。
/// **Task 3 阶段为空**(所有模块仍走 `dispatch` fallback);全部迁完后 fallback 删除(Task 11)。
pub(crate) fn register_all(reg: &mut NativeRegistry) {
    sun_nio_fs::register(reg);
    jdk_internal_loader::register(reg);
    java_io::register(reg);
    java_lang_invoke::register(reg);
    jdk_internal::register(reg);
    jdk_internal_reflect::register(reg);
    java_lang::register(reg);
}

/// 原语关键字名(`"int"`/…/`"void"`)判定——`name2type` 的等价物
/// (jvm.cpp:770 `JVM_FindPrimitiveClass` 的 `t != T_ILLEGAL && !is_reference_type(t)`)。
fn is_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "int" | "long" | "byte" | "char" | "short" | "boolean" | "double" | "float" | "void"
    )
}

/// 取第 0 参(Class 镜像)的内部名(如 `[B`);非 Class 镜像 / 悬空 → `None`。
/// 供 `Unsafe.arrayIndexScale(Class)` 按数组组件类型定刻度。镜像现为 `java/lang/Class`
/// Instance,所表示的类型经 `Vm::mirror_internal_name` 反查(4.12)。
fn class_arg_name(vm: &VmThread, args: &[Value]) -> Option<String> {
    let Value::Reference(r) = args.first().copied()? else {
        return None;
    };
    vm.mirror_internal_name(r).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 宏展开 → 生成 `pub(super) fn register(&mut NativeRegistry)`,登记 2 条测试 native。
    // 文本作用域:`natives!` 在本文件顶部定义、本 mod 在其后声明 → 本 mod 可见。
    natives! {
        ("test/Sample", "one", "()I") => |_vm, _this, _args| Ok(Value::Int(1));
        ("test/Sample", "two", "()I") => |_vm, _this, _args| Ok(Value::Int(2));
    }

    // 最小集的四个 native 均**不**触碰注册表(仅未登记路径 throw_exception 需之);
    // 故可用无注册表的 `Vm::default()` 直测分派逻辑。

    #[test]
    fn register_natives_is_noop_for_any_class() {
        let mut vm = crate::runtime::VmThread::default();
        // System / Object / Thread 等皆有 registerNatives()V —— 一律空操作。
        assert_eq!(
            invoke(&mut vm, "java/lang/System", "registerNatives", "()V", None, &[]).unwrap(),
            Value::Void
        );
        assert_eq!(
            invoke(&mut vm, "java/lang/Object", "registerNatives", "()V", None, &[]).unwrap(),
            Value::Void
        );
    }

    #[test]
    fn object_hashcode_is_handle_id_mode4() {
        let mut vm = crate::runtime::VmThread::default();
        // 句柄 id=7 → hashCode = 7(对象标识,mode 4)。同一对象返回同一值。
        let this = Reference::from_id(7);
        assert_eq!(
            invoke(&mut vm, "java/lang/Object", "hashCode", "()I", Some(this), &[]).unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn system_current_time_millis_returns_wall_clock_long() {
        let mut vm = crate::runtime::VmThread::default();
        match invoke(&mut vm, "java/lang/System", "currentTimeMillis", "()J", None, &[]).unwrap() {
            Value::Long(millis) => {
                // 墙钟毫秒:2023-11 之后(> 1.7e12),且随调用单调不退。
                assert!(millis > 1_700_000_000_000, "currentTimeMillis 应为当前墙钟毫秒: {millis}");
            }
            other => panic!("期望 Long,得 {other:?}"),
        }
    }

    #[test]
    fn system_nano_time_returns_long() {
        let mut vm = crate::runtime::VmThread::default();
        assert!(matches!(
            invoke(&mut vm, "java/lang/System", "nanoTime", "()J", None, &[]).unwrap(),
            Value::Long(_)
        ));
    }

    // Runtime.availableProcessors()I —— jvm.cpp JVM_ActiveProcessorCount。返 CPU 核数(≥1)。
    #[test]
    fn runtime_available_processors_returns_positive() {
        // 须注册表:未登记臂走 throw_exception 须有引导桩(RED 阶段);GREEN 后本臂不触之。
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::VmThread::new(reg);
        match invoke(&mut vm, "java/lang/Runtime", "availableProcessors", "()I", None, &[]).unwrap() {
            Value::Int(n) => assert!(n >= 1, "availableProcessors 须 ≥1,得 {n}"),
            other => panic!("期望 Int,得 {other:?}"),
        }
    }

    // getPrimitiveClass 的 String 收参路径(返原语 Class 镜像 / 非原语抛 ClassNotFoundException)
    // 经集成闸门覆盖:`real_integer.rs` 的 `Integer.<clinit>` 调 `Class.getPrimitiveClass("int")`
    // 端到端(须先预载真 String)。单测层面仅覆盖缺参 → NPE 与 `is_primitive_name` 纯逻辑。

    #[test]
    fn is_primitive_name_recognizes_keywords() {
        assert!(is_primitive_name("int"));
        assert!(is_primitive_name("void"));
        assert!(!is_primitive_name("java/lang/Object"));
        assert!(!is_primitive_name("String"));
    }

    #[test]
    fn get_primitive_class_missing_arg_throws_npe() {
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::VmThread::new(reg);
        let err = invoke(
            &mut vm,
            "java/lang/Class",
            "getPrimitiveClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            None,
            &[],
        )
        .unwrap_err();
        let crate::runtime::VmError::ThrownException(exc) = err else {
            panic!("应抛 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let crate::oops::Oop::Instance(i) = heap.get(exc).unwrap() else {
            panic!("须为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/NullPointerException");
    }

    #[test]
    fn float_to_raw_int_bits_is_ieee754_reinterpret() {
        let mut vm = crate::runtime::VmThread::default();
        // 1.0f 的 IEEE-754 位 = 0x3f800000。
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "floatToRawIntBits", "(F)I", None, &[Value::Float(1.0)])
                .unwrap(),
            Value::Int(0x3f800000)
        );
        // -0.0f(Math.<clinit> 的 negativeZeroFloatBits 来源)= 0x80000000。
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "floatToRawIntBits", "(F)I", None, &[Value::Float(-0.0)])
                .unwrap(),
            Value::Int(0x8000_0000u32 as i32)
        );
        // 正无穷 = 0x7f800000。
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "floatToRawIntBits", "(F)I", None, &[Value::Float(f32::INFINITY)])
                .unwrap(),
            Value::Int(0x7f800000)
        );
    }

    #[test]
    fn float_to_raw_int_bits_preserves_nan_bits() {
        // raw 变体**不**折叠 NaN:特定 NaN 位模式原样保留(与 floatToIntBits 折叠到 0x7fc00000 区分)。
        let mut vm = crate::runtime::VmThread::default();
        let nan = f32::from_bits(0x7fc0_0042); // 一个带尾数的 NaN
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "floatToRawIntBits", "(F)I", None, &[Value::Float(nan)])
                .unwrap(),
            Value::Int(0x7fc0_0042)
        );
    }

    #[test]
    fn int_bits_to_float_is_inverse_of_raw() {
        let mut vm = crate::runtime::VmThread::default();
        // 0x3f800000 → 1.0f;0x80000000 → -0.0f。
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "intBitsToFloat", "(I)F", None, &[Value::Int(0x3f800000)])
                .unwrap(),
            Value::Float(1.0)
        );
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "intBitsToFloat", "(I)F", None, &[Value::Int(0x8000_0000u32 as i32)])
                .unwrap(),
            Value::Float(-0.0)
        );
        // 位 → NaN(逆:from_bits 与 to_bits 互逆;NaN!=NaN 故比位,不比值)。
        let got = invoke(&mut vm, "java/lang/Float", "intBitsToFloat", "(I)F", None, &[Value::Int(0x7fc0_0042)])
            .unwrap();
        let Value::Float(f) = got else {
            panic!("须 Float,得 {got:?}");
        };
        assert_eq!(f.to_bits(), 0x7fc0_0042, "intBitsToFloat 须原样保留 NaN 位");
    }

    #[test]
    fn double_to_raw_long_bits_and_inverse() {
        let mut vm = crate::runtime::VmThread::default();
        // 1.0d 的 IEEE-754 位 = 0x3ff0000000000000(Math.<clinit> 经此路径取 negativeZeroDoubleBits)。
        assert_eq!(
            invoke(&mut vm, "java/lang/Double", "doubleToRawLongBits", "(D)J", None, &[Value::Double(1.0)])
                .unwrap(),
            Value::Long(0x3ff0_0000_0000_0000u64 as i64)
        );
        // -0.0d = 0x8000000000000000。
        assert_eq!(
            invoke(&mut vm, "java/lang/Double", "doubleToRawLongBits", "(D)J", None, &[Value::Double(-0.0)])
                .unwrap(),
            Value::Long(i64::MIN)
        );
        // 逆:longBitsToDouble。
        assert_eq!(
            invoke(&mut vm, "java/lang/Double", "longBitsToDouble", "(J)D", None, &[Value::Long(0x3ff0_0000_0000_0000u64 as i64)])
                .unwrap(),
            Value::Double(1.0)
        );
        // raw 保留 NaN 位(long 级;NaN!=NaN 故比位)。
        let nan_bits = 0x7ff8_0000_0000_0042u64 as i64;
        let got = invoke(&mut vm, "java/lang/Double", "longBitsToDouble", "(J)D", None, &[Value::Long(nan_bits)])
            .unwrap();
        let Value::Double(d) = got else {
            panic!("须 Double,得 {got:?}");
        };
        assert_eq!(d.to_bits(), nan_bits as u64, "longBitsToDouble 须原样保留 NaN 位");
    }

    #[test]
    fn unbound_native_throws_unsatisfied_link_error() {
        // 未登记的 native → UnsatisfiedLinkError(须有注册表:throw_exception 取引导桩)。
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::VmThread::new(reg);
        let err = invoke(&mut vm, "java/lang/Foo", "bar", "()V", None, &[]).unwrap_err();
        let crate::runtime::VmError::ThrownException(exc) = err else {
            panic!("未登记 native 应抛 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(crate::oops::Oop::Instance(i)) = heap.get(exc) else {
            panic!("UnsatisfiedLinkError 应为引导桩实例");
        };
        assert_eq!(i.class_name(), "java/lang/UnsatisfiedLinkError");
    }

    /// `natives!` 宏生成的 `register(&mut NativeRegistry)` 须把每条 (class,name,desc)=>闭包
    /// 登记进表;非捕获闭包协变为 fn 指针。
    #[test]
    fn natives_macro_generates_register() {
        let mut reg = NativeRegistry::new();
        register(&mut reg); // 宏在本 mod 作用域生成的 register。
        let mut vm = crate::runtime::VmThread::default();
        assert_eq!(
            reg.resolve("test/Sample", "one", "()I").unwrap()(&mut vm, None, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            reg.resolve("test/Sample", "two", "()I").unwrap()(&mut vm, None, &[]).unwrap(),
            Value::Int(2)
        );
        assert!(reg.resolve("test/Sample", "missing", "()I").is_none());
    }
}
