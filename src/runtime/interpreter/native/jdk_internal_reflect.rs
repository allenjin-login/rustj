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

use crate::runtime::{Reference, Value, Vm, VmError};

use super::super::throw_exception;

/// `jdk/internal/reflect/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm,
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
fn get_class_access_flags(vm: &mut Vm, args: &[Value]) -> Result<i32, VmError> {
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

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::class_loader::class_path::ClassPath;
    use crate::runtime::class_loader::loader::load_closure;
    use crate::runtime::{Reference, Value, Vm, VmError};

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

        let mut vm = Vm::new(registry);
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

        let mut vm = Vm::new(registry);
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

        let mut vm = Vm::new(registry);
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

        let mut vm = Vm::new(registry);
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

        let mut vm = Vm::new(registry);
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
        let mut vm = Vm::new(registry);
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
        let mut vm = Vm::new(registry);
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
}
