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
    match (class, name, desc) {
        // Throwable.fillInStackTrace(I)Ljava/lang/Throwable; —— 每个 Throwable 构造器首调
        // (捕获栈回溯)。rustj 暂无栈回溯捕获机制 → 空操作,返回 this(对应"无栈帧记录")。
        // 保留 `this` 不变;HotSpot 此法返回 this 以便链式。
        ("java/lang/Throwable", "fillInStackTrace", "(I)Ljava/lang/Throwable;") => {
            match this {
                Some(r) => Ok(Value::Reference(r)),
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

        // Object.hashCode()I —— synchronizer.cpp get_next_hash mode 4(对象标识/地址)。
        // 句柄 id 即堆上唯一标识;null 收者(理论不可达,实例方法)兜底 0。
        ("java/lang/Object", "hashCode", "()I") => {
            let id = this.and_then(Reference::id).unwrap_or(0) as i32;
            Ok(Value::Int(id))
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
            let text = match vm.heap().get(r) {
                Some(Oop::String(s)) => s.text().to_string(),
                _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
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

        // 未登记 → UnsatisfiedLinkError(nativeLookup.cpp 解析失败的对应物)。
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

    #[test]
    fn get_primitive_class_returns_class_oop() {
        // jvm.cpp:770 JVM_FindPrimitiveClass:"int" → Class 镜像。
        let mut vm = crate::runtime::Vm::default();
        let s = vm.heap_mut().alloc(crate::oops::Oop::String(
            crate::oops::StringOop::new("int".into()),
        ));
        let r = invoke(
            &mut vm,
            "java/lang/Class",
            "getPrimitiveClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            None,
            &[Value::Reference(s)],
        )
        .unwrap();
        let Value::Reference(cls) = r else {
            panic!("期望 Class 引用,得 {r:?}");
        };
        let crate::oops::Oop::Class(c) = vm.heap().get(cls).unwrap() else {
            panic!("期望 Oop::Class");
        };
        assert_eq!(c.name(), "int");
    }

    #[test]
    fn get_primitive_class_non_primitive_throws_class_not_found() {
        // 非原语名 → ClassNotFoundException(jvm.cpp 的 THROW_MSG_NULL)。
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::Vm::new(&reg);
        let s = vm.heap_mut().alloc(crate::oops::Oop::String(
            crate::oops::StringOop::new("java/lang/Object".into()),
        ));
        let err = invoke(
            &mut vm,
            "java/lang/Class",
            "getPrimitiveClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            None,
            &[Value::Reference(s)],
        )
        .unwrap_err();
        let crate::runtime::VmError::ThrownException(exc) = err else {
            panic!("应抛 ThrownException,得 {err:?}");
        };
        let crate::oops::Oop::Instance(i) = vm.heap().get(exc).unwrap() else {
            panic!("须为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/ClassNotFoundException");
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
