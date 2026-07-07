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

        // Object.notify()/notifyAll()/wait() —— jvm.cpp JVM_MonitorNotify/NotifyAll/Wait。
        // rustj 单线程:管程恒已满足、无 wait set → **空操作**(无并发线程可唤醒/等待)。
        // 解锁 `synchronized` 块尾的 `lock.notifyAll()`(如 VM.initLevel)等;真阻塞/调度顺延。
        ("java/lang/Object", "notify", "()V") => Ok(Value::Void),
        ("java/lang/Object", "notifyAll", "()V") => Ok(Value::Void),
        ("java/lang/Object", "wait", "()V") => Ok(Value::Void),
        ("java/lang/Object", "wait", "(J)V") => Ok(Value::Void),
        ("java/lang/Object", "wait", "(JI)V") => Ok(Value::Void),

        // Object.getClass()Ljava/lang/Class; —— Object.java:68 public final native
        // (HotSpot 为 intrinsic)。返接收者运行时类的 Class 镜像(intern:同类恒同引用,使
        // `obj.getClass() == Foo.class` 成立)。Instance→类名(Class 镜像自身为 java/lang/Class
        // Instance → 其 getClass 返 java/lang/Class 镜像,自洽);Array→数组描述符([I/…)。
        ("java/lang/Object", "getClass", "()Ljava/lang/Class;") => {
            let Some(r) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let name = match vm.heap().get(r) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                Some(Oop::Array(a)) => a.class_name().to_string(),
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

        // Class.forName0(Ljava/lang/String;ZLjava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/Class;
        // —— Class.java 私有 static native,经 `forName(name, init, loader)` 调。第 4 参 `caller`
        // (Class,安全/类加载器上下文)在 rustj 恒 Bootstrap——忽略。移植 jvm.cpp
        // `JVM_FindClassFromCaller` 语义:按名(点形 "java.lang.Integer")查注册表 →
        // `initialize=true` 触发 `ensure_class_initialized` → 返类镜像;未找到 →
        // `ClassNotFoundException`(jvm.cpp THROW_MSG_NULL)。loader 在 rustj 恒 Bootstrap
        // (Class.classLoader=null),故不查 ClassPath——反射仅解析已加载的类。
        ("java/lang/Class", "forName0", "(Ljava/lang/String;ZLjava/lang/ClassLoader;Ljava/lang/Class;)Ljava/lang/Class;") => {
            let Value::Reference(r) = args.first().copied().unwrap_or(Value::Void) else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(text) = super::super::string::read_text(vm, r)? else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let initialize = matches!(args.get(1), Some(Value::Int(n)) if *n != 0);
            let internal = text.replace('.', "/");
            let found = vm.registry().is_some_and(|reg| reg.get(&internal).is_some());
            if !found {
                return Err(throw_exception(vm, "java/lang/ClassNotFoundException"));
            }
            if initialize {
                super::super::clinit::ensure_class_initialized(vm, &internal)?;
            }
            Ok(Value::Reference(vm.intern_class_mirror(&internal)))
        }
        // Class.getDeclaredFields0(Z)[Ljava/lang/reflect/Field; —— Class.java:3246 私有 native,
        // 经 `privateGetDeclaredFields`→`Reflection.filterFields`(透传)后由 `copyFields` 包装。
        // 移植 HotSpot semantics:遍历声明类 `cf.fields`,按 `publicOnly`(ACC_PUBLIC)过滤,逐字段
        // 构造真 `java/lang/reflect/Field` Instance 并置 `clazz`/`slot`/`name`/`type`/`modifiers`
        // 字段(`trustedFinal` 默认 false),返回 `[Ljava/lang/reflect/Field;`。`slot` = 字段在**本类
        // 声明序**(`copyField` 经 `Field.copy()` 透传;getName/getModifiers 不读 slot,4.15b get/set
        // 用到时再定语义)。`type` = 字段描述符→内部名→Class 镜像(原语 "I"→"int" 原语镜像)。
        ("java/lang/Class", "getDeclaredFields0", "(Z)[Ljava/lang/reflect/Field;") => {
            get_declared_fields0(vm, this, args)
        }
        // Class.getDeclaredMethods0(Z)[Ljava/lang/reflect/Method; —— Class.java:3247 私有 native,
        // 经 `privateGetDeclaredMethods`→`Reflection.filterMethods`→`copyMethods` 包装。遍历声明类
        // `cf.methods`,按 publicOnly(ACC_PUBLIC)过滤,逐方法构造真 `java/lang/reflect/Method`
        // Instance 并置 `clazz`/`slot`/`name`/`returnType`/`parameterTypes`/`exceptionTypes`/`modifiers`
        // (`parameterTypes`/`exceptionTypes` 为 Class[],经方法描述符解析)。返 `[Ljava/lang/reflect/Method;`。
        ("java/lang/Class", "getDeclaredMethods0", "(Z)[Ljava/lang/reflect/Method;") => {
            get_declared_methods0(vm, this, args)
        }
        // Class.getDeclaredConstructors0(Z)[Ljava/lang/reflect/Constructor; —— Class.java:3248 私有
        // native,经 `privateGetDeclaredConstructors`→`Reflection.filterConstructors`→`copyConstructors`
        // 包装。构造器无 name/returnType,字段为 `clazz`/`slot`/`parameterTypes`/`exceptionTypes`/
        // `modifiers`。返 `[Ljava/lang/reflect/Constructor;`。
        (
            "java/lang/Class",
            "getDeclaredConstructors0",
            "(Z)[Ljava/lang/reflect/Constructor;",
        ) => get_declared_constructors0(vm, this, args),
        // desiredAssertionStatus() 字节码 `return desiredAssertionStatus0(this)` 调)。rustj
        // 无断言支持 → 恒 false(断言禁用,即 `$assertionsDisabled = true`)。
        ("java/lang/Class", "desiredAssertionStatus0", "(Ljava/lang/Class;)Z") => Ok(Value::Int(0)),

        // Class.initClassName()Ljava/lang/String; —— Class.java:967 真原生;getName() 字节码
        // 首次(`name == null`)调此。按镜像反查内部名→外部形(`/`→`.`),经 string::intern 造真
        // String,回填 `name` 字段并返之;后续 getName 直接读字段(不再进 native)。
        ("java/lang/Class", "initClassName", "()Ljava/lang/String;") => {
            let Some(this) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(internal) = vm.mirror_internal_name(this).map(|s| s.to_string()) else {
                return Err(throw_exception(vm, "java/lang/InternalError"));
            };
            let external = internal.replace('/', ".");
            let s = super::super::string::intern(vm, &external)?;
            vm.set_class_instance_field(this, "name", Slot::Reference(s));
            Ok(Value::Reference(s))
        }

        // Class.isInstance(Ljava/lang/Object;)Z —— Class.java:768 真原生。obj 的运行时类是否
        // 本镜像类的子类型 = is_instance(obj_class, this_internal)(registry 语义:子类型关系)。
        ("java/lang/Class", "isInstance", "(Ljava/lang/Object;)Z") => {
            let Some(this) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(this_internal) = vm.mirror_internal_name(this).map(|s| s.to_string()) else {
                return Err(throw_exception(vm, "java/lang/InternalError"));
            };
            let Value::Reference(objref) = args.first().copied().unwrap_or(Value::Void) else {
                return Ok(Value::Int(0));
            };
            if objref.is_null() {
                return Ok(Value::Int(0));
            }
            let Some(reg) = vm.registry() else {
                return Ok(Value::Int(0));
            };
            let arg_class = match vm.heap().get(objref) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                Some(Oop::Array(a)) => a.class_name().to_string(),
                _ => return Ok(Value::Int(0)),
            };
            // is_instance(X, Y) = "X 是 Y 的子类型";obj 是本类实例 ⇔ arg_class 是 this 的子类型
            // ⇔ is_instance(arg_class, this_internal)。
            Ok(Value::Int(if reg.is_instance(&arg_class, &this_internal) {
                1
            } else {
                0
            }))
        }

        // Class.isAssignableFrom(Ljava/lang/Class;)Z —— Class.java:795 真原生。arg 镜像类是否
        // 本镜像类的子类型 = is_instance(arg_internal, this_internal)。
        ("java/lang/Class", "isAssignableFrom", "(Ljava/lang/Class;)Z") => {
            let Some(this) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(this_internal) = vm.mirror_internal_name(this).map(|s| s.to_string()) else {
                return Err(throw_exception(vm, "java/lang/InternalError"));
            };
            let Value::Reference(arg_mirror) = args.first().copied().unwrap_or(Value::Void) else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(arg_internal) = vm.mirror_internal_name(arg_mirror).map(|s| s.to_string()) else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(reg) = vm.registry() else {
                return Ok(Value::Int(0));
            };
            Ok(Value::Int(if reg.is_instance(&arg_internal, &this_internal) {
                1
            } else {
                0
            }))
        }

        // Class.getSuperclass()Ljava/lang/Class; —— Class.java:1066 真原生。镜像类的直接超类
        // → 其镜像;数组→Object;原语/void(注册表无对应 LoadedClass)→ null。接口语义顺延
        //(接口 classfile 的 super 为 Object,故接口暂返 Object 镜像;完整接口判定顺延)。
        ("java/lang/Class", "getSuperclass", "()Ljava/lang/Class;") => {
            let Some(this) = this else {
                return Err(throw_exception(vm, "java/lang/NullPointerException"));
            };
            let Some(internal) = vm.mirror_internal_name(this).map(|s| s.to_string()) else {
                return Err(throw_exception(vm, "java/lang/InternalError"));
            };
            let super_name = if internal.starts_with('[') {
                Some("java/lang/Object".to_string())
            } else {
                vm.registry()
                    .and_then(|reg| reg.get(&internal))
                    .and_then(|lc| lc.super_class_name().map(|s| s.to_string()))
            };
            let result = match super_name {
                Some(s) => vm.intern_class_mirror(&s),
                None => Reference::null(),
            };
            Ok(Value::Reference(result))
        }

        // Runtime.maxMemory()J —— jvm.cpp JVM_MaxMemory:堆上限。rustj 堆为无界 Vec → 取 i64::MAX
        // (VM.saveProperties 存进 directMemory,本场景不用;真值无意义)。
        ("java/lang/Runtime", "maxMemory", "()J") => Ok(Value::Long(i64::MAX)),

        // Runtime.availableProcessors()I —— jvm.cpp JVM_ActiveProcessorCount:CPU 核数。
        // 经 std::thread::available_parallelism;失败或 >i32::MAX → 1(规范下限 ≥1)。
        // 解锁 ConcurrentHashMap.<clinit>(runtimeSetup 读 NCPU)等依赖核数的 <clinit>。
        ("java/lang/Runtime", "availableProcessors", "()I") => {
            let n = std::thread::available_parallelism()
                .map(|nz| nz.get())
                .unwrap_or(1)
                .try_into()
                .unwrap_or(1);
            Ok(Value::Int(n))
        }

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

        // Reference.refersTo0(Object)Z —— Reference.java:373 native。this.referent 与 o 比
        // 引用身份(JVM_ReferenceRefersTo)。referent 由 Reference.<init>(T) 置(Reference.java:532,
        // 普通实例字段——rustj 无 GC,不特殊处理)。子类(WeakReference 等)扁平布局 referent 同序,
        // 故按**实例声明类**查 ord。
        ("java/lang/ref/Reference", "refersTo0", "(Ljava/lang/Object;)Z") => {
            let (this_ref, o) = match (this, args.first()) {
                (Some(t), Some(Value::Reference(o))) => (t, *o),
                _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
            };
            // 取实例声明类 → 扁平布局查 "referent" ord(§6:'a 不绑 &self,出块 owned)。
            let cn = vm
                .heap()
                .get(this_ref)
                .and_then(|obj| match obj {
                    Oop::Instance(i) => Some(i.class_name().to_string()),
                    _ => None,
                });
            let ord = cn.as_ref().and_then(|cn| {
                vm.registry().and_then(|reg| {
                    reg.get(cn).and_then(|lc| {
                        reg.flattened_instance_fields(lc)
                            .iter()
                            .position(|f| f.name == "referent")
                    })
                })
            });
            let refers = matches!(
                (ord, vm.heap().get(this_ref)),
                (Some(ord), Some(Oop::Instance(i))) if matches!(i.field(ord), Slot::Reference(r) if r == o)
            );
            Ok(Value::Int(if refers { 1 } else { 0 }))
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

/// `Class.getDeclaredFields0(Z)[Ljava/lang/reflect/Field;` 实现(见分派臂注释)。
///
/// 两阶段(避开 registry 不可变借与 heap `&mut` 冲突):
/// 1. 借注册表(§6:'a 不绑 &self)收字段元数据 + 解析 Field 类字段序号,出块为 owned;
/// 2. 独占 `&mut vm` 分配 Field[] + 逐字段 Instance 并填,无残余借用。
fn get_declared_fields0(
    vm: &mut Vm<'_>,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    use crate::constant_pool::ConstantPoolEntry;
    use crate::metadata::access_flags::ACC_PUBLIC;
    use crate::oops::ArrayOop;

    let Some(this) = this else {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    };
    // 先确保 `java/lang/reflect/AccessibleObject` 已初始化:其 `<clinit>`(AccessibleObject.java:78)
    // 调 `SharedSecrets.setJavaLangReflectAccess(new ReflectAccess())`。`ReflectionFactory` 构造时
    // 一次性缓存该值(final 字段),若本 native 返回后 Java 侧 `copyFields` 才首次构造
    // ReflectionFactory,须在此之前把 SharedSecrets 置好,否则 `langReflectAccess` 恒 null → NPE。
    // Field extends AccessibleObject → 反射使用时 AccessibleObject 必已加载,故 ensure 安全。
    super::super::clinit::ensure_class_initialized(vm, "java/lang/reflect/AccessibleObject")?;
    let public_only = matches!(args.first(), Some(Value::Int(n)) if *n != 0);

    // 阶段 1:借注册表收 (name,desc,access) 元组 + Field 类字段序号。
    let (md, field_ords): (Vec<(String, String, u16)>, FieldOrds) = {
        let internal = vm
            .mirror_internal_name(this)
            .ok_or(VmError::BadConstant("getDeclaredFields0:非 Class 镜像"))?
            .to_string();
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("getDeclaredFields0 需注册表"))?;
        let lc = reg
            .get(&internal)
            .ok_or(VmError::BadConstant("getDeclaredFields0:声明类未加载"))?;
        let md: Vec<(String, String, u16)> = lc
            .cf
            .fields
            .iter()
            .filter(|f| !public_only || f.access_flags.bits() & ACC_PUBLIC != 0)
            .filter_map(|f| {
                let name = match lc.cf.constant_pool.get(f.name_index) {
                    Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
                    _ => return None,
                };
                let desc = match lc.cf.constant_pool.get(f.descriptor_index) {
                    Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
                    _ => return None,
                };
                Some((name, desc, f.access_flags.bits()))
            })
            .collect();
        let field_lc = reg
            .get("java/lang/reflect/Field")
            .ok_or(VmError::BadConstant("java/lang/reflect/Field 未预载"))?;
        let flat = reg.flattened_instance_fields(field_lc);
        let find = |n: &str| {
            flat.iter().position(|f| f.name == n).ok_or(VmError::BadConstant(
                "java/lang/reflect/Field 缺 clazz/slot/name/type/modifiers 之一",
            ))
        };
        (
            md,
            FieldOrds {
                clazz: find("clazz")?,
                slot: find("slot")?,
                name: find("name")?,
                r#type: find("type")?,
                modifiers: find("modifiers")?,
            },
        )
    };

    // 阶段 2:分配 Field[](null 填充)+ 逐字段 Instance 填字段入数组。
    let arr = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
        "[Ljava/lang/reflect/Field;".to_string(),
        vec![Slot::Reference(Reference::null()); md.len()],
    )));
    for (i, (name, desc, access)) in md.into_iter().enumerate() {
        let field_ref = {
            let reg = vm
                .registry()
                .ok_or(VmError::BadConstant("getDeclaredFields0 需注册表"))?;
            let field_lc = reg
                .get("java/lang/reflect/Field")
                .ok_or(VmError::BadConstant("java/lang/reflect/Field 未预载"))?;
            vm.heap_mut()
                .alloc(Oop::Instance(reg.new_instance(field_lc)))
        };
        let type_mirror = vm.intern_class_mirror(&field_type_internal(&desc));
        let name_ref = super::super::string::intern(vm, &name)?;
        if let Some(Oop::Instance(inst)) = vm.heap_mut().get_mut(field_ref) {
            inst.set_field(field_ords.clazz, Slot::Reference(this));
            inst.set_field(field_ords.slot, Slot::Int(i as i32));
            inst.set_field(field_ords.name, Slot::Reference(name_ref));
            inst.set_field(field_ords.r#type, Slot::Reference(type_mirror));
            inst.set_field(field_ords.modifiers, Slot::Int(access as i32));
        }
        if let Some(Oop::Array(a)) = vm.heap_mut().get_mut(arr) {
            a.set_element(i, Slot::Reference(field_ref));
        }
    }
    Ok(Value::Reference(arr))
}

/// `java/lang/reflect/Field` 实例五字段在扁平布局中的序号(供 [`get_declared_fields0`] 填字段)。
struct FieldOrds {
    clazz: usize,
    slot: usize,
    name: usize,
    r#type: usize,
    modifiers: usize,
}

/// 字段描述符 → 其类型的 Class 镜像内部名(供 `Field.type` 字段):原语单字符→关键字
/// (`I`→`int`、`J`→`long` …)、`Lx/y/Z;`→`x/y/Z`、数组描述符(`[…]`)原样保留(数组类名)。
fn field_type_internal(desc: &str) -> String {
    let mapped = match desc {
        "B" => "byte",
        "C" => "char",
        "D" => "double",
        "F" => "float",
        "I" => "int",
        "J" => "long",
        "S" => "short",
        "Z" => "boolean",
        "V" => "void",
        s if s.starts_with('L') && s.ends_with(';') && s.len() >= 3 => &s[1..s.len() - 1],
        _ => return desc.to_string(),
    };
    mapped.to_string()
}

/// `FieldType` → Class 镜像内部名:原语变体→关键字、Class→内部名、Array→描述符形式(数组类名)。
/// 供 Method/Constructor 的 parameterTypes/returnType(Class[]/Class)构造。
fn field_type_to_class_name(ft: &crate::metadata::descriptor::FieldType) -> String {
    use crate::metadata::descriptor::FieldType;
    match ft {
        FieldType::Byte => "byte",
        FieldType::Char => "char",
        FieldType::Double => "double",
        FieldType::Float => "float",
        FieldType::Int => "int",
        FieldType::Long => "long",
        FieldType::Short => "short",
        FieldType::Boolean => "boolean",
        FieldType::Class(name) => return name.clone(),
        FieldType::Array(_) => return ft.to_string(),
    }
    .to_string()
}

/// 构造 `[Ljava/lang/Class;` 数组,元素为各 `FieldType` 的 Class 镜像(供 parameterTypes/
/// exceptionTypes)。空切片 → 长度 0 的 Class[](getParameterCount 返 0)。
fn class_array_of(vm: &mut Vm<'_>, types: &[crate::metadata::descriptor::FieldType]) -> Reference {
    use crate::oops::ArrayOop;
    let elements: Vec<Slot> = types
        .iter()
        .map(|t| Slot::Reference(vm.intern_class_mirror(&field_type_to_class_name(t))))
        .collect();
    vm.heap_mut()
        .alloc(Oop::Array(ArrayOop::new("[Ljava/lang/Class;".to_string(), elements)))
}

/// 解析方法描述符的**返回类型**为 Class 镜像(V→void 镜像;否则字段类型→镜像)。供 Method.returnType。
fn return_type_mirror(
    vm: &mut Vm<'_>,
    ret: &crate::metadata::descriptor::ReturnDescriptor,
) -> Reference {
    use crate::metadata::descriptor::ReturnDescriptor;
    match ret {
        ReturnDescriptor::Void => vm.intern_class_mirror("void"),
        ReturnDescriptor::FieldType(ft) => {
            vm.intern_class_mirror(&field_type_to_class_name(ft))
        }
    }
}

/// `Class.getDeclaredMethods0(Z)[Ljava/lang/reflect/Method;` 实现(见分派臂注释)。
fn get_declared_methods0(
    vm: &mut Vm<'_>,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    use crate::constant_pool::ConstantPoolEntry;
    use crate::metadata::access_flags::ACC_PUBLIC;
    use crate::metadata::descriptor::parse_method_descriptor;
    use crate::oops::ArrayOop;

    let Some(this) = this else {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    };
    super::super::clinit::ensure_class_initialized(vm, "java/lang/reflect/AccessibleObject")?;
    let public_only = matches!(args.first(), Some(Value::Int(n)) if *n != 0);

    // 阶段 1:借注册表收 (name,desc,access) + Method 类字段序号。
    let (md, ords): (Vec<(String, String, u16)>, ExecutableOrds) = {
        let internal = vm
            .mirror_internal_name(this)
            .ok_or(VmError::BadConstant("getDeclaredMethods0:非 Class 镜像"))?
            .to_string();
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("getDeclaredMethods0 需注册表"))?;
        let lc = reg
            .get(&internal)
            .ok_or(VmError::BadConstant("getDeclaredMethods0:声明类未加载"))?;
        let md: Vec<(String, String, u16)> = lc
            .cf
            .methods
            .iter()
            .filter(|m| !public_only || m.access_flags.bits() & ACC_PUBLIC != 0)
            .filter_map(|m| {
                let name = match lc.cf.constant_pool.get(m.name_index) {
                    Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
                    _ => return None,
                };
                let desc = match lc.cf.constant_pool.get(m.descriptor_index) {
                    Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
                    _ => return None,
                };
                Some((name, desc, m.access_flags.bits()))
            })
            .collect();
        let m_lc = reg
            .get("java/lang/reflect/Method")
            .ok_or(VmError::BadConstant("java/lang/reflect/Method 未预载"))?;
        let flat = reg.flattened_instance_fields(m_lc);
        let find = |n: &str| {
            flat.iter().position(|f| f.name == n).ok_or(VmError::BadConstant(
                "java/lang/reflect/Method 缺 clazz/slot/name/returnType/parameterTypes/exceptionTypes/modifiers 之一",
            ))
        };
        (
            md,
            ExecutableOrds {
                clazz: find("clazz")?,
                slot: find("slot")?,
                name: find("name")?,
                parameter_types: find("parameterTypes")?,
                exception_types: find("exceptionTypes")?,
                modifiers: find("modifiers")?,
                extra: find("returnType")?,
            },
        )
    };

    // 阶段 2:分配 Method[] + 逐方法 Instance 填字段入数组。
    let arr = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
        "[Ljava/lang/reflect/Method;".to_string(),
        vec![Slot::Reference(Reference::null()); md.len()],
    )));
    for (i, (name, desc, access)) in md.into_iter().enumerate() {
        let parsed = parse_method_descriptor(&desc)
            .map_err(|_| VmError::BadConstant("getDeclaredMethods0:方法描述符解析失败"))?;
        let param_arr = class_array_of(vm, &parsed.parameters);
        let return_mirror = return_type_mirror(vm, &parsed.return_type);
        let exc_arr = class_array_of(vm, &[]);
        let name_ref = super::super::string::intern(vm, &name)?;
        let inst_ref = {
            let reg = vm
                .registry()
                .ok_or(VmError::BadConstant("getDeclaredMethods0 需注册表"))?;
            let m_lc = reg
                .get("java/lang/reflect/Method")
                .ok_or(VmError::BadConstant("java/lang/reflect/Method 未预载"))?;
            vm.heap_mut()
                .alloc(Oop::Instance(reg.new_instance(m_lc)))
        };
        if let Some(Oop::Instance(inst)) = vm.heap_mut().get_mut(inst_ref) {
            inst.set_field(ords.clazz, Slot::Reference(this));
            inst.set_field(ords.slot, Slot::Int(i as i32));
            inst.set_field(ords.name, Slot::Reference(name_ref));
            inst.set_field(ords.extra, Slot::Reference(return_mirror));
            inst.set_field(ords.parameter_types, Slot::Reference(param_arr));
            inst.set_field(ords.exception_types, Slot::Reference(exc_arr));
            inst.set_field(ords.modifiers, Slot::Int(access as i32));
        }
        if let Some(Oop::Array(a)) = vm.heap_mut().get_mut(arr) {
            a.set_element(i, Slot::Reference(inst_ref));
        }
    }
    Ok(Value::Reference(arr))
}

/// `Class.getDeclaredConstructors0(Z)[Ljava/lang/reflect/Constructor;` 实现(见分派臂注释)。
fn get_declared_constructors0(
    vm: &mut Vm<'_>,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    use crate::constant_pool::ConstantPoolEntry;
    use crate::metadata::access_flags::ACC_PUBLIC;
    use crate::metadata::descriptor::parse_method_descriptor;
    use crate::oops::ArrayOop;

    let Some(this) = this else {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    };
    super::super::clinit::ensure_class_initialized(vm, "java/lang/reflect/AccessibleObject")?;
    let public_only = matches!(args.first(), Some(Value::Int(n)) if *n != 0);

    // 阶段 1:仅收 `<init>` 构造器(name=="<init>")的 (desc,access) + Constructor 字段序号。
    let (md, ords): (Vec<(String, u16)>, ExecutableOrds) = {
        let internal = vm
            .mirror_internal_name(this)
            .ok_or(VmError::BadConstant("getDeclaredConstructors0:非 Class 镜像"))?
            .to_string();
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("getDeclaredConstructors0 需注册表"))?;
        let lc = reg
            .get(&internal)
            .ok_or(VmError::BadConstant("getDeclaredConstructors0:声明类未加载"))?;
        let md: Vec<(String, u16)> = lc
            .cf
            .methods
            .iter()
            .filter(|m| !public_only || m.access_flags.bits() & ACC_PUBLIC != 0)
            .filter_map(|m| {
                let name = match lc.cf.constant_pool.get(m.name_index) {
                    Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
                    _ => return None,
                };
                if name != "<init>" {
                    return None;
                }
                let desc = match lc.cf.constant_pool.get(m.descriptor_index) {
                    Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
                    _ => return None,
                };
                Some((desc, m.access_flags.bits()))
            })
            .collect();
        let c_lc = reg
            .get("java/lang/reflect/Constructor")
            .ok_or(VmError::BadConstant("java/lang/reflect/Constructor 未预载"))?;
        let flat = reg.flattened_instance_fields(c_lc);
        let find = |n: &str| {
            flat.iter().position(|f| f.name == n).ok_or(VmError::BadConstant(
                "java/lang/reflect/Constructor 缺 clazz/slot/parameterTypes/exceptionTypes/modifiers 之一",
            ))
        };
        (
            md,
            ExecutableOrds {
                clazz: find("clazz")?,
                slot: find("slot")?,
                parameter_types: find("parameterTypes")?,
                exception_types: find("exceptionTypes")?,
                modifiers: find("modifiers")?,
                name: 0,
                extra: 0,
            },
        )
    };

    // 阶段 2:分配 Constructor[] + 逐构造器 Instance 填字段入数组。
    let arr = vm.heap_mut().alloc(Oop::Array(ArrayOop::new(
        "[Ljava/lang/reflect/Constructor;".to_string(),
        vec![Slot::Reference(Reference::null()); md.len()],
    )));
    for (i, (desc, access)) in md.into_iter().enumerate() {
        let parsed = parse_method_descriptor(&desc)
            .map_err(|_| VmError::BadConstant("getDeclaredConstructors0:方法描述符解析失败"))?;
        let param_arr = class_array_of(vm, &parsed.parameters);
        let exc_arr = class_array_of(vm, &[]);
        let inst_ref = {
            let reg = vm
                .registry()
                .ok_or(VmError::BadConstant("getDeclaredConstructors0 需注册表"))?;
            let c_lc = reg
                .get("java/lang/reflect/Constructor")
                .ok_or(VmError::BadConstant("java/lang/reflect/Constructor 未预载"))?;
            vm.heap_mut()
                .alloc(Oop::Instance(reg.new_instance(c_lc)))
        };
        if let Some(Oop::Instance(inst)) = vm.heap_mut().get_mut(inst_ref) {
            inst.set_field(ords.clazz, Slot::Reference(this));
            inst.set_field(ords.slot, Slot::Int(i as i32));
            inst.set_field(ords.parameter_types, Slot::Reference(param_arr));
            inst.set_field(ords.exception_types, Slot::Reference(exc_arr));
            inst.set_field(ords.modifiers, Slot::Int(access as i32));
        }
        if let Some(Oop::Array(a)) = vm.heap_mut().get_mut(arr) {
            a.set_element(i, Slot::Reference(inst_ref));
        }
    }
    Ok(Value::Reference(arr))
}

/// `java/lang/reflect/Method`/`Constructor` 共有字段序号;`extra` = Method 的 `returnType`
/// (Constructor 不用,name/extra 置 0 占位)。`name` 仅 Method 用(Constructor 无 name 字段)。
struct ExecutableOrds {
    clazz: usize,
    slot: usize,
    name: usize,
    parameter_types: usize,
    exception_types: usize,
    modifiers: usize,
    extra: usize,
}

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::class_loader::class_path::ClassPath;
    use crate::runtime::class_loader::loader::load_closure;
    use crate::runtime::{Reference, Slot, Value, Vm, VmError};

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

    /// 取实例 `referent` 字段序号(声明于 Reference;子类扁平布局同序)。
    fn referent_ord(vm: &Vm<'_>, r: Reference) -> Option<usize> {
        let cn = match vm.heap().get(r)? {
            crate::oops::Oop::Instance(i) => i.class_name().to_string(),
            _ => return None,
        };
        vm.registry().and_then(|reg| {
            reg.get(&cn).and_then(|lc| {
                reg.flattened_instance_fields(lc)
                    .iter()
                    .position(|f| f.name == "referent")
            })
        })
    }

    /// **RED→GREEN**:`Reference.refersTo0(Object)Z` = `this.referent` 与 `o` 引用身份比较
    ///(JVM_ReferenceRefersTo;Reference.java:373)。referent 由 `Reference.<init>(T)` 置
    ///(Reference.java:532,普通实例字段——rustj 无 GC,不特殊处理)。
    #[test]
    fn refers_to0_compares_referent_identity() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/ref/Reference").unwrap();
        load_closure(&mut registry, &cp, "java/lang/Object").unwrap();

        let mut vm = Vm::new(&registry);
        // 两个不同 Object 实例 A、B(身份不同)。§6:'a 不绑 &self → inst 先算出(owned),
        // 再 heap_mut().alloc,免 &mut vm 与 &vm 并发。
        let new_obj = |vm: &mut Vm<'_>| -> Reference {
            let reg = vm.registry().expect("须有注册表");
            let lc = reg.get("java/lang/Object").expect("Object 须已加载");
            let inst = reg.new_instance(lc);
            vm.heap_mut().alloc(crate::oops::Oop::Instance(inst))
        };
        let a = new_obj(&mut vm);
        let b = new_obj(&mut vm);
        // Reference 实例,直置 referent=A(等价 Reference.<init>(A) 的字段写)。
        let r = {
            let reg = vm.registry().expect("须有注册表");
            let lc = reg
                .get("java/lang/ref/Reference")
                .expect("Reference 须已加载");
            let inst = reg.new_instance(lc);
            vm.heap_mut().alloc(crate::oops::Oop::Instance(inst))
        };
        let ord = referent_ord(&vm, r).expect("Reference 须有 referent 字段");
        if let Some(crate::oops::Oop::Instance(i)) = vm.heap_mut().get_mut(r) {
            i.set_field(ord, Slot::Reference(a));
        }

        // refersTo0(A) → true(referent 身份 == A)。
        let yes = super::super::invoke(
            &mut vm,
            "java/lang/ref/Reference",
            "refersTo0",
            "(Ljava/lang/Object;)Z",
            Some(r),
            &[Value::Reference(a)],
        )
        .expect("refersTo0(A) 应返布尔,非抛");
        assert_eq!(yes, Value::Int(1), "referent==A 时 refersTo0 须返 true");
        // refersTo0(B) → false(B 与 referent A 身份不同)。
        let no = super::super::invoke(
            &mut vm,
            "java/lang/ref/Reference",
            "refersTo0",
            "(Ljava/lang/Object;)Z",
            Some(r),
            &[Value::Reference(b)],
        )
        .expect("refersTo0(B) 应返布尔,非抛");
        assert_eq!(no, Value::Int(0), "referent=A 时 refersTo0(B) 须返 false");
    }

    /// 收尾:未登记的 Reference native 仍抛 ULE。
    #[test]
    fn unbound_reference_native_throws_ule() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(&registry);
        let err = super::super::invoke(
            &mut vm,
            "java/lang/ref/Reference",
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
}
