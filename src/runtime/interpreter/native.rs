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
use crate::runtime::{Reference, Slot, Vm};

use super::{capture_backtrace, throw_exception, Value, VmError};

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
        // (捕获栈回溯)。rustj 经 capture_backtrace 快照调用链入 exception_meta 并置真
        // Throwable 的 backtrace/depth 字段,对应 HotSpot 的栈回溯捕获;返回 this 以便链式。
        ("java/lang/Throwable", "fillInStackTrace", "(I)Ljava/lang/Throwable;") => {
            match this {
                Some(r) => {
                    capture_backtrace(vm, r);
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

        // Float/Double 的 IEEE-754 位转换 native(均 @IntrinsicCandidate)——位模式原样重解,
        // Rust `to_bits`/`from_bits` 安全实现。解锁 `Math.<clinit>`(其 negativeZeroFloatBits /
        // negativeZeroDoubleBits 静态字段初始化器 `Math.java:2043-2044` 调此二 raw native);
        // 进而解锁 `Arrays.copyOfRange`(`Math.min`)→ `String.<init>` → `StringBuilder.toString`。
        // 注:`floatToIntBits`/`doubleToLongBits`(非 raw)是纯 Java 字节码包装器(NaN 折叠到
        // 规范值后转调本 raw native),故不入此表。
        ("java/lang/Float", "floatToRawIntBits", "(F)I") => match args.first() {
            Some(Value::Float(f)) => Ok(Value::Int(f.to_bits() as i32)),
            _ => Err(VmError::BadConstant("floatToRawIntBits 实参须为 float")),
        },
        ("java/lang/Float", "intBitsToFloat", "(I)F") => match args.first() {
            Some(Value::Int(i)) => Ok(Value::Float(f32::from_bits(*i as u32))),
            _ => Err(VmError::BadConstant("intBitsToFloat 实参须为 int")),
        },
        ("java/lang/Double", "doubleToRawLongBits", "(D)J") => match args.first() {
            Some(Value::Double(d)) => Ok(Value::Long(d.to_bits() as i64)),
            _ => Err(VmError::BadConstant("doubleToRawLongBits 实参须为 double")),
        },
        ("java/lang/Double", "longBitsToDouble", "(J)D") => match args.first() {
            Some(Value::Long(l)) => Ok(Value::Double(f64::from_bits(*l as u64))),
            _ => Err(VmError::BadConstant("longBitsToDouble 实参须为 long")),
        },

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

        // Class.getClassLoader0()Ljava/lang/ClassLoader; —— Class.java:987(字段读字节码),
        // 但 Oop::Class 镜像经本表分派(无真实 ClassLoader 字段)。rustj 视所有类为引导类
        // → null。真 STE.computeFormat(STE.java:466)据此:`loader instanceof BuiltinClassLoader`
        // 对 null 安全(instanceof null → 0),不走该分支。
        ("java/lang/Class", "getClassLoader0", "()Ljava/lang/ClassLoader;") => {
            Ok(Value::Reference(Reference::null()))
        }
        // Class.getModule()Ljava/lang/Module; —— Class.java:1005。镜像无 Module 字段 → null。
        // computeFormat 传 m 给 isHashedInJavaBase(m)(STE.java:512):其首判
        // `!VM.isModuleSystemInited()`(VM.java:92,字节码读 initLevel,默认 0 < MODULE_SYSTEM_INITED
        // → 假;故 !假 = 真 → 返真,短路,不 deref m)→ format 取默认,无 NPE。
        ("java/lang/Class", "getModule", "()Ljava/lang/Module;") => {
            Ok(Value::Reference(Reference::null()))
        }

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

        // StackTraceElement.initStackTraceElements(ste[], backtrace, depth)V —— STE.java:590
        // private static native,由 STE.of(STE.java:556,经 Throwable.getOurStackTrace 转调)
        // 逐元素回填。backtrace = capture_backtrace 置入的 Throwable 自指句柄,据此从
        // exception_meta 取捕获帧,**逆序**(最内帧 → ste[0],Java 惯例)回填每个 STE 的
        // declaringClass/methodName/fileName/lineNumber + declaringClassObject(供随后
        // finishInit→computeFormat 判类加载器/模块;rustj Class 镜像无此二者,native 返 null)。
        ("java/lang/StackTraceElement", "initStackTraceElements",
         "([Ljava/lang/StackTraceElement;Ljava/lang/Object;I)V") => {
            let (elements, backtrace, depth) =
                match (args.first(), args.get(1), args.get(2)) {
                    (Some(Value::Reference(e)), Some(Value::Reference(b)), Some(Value::Int(d))) => {
                        (*e, *b, *d)
                    }
                    _ => return Err(VmError::BadConstant("initStackTraceElements 实参缺失/类型不符")),
                };
            init_stack_trace_elements(vm, elements, backtrace, depth)?;
            Ok(Value::Void)
        }

        // 未登记 → UnsatisfiedLinkError(nativeLookup.cpp 解析失败的对应物)。
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    };
    vm.pop_frame();
    result
}

/// `StackTraceElement.initStackTraceElements(ste[], backtrace, depth)` 的实现(见分派臂注释)。
/// 据 backtrace(= Throwable 自指句柄)取 exception_meta 捕获帧,**逆序**回填 ste[i] 五字段。
fn init_stack_trace_elements(
    vm: &mut Vm<'_>,
    elements: Reference,
    backtrace: Reference,
    depth: i32,
) -> Result<(), VmError> {
    use crate::metadata::descriptor::FieldType;

    const STE: &str = "java/lang/StackTraceElement";
    if depth <= 0 {
        return Ok(());
    }

    // 取捕获帧(exception_meta),逆序使最内帧对应 ste[0](Java 惯例)。to_vec 释放共享借用。
    let frames: Vec<crate::runtime::vm::CallFrame> = vm
        .exception_frames(backtrace)
        .map(|f| f.to_vec())
        .unwrap_or_default();
    if frames.is_empty() {
        return Ok(());
    }

    // 解析 STE 五字段序号(借注册表 'a;出块 owned,后续可 &mut vm)。
    let str_ft = FieldType::Class("java/lang/String".into());
    let class_ft = FieldType::Class("java/lang/Class".into());
    let (ord_dc, ord_mn, ord_fn, ord_ln, ord_dco) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("initStackTraceElements 需注册表"))?;
        let lc = reg
            .get(STE)
            .ok_or(VmError::BadConstant("StackTraceElement 未预载"))?;
        let ord_dc = reg
            .instance_field(lc, "declaringClass", &str_ft)
            .ok_or(VmError::BadConstant("STE.declaringClass 未找到"))?;
        let ord_mn = reg
            .instance_field(lc, "methodName", &str_ft)
            .ok_or(VmError::BadConstant("STE.methodName 未找到"))?;
        let ord_fn = reg
            .instance_field(lc, "fileName", &str_ft)
            .ok_or(VmError::BadConstant("STE.fileName 未找到"))?;
        let ord_ln = reg
            .instance_field(lc, "lineNumber", &FieldType::Int)
            .ok_or(VmError::BadConstant("STE.lineNumber 未找到"))?;
        let ord_dco = reg
            .instance_field(lc, "declaringClassObject", &class_ft)
            .ok_or(VmError::BadConstant("STE.declaringClassObject 未找到"))?;
        (ord_dc, ord_mn, ord_fn, ord_ln, ord_dco)
    };

    // 逐帧(逆序)回填 ste[i]。
    let n = (depth as usize).min(frames.len());
    for i in 0..n {
        let f = &frames[frames.len() - 1 - i];
        let declaring_dotted = f.class.replace('/', ".");
        let method = f.method.clone();
        let (file_owned, line) = match vm.frame_source(f) {
            Some((fl, ln)) => (Some(fl.to_string()), ln),
            None => (None, 0),
        };

        // 取 ste[i] 句柄(借堆读 → owned Reference,释放借用后再 &mut vm)。
        let ste_ref = match vm.heap().get(elements) {
            Some(Oop::Array(a)) => match a.element(i) {
                Slot::Reference(r) => Some(r),
                _ => None,
            },
            _ => None,
        };
        let Some(ste_ref) = ste_ref else {
            continue;
        };

        let dc_ref = super::string::intern(vm, &declaring_dotted)?;
        let mn_ref = super::string::intern(vm, &method)?;
        let fn_ref = match file_owned {
            Some(fl) => Some(super::string::intern(vm, &fl)?),
            None => None,
        };
        // declaringClassObject = 声明类的 Class 镜像(供 computeFormat 的非 null 哨兵)。
        let dco_ref = vm
            .heap_mut()
            .alloc(Oop::Class(ClassOop::new(f.class.clone())));

        if let Some(Oop::Instance(inst)) = vm.heap_mut().get_mut(ste_ref) {
            inst.set_field(ord_dc, Slot::Reference(dc_ref));
            inst.set_field(ord_mn, Slot::Reference(mn_ref));
            if let Some(fr) = fn_ref {
                inst.set_field(ord_fn, Slot::Reference(fr));
            }
            inst.set_field(ord_ln, Slot::Int(line as i32));
            inst.set_field(ord_dco, Slot::Reference(dco_ref));
        }
    }
    Ok(())
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
