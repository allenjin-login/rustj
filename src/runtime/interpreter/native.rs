//! 内置 native 方法分派表。
//!
//! 对应 HotSpot 的 native 解析与桥接链:
//! - `prims/nativeLookup.cpp`:`Java_<class>_<method>` 符号查找 / 显式 `registerNatives`
//!   (`JNINativeMethod[]`)——查找失败抛 `UnsatisfiedLinkError`。
//! - `prims/jvm.cpp`:VM 侧的 native 桥(`JVM_CurrentTimeMillis` / `JVM_NanoTime` 等),
//!   非 JDK 代码;JDK 的 `registerNatives` 把这些 `Java_*` / `JVM_*` 符号登记进方法槽。
//!
//! rustj 用**编译期**分派表(`match (class, name, desc)`)替代运行期符号查找——等价于
//! "符号在编译期已绑定",故 JDK 各类的 `registerNatives()V` 在此为**空操作**(native 恒已
//! "注册");未登记的 native → `UnsatisfiedLinkError`(`ThrownException`)。
//!
//! **Step 0 源码依据**:
//! - `Object.hashCode` 的地址模式 = `synchronizer.cpp` `get_next_hash` mode 4(对象地址/标识);
//!   rustj 以句柄 id(堆槽号)为对象唯一标识。
//! - `System.currentTimeMillis/nanoTime` = `jvm.cpp` `JVM_CurrentTimeMillis` / `JVM_NanoTime`。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::oops::{ClassOop, Oop};
use crate::runtime::{Reference, Vm};

use super::{throw_exception, Value, VmError};

/// 派发一个 native 方法调用。
///
/// - `class` = 声明该 native 的类内部名(注册键);
/// - `name` / `desc` = 方法名 / 描述符;
/// - `this` = 实例方法的接收者(静态方法为 `None`);
/// - `args` = 实参正序(`args[0]` = 第 0 形参)。
///
/// 返回值须匹配 `desc` 的返回类型(void → [`Value::Void`])。未注册的 native →
/// `UnsatisfiedLinkError`,对齐 `nativeLookup.cpp` 解析失败语义。
pub(super) fn invoke(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    // 推一个 native 栈帧:未登记 native 抛 UnsatisfiedLinkError 时,栈轨迹含此帧
    // (直接答"缺哪个 native")。出口配对 pop(覆盖所有 Ok/Err 臂)。
    vm.push_frame(class, name);
    let result = match (class, name, desc) {
        // Throwable.fillInStackTrace(I)Ljava/lang/Throwable; —— 每个 Throwable 构造器首调
        // (捕获栈回溯)。rustj 在此快照当前调用链(record_trace),对应 HotSpot 的栈回溯捕获;
        // 返回 this 以便链式。stub 异常不经真 <init>,其轨迹由 throw_exception 直接捕获。
        ("java/lang/Throwable", "fillInStackTrace", "(I)Ljava/lang/Throwable;") => {
            match this {
                Some(r) => {
                    vm.record_trace(r);
                    Ok(Value::Reference(r))
                }
                None => Err(throw_exception(vm, "java/lang/NullPointerException")),
            }
        }

        // JDK 各类私有 registerNatives()V:rustj 编译期表,native 恒已"注册" → 空操作。
        // 最高杠杆:解锁 System/Object 等真实 <clinit>(否则其 <clinit> 调它即 UnsatisfiedLinkError)。
        (_, "registerNatives", "()V") => Ok(Value::Void),

        // jdk.internal.misc.VM.initialize()V —— VM.java:451 私有 native,VM.<clinit> 首调,
        // 做 JDK 启动期一次性引导(保存属性 / 直接内存上限 / …)。rustj 无 launcher 传递的启动态,
        // 此处恒空操作(等价"VM 已初始化,无保存属性"——后续 getSavedProperty 读空表得 null)。
        ("jdk/internal/misc/VM", "initialize", "()V") => Ok(Value::Void),

        // jdk.internal.misc.CDS.initializeFromArchive(Ljava/lang/Class;)V —— CDS.java:130
        // public static native。HotSpot `JVM_InitializeFromArchive`:从 CDS/AOT 归档恢复类的
        // 归档静态状态;无归档(rustj 无 CDS)→ 空操作(归档字段留默认 null)。包装类(Integer/
        // Long/…)$<clinit> 经 `runtimeSetup()` 调之以尝试恢复 archivedCache;空操作后走"新建缓存"
        // 分支,即非 CDS 运行的规范行为。
        ("jdk/internal/misc/CDS", "initializeFromArchive", "(Ljava/lang/Class;)V") => Ok(Value::Void),

        // jdk.internal.misc.CDS.getCDSConfigStatus()I —— CDS.java:95 私有 native,<clinit> 经
        // `configStatus = getCDSConfigStatus()` 调之。HotSpot 返回 CDS 配置位掩码(cdsConfig.hpp:
        // IS_DUMPING_ARCHIVE / IS_USING_ARCHIVE / …);rustj 无 CDS → 恒 0(所有标志关闭),
        // 即 isUsingArchive()/isDumpingArchive()/… 均假——规范的非 CDS 运行。
        ("jdk/internal/misc/CDS", "getCDSConfigStatus", "()I") => Ok(Value::Int(0)),

        // Object.hashCode()I —— synchronizer.cpp get_next_hash mode 4(对象标识/地址)。
        // 句柄 id 即堆上唯一标识;null 收者(理论不可达,实例方法)兜底 0。
        ("java/lang/Object", "hashCode", "()I") => {
            let id = this.and_then(Reference::id).unwrap_or(0) as i32;
            Ok(Value::Int(id))
        }

        // System.arraycopy(Ljava/lang/Object;ILjava/lang/Object;II)V —— jvm.cpp:293-305
        // JVM_ArrayCopy → typeArrayKlass/objArrayKlass::copy_array。检查序(null→NPE、
        // 非数组/类型不符→ASE、负值/越界→AIOOBE)、引用 checkcast、重叠 memmove 见
        // `arraycopy::system_arraycopy`。高价值 native:解锁 StringBuilder/String 字节拷贝。
        ("java/lang/System", "arraycopy", "(Ljava/lang/Object;ILjava/lang/Object;II)V") => {
            let (src, src_pos, dst, dst_pos, length) =
                match (args.first(), args.get(1), args.get(2), args.get(3), args.get(4)) {
                    (
                        Some(Value::Reference(s)),
                        Some(Value::Int(sp)),
                        Some(Value::Reference(d)),
                        Some(Value::Int(dp)),
                        Some(Value::Int(l)),
                    ) => (*s, *sp, *d, *dp, *l),
                    _ => return Err(VmError::BadConstant("arraycopy 实参缺失/类型不符")),
                };
            super::arraycopy::system_arraycopy(vm, src, src_pos, dst, dst_pos, length)
        }

        // System.currentTimeMillis()J —— jvm.cpp JVM_CurrentTimeMillis:墙钟毫秒(自 Unix 纪元)。
        ("java/lang/System", "currentTimeMillis", "()J") => {
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Ok(Value::Long(millis))
        }

        // System.nanoTime()J —— jvm.cpp JVM_NanoTime。
        // 注:HotSpot 用单调高精度计数器;此处暂以墙钟纳秒充数(单调性债,顺延)。
        ("java/lang/System", "nanoTime", "()J") => {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);
            Ok(Value::Long(nanos))
        }

        // Class.getPrimitiveClass(Ljava/lang/String;)Ljava/lang/Class;
        // —— jvm.cpp:770 JVM_FindPrimitiveClass:name2type → Universe::java_mirror。
        // 原语名 → Class 镜像;非原语名 → ClassNotFoundException。
        ("java/lang/Class", "getPrimitiveClass", "(Ljava/lang/String;)Ljava/lang/Class;") => {
            let Value::Reference(r) = args.first().copied().unwrap_or(Value::Void) else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            // 收参为真 String 实例(经 intern):读回文本取原语名。
            let Some(text) = super::string::read_text(vm, r)? else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            if !is_primitive_name(&text) {
                // 对应 jvm.cpp 的 THROW_MSG_NULL(ClassNotFoundException, utf)。
                return Err(throw_exception(vm, "java/lang/ClassNotFoundException"));
            }
            let cls = Oop::Class(ClassOop::new(text));
            Ok(Value::Reference(vm.heap_mut().alloc(cls)))
        }

        // Class.desiredAssertionStatus()Z —— javac 断言初始化(`!Foo.class.desiredAssertionStatus()`)
        // 广见于 java.base 各 <clinit>。真 Class 类此法走 ClassLoader + desiredAssertionStatus0;
        // rustj 无断言支持 → 恒 false(断言禁用,即 `$assertionsDisabled = true`)。Oop::Class 镜像
        // 由 invoke 路径按 "java/lang/Class" 经本表分派(见 invoke_virtual/interface)。
        ("java/lang/Class", "desiredAssertionStatus", "()Z") => Ok(Value::Int(0)),

        // Runtime.maxMemory()J —— jvm.cpp JVM_MaxMemory:堆上限。rustj 堆为无界 Vec → 取 i64::MAX
        // (VM.saveProperties 存进 directMemory,本场景不用;真值无意义)。
        ("java/lang/Runtime", "maxMemory", "()J") => Ok(Value::Long(i64::MAX)),

        // jdk.internal.misc.Unsafe 的数组布局 native —— Unsafe.<clinit> 经
        // `theUnsafe.arrayBaseOffset(X[].class)` / `arrayIndexScale(X[].class)`(皆为**非 native**
        // 字节码包装器)转调私有 native `arrayBaseOffset0` / `arrayIndexScale0`,初始化各
        // ARRAY_*_BASE_OFFSET / _INDEX_SCALE 静态字段(ArraysSupport.<clinit> 读之,进而
        // StringLatin1.hashCode → ArraysSupport.hashCodeOfUnsigned 触发其初始化)。rustj 数组
        // 无真实内存偏移:基偏移取常量、刻度按组件类型大小,仅供偏移算术(mismatch 等);
        // **不参与 String.hashCode 计算**——后者经 `unsignedHashCode` 的朴素 baload 循环。
        ("jdk/internal/misc/Unsafe", "arrayBaseOffset0", "(Ljava/lang/Class;)I") => Ok(Value::Int(16)),
        ("jdk/internal/misc/Unsafe", "arrayIndexScale0", "(Ljava/lang/Class;)I") => {
            let scale = match class_arg_name(vm, args).as_deref() {
                Some("[B") | Some("[Z") => 1,
                Some("[C") | Some("[S") => 2,
                Some("[I") | Some("[F") => 4,
                Some("[J") | Some("[D") => 8,
                _ => 1, // 引用数组/未知 → 1(保守;hash 不用此值)
            };
            Ok(Value::Int(scale))
        }
        // 注:addressSize()/pageSize()/isBigEndian()/unalignedAccess() 均为返回常量字段
        // (ADDRESS_SIZE / PAGE_SIZE / BIG_ENDIAN / UNALIGNED_ACCESS)的字节码方法;这些字段在
        // Unsafe.class 中已是字面量初始化(不经 native),故 <clinit> 无更多 native 依赖。

        // String.intern()Ljava/lang/String; —— String.java:5086 native。读 this 文本 → 经
        // StringPool 规范化(同文本恒同引用),返规范引用。对应 jvm.cpp JVM_InternString / HotSpot
        // StringTable。String 的其余方法(equals/hashCode/length/…)退役 Oop::String 后跑真字节码
        // (经 invokevirtual 正常分派 → StringLatin1/StringUTF16),不经本表。
        ("java/lang/String", "intern", "()Ljava/lang/String;") => {
            let Some(this_ref) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(text) = super::string::read_text(vm, this_ref)? else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            Ok(Value::Reference(super::string::intern(vm, &text)?))
        }

        // 未登记 → UnsatisfiedLinkError(nativeLookup.cpp 解析失败的对应物)。
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    };
    vm.pop_frame();
    result
}

/// 原语关键字名(`"int"`/…/`"void"`)判定——`name2type` 的等价物
/// (jvm.cpp:770 `JVM_FindPrimitiveClass` 的 `t != T_ILLEGAL && !is_reference_type(t)`)。
fn is_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "int" | "long" | "byte" | "char" | "short" | "boolean" | "double" | "float" | "void"
    )
}

/// 取第 0 参(Class 镜像)的内部名(如 `[B`);非 Class 实参 / 悬空 → `None`。
/// 供 `Unsafe.arrayIndexScale(Class)` 按数组组件类型定刻度。
fn class_arg_name(vm: &Vm<'_>, args: &[Value]) -> Option<String> {
    let Value::Reference(r) = args.first().copied()? else {
        return None;
    };
    match vm.heap().get(r)? {
        Oop::Class(c) => Some(c.name().to_string()),
        _ => None,
    }
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
