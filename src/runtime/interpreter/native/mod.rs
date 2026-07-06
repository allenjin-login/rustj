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
//! 按声明类**前缀**路由到各包子模块的 `dispatch`;`java/lang/*` → [`java_lang`],
//! `jdk/internal/misc/*` → [`jdk_internal`]。新增 native 只动对应子模块,不再扫全表。
//!
//! **Step 0 源码依据**:
//! - `Object.hashCode` 的地址模式 = `synchronizer.cpp` `get_next_hash` mode 4(对象地址/标识);
//!   rustj 以句柄 id(堆槽号)为对象唯一标识。
//! - `System.currentTimeMillis/nanoTime` = `jvm.cpp` `JVM_CurrentTimeMillis` / `JVM_NanoTime`。

use crate::runtime::{Reference, Vm};

use super::{throw_exception, Value, VmError};

mod java_lang;
mod jdk_internal;
mod jdk_internal_loader;

/// native 方法分派入口(对应 HotSpot `prims/jvm.cpp` 的 `JVM_*` 桥 + `nativeLookup.cpp`
/// 解析到的 JDK 侧 `Java_*` 桥)。
///
/// - `class` = 声明该 native 的类内部名(注册键);
/// - `name` / `desc` = 方法名 / 描述符;
/// - `this` = 实例方法的接收者(静态方法为 `None`);
/// - `args` = 实参正序(`args[0]` = 第 0 形参)。
///
/// 推一个 native 栈帧(未登记 → `UnsatisfiedLinkError` 时栈轨迹含此帧,直答"缺哪个
/// native"),按声明类路由到包子模块 `dispatch`,出口**配对 pop**(覆盖所有 Ok/Err 路径)。
/// 返回值须匹配 `desc` 返回类型(void → [`Value::Void`])。
pub(super) fn invoke(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    vm.push_frame(class, name);
    let result = dispatch(vm, class, name, desc, this, args);
    vm.pop_frame();
    result
}

/// 按**声明类前缀**路由:任意类的 `registerNatives()V` → 空操作(rustj 编译期表,native 恒
/// 已注册);`java/lang/*` → [`java_lang`];`jdk/internal/misc/*` → [`jdk_internal`];
/// 其余 → `UnsatisfiedLinkError`(`nativeLookup.cpp` 解析失败的对应物)。
fn dispatch(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    // 任意类的 registerNatives()V —— 最高杠杆:解锁 System/Object 等真实 <clinit>
    // (否则其 <clinit> 调它即 UnsatisfiedLinkError)。
    if name == "registerNatives" && desc == "()V" {
        return Ok(Value::Void);
    }
    match class {
        c if c.starts_with("java/lang/") => java_lang::dispatch(vm, c, name, desc, this, args),
        "jdk/internal/misc/VM" | "jdk/internal/misc/CDS" | "jdk/internal/misc/Unsafe" => {
            jdk_internal::dispatch(vm, class, name, desc, this, args)
        }
        "jdk/internal/loader/NativeLibraries" | "jdk/internal/loader/NativeLibrary" => {
            jdk_internal_loader::dispatch(vm, class, name, desc, this, args)
        }
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
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
fn class_arg_name(vm: &Vm<'_>, args: &[Value]) -> Option<String> {
    let Value::Reference(r) = args.first().copied()? else {
        return None;
    };
    vm.mirror_internal_name(r).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 最小集的四个 native 均**不**触碰注册表(仅未登记路径 throw_exception 需之);
    // 故可用无注册表的 `Vm::default()` 直测分派逻辑。

    #[test]
    fn register_natives_is_noop_for_any_class() {
        let mut vm = crate::runtime::Vm::default();
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
        let mut vm = crate::runtime::Vm::default();
        // 句柄 id=7 → hashCode = 7(对象标识,mode 4)。同一对象返回同一值。
        let this = Reference::from_id(7);
        assert_eq!(
            invoke(&mut vm, "java/lang/Object", "hashCode", "()I", Some(this), &[]).unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn system_current_time_millis_returns_wall_clock_long() {
        let mut vm = crate::runtime::Vm::default();
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
        let mut vm = crate::runtime::Vm::default();
        assert!(matches!(
            invoke(&mut vm, "java/lang/System", "nanoTime", "()J", None, &[]).unwrap(),
            Value::Long(_)
        ));
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
        let mut vm = crate::runtime::Vm::new(&reg);
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
        let crate::oops::Oop::Instance(i) = vm.heap().get(exc).unwrap() else {
            panic!("须为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/NullPointerException");
    }

    #[test]
    fn float_to_raw_int_bits_is_ieee754_reinterpret() {
        let mut vm = crate::runtime::Vm::default();
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
        let mut vm = crate::runtime::Vm::default();
        let nan = f32::from_bits(0x7fc0_0042); // 一个带尾数的 NaN
        assert_eq!(
            invoke(&mut vm, "java/lang/Float", "floatToRawIntBits", "(F)I", None, &[Value::Float(nan)])
                .unwrap(),
            Value::Int(0x7fc0_0042)
        );
    }

    #[test]
    fn int_bits_to_float_is_inverse_of_raw() {
        let mut vm = crate::runtime::Vm::default();
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
        let mut vm = crate::runtime::Vm::default();
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
        let mut vm = crate::runtime::Vm::new(&reg);
        let err = invoke(&mut vm, "java/lang/Foo", "bar", "()V", None, &[]).unwrap_err();
        let crate::runtime::VmError::ThrownException(exc) = err else {
            panic!("未登记 native 应抛 ThrownException,得 {err:?}");
        };
        let Some(crate::oops::Oop::Instance(i)) = vm.heap().get(exc) else {
            panic!("UnsatisfiedLinkError 应为引导桩实例");
        };
        assert_eq!(i.class_name(), "java/lang/UnsatisfiedLinkError");
    }
}
