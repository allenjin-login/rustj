//! `java/lang/*` 的 native 桥(`Throwable` / `Object` / `System` / `Float` / `Double` /
//! `Class` / `Runtime` / `String` / `StackTraceElement`)。语义移植自 `prims/jvm.cpp` 的
//! `JVM_*` 与 JDK 侧 `Java_*`。由 [`super::dispatch`] 按 `"java/lang/"` 前缀路由至此。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, Value, Vm, VmError};

use super::super::{capture_backtrace, throw_exception};

/// `java/lang/*` native 分派。未登记(类前缀命中但 (name,desc) 不匹配)→ `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // Throwable.fillInStackTrace(I)Ljava/lang/Throwable; —— 每个 Throwable 构造器首调
        // (捕获栈回溯)。rustj 经 capture_backtrace 快照调用链入 exception_meta 并置真
        // Throwable 的 backtrace/depth 字段,对应 HotSpot 的栈回溯捕获;返回 this 以便链式。
        ("java/lang/Throwable", "fillInStackTrace", "(I)Ljava/lang/Throwable;") => match this {
            Some(r) => {
                capture_backtrace(vm, r);
                Ok(Value::Reference(r))
            }
            None => Err(throw_exception(vm, "java/lang/NullPointerException")),
        },

        // Object.hashCode()I —— synchronizer.cpp get_next_hash mode 4(对象标识/地址)。
        // 句柄 id 即堆上唯一标识;null 收者(理论不可达,实例方法)兜底 0。
        ("java/lang/Object", "hashCode", "()I") => {
            let id = this.and_then(Reference::id).unwrap_or(0) as i32;
            Ok(Value::Int(id))
        }

        // Object.getClass()Ljava/lang/Class; —— Object.java:68 public final native
        // (HotSpot 为 intrinsic)。返接收者运行时类的 Class 镜像(intern:同类恒同引用,使
        // `obj.getClass() == Foo.class` 成立)。Instance→类名;Array→数组描述符([I/…);
        // Class 镜像自身→java.lang.Class。
        ("java/lang/Object", "getClass", "()Ljava/lang/Class;") => {
            let Some(r) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let name = match vm.heap().get(r) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                Some(Oop::Array(a)) => a.class_name().to_string(),
                Some(Oop::Class(_)) => "java/lang/Class".to_string(),
                _ => return Err(throw_exception(vm, "java/lang/InternalError")),
            };
            Ok(Value::Reference(vm.intern_class_mirror(&name)))
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
            super::super::arraycopy::system_arraycopy(vm, src, src_pos, dst, dst_pos, length)
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

        // Class.getPrimitiveClass(Ljava/lang/String;)Ljava/lang/Class;
        // —— jvm.cpp:770 JVM_FindPrimitiveClass:name2type → Universe::java_mirror。
        // 原语名 → Class 镜像;非原语名 → ClassNotFoundException。
        ("java/lang/Class", "getPrimitiveClass", "(Ljava/lang/String;)Ljava/lang/Class;") => {
            let Value::Reference(r) = args.first().copied().unwrap_or(Value::Void) else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            // 收参为真 String 实例(经 intern):读回文本取原语名。
            let Some(text) = super::super::string::read_text(vm, r)? else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            if !super::is_primitive_name(&text) {
                // 对应 jvm.cpp 的 THROW_MSG_NULL(ClassNotFoundException, utf)。
                return Err(throw_exception(vm, "java/lang/ClassNotFoundException"));
            }
            Ok(Value::Reference(vm.intern_class_mirror(&text)))
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

        // String.intern()Ljava/lang/String; —— String.java:5086 native。读 this 文本 → 经
        // StringPool 规范化(同文本恒同引用),返规范引用。对应 jvm.cpp JVM_InternString / HotSpot
        // StringTable。String 的其余方法(equals/hashCode/length/…)退役 Oop::String 后跑真字节码
        // (经 invokevirtual 正常分派 → StringLatin1/StringUTF16),不经本表。
        ("java/lang/String", "intern", "()Ljava/lang/String;") => {
            let Some(this_ref) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(text) = super::super::string::read_text(vm, this_ref)? else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            Ok(Value::Reference(super::super::string::intern(vm, &text)?))
        }

        // StackTraceElement.initStackTraceElements(ste[], backtrace, depth)V —— STE.java:590
        // private static native,由 STE.of(STE.java:556,经 Throwable.getOurStackTrace 转调)
        // 逐元素回填。backtrace = capture_backtrace 置入的 Throwable 自指句柄,据此从
        // exception_meta 取捕获帧,**逆序**(最内帧 → ste[0],Java 惯例)回填每个 STE 的
        // declaringClass/methodName/fileName/lineNumber + declaringClassObject(供随后
        // finishInit→computeFormat 判类加载器/模块;rustj Class 镜像无此二者,native 返 null)。
        (
            "java/lang/StackTraceElement",
            "initStackTraceElements",
            "([Ljava/lang/StackTraceElement;Ljava/lang/Object;I)V",
        ) => {
            let (elements, backtrace, depth) = match (args.first(), args.get(1), args.get(2)) {
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
    }
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

        let dc_ref = super::super::string::intern(vm, &declaring_dotted)?;
        let mn_ref = super::super::string::intern(vm, &method)?;
        let fn_ref = match file_owned {
            Some(fl) => Some(super::super::string::intern(vm, &fl)?),
            None => None,
        };
        // declaringClassObject = 声明类的 Class 镜像(intern;供 computeFormat 的非 null 哨兵)。
        let dco_ref = vm.intern_class_mirror(&f.class);

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
