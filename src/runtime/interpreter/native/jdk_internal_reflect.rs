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
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    _args: &[Value],
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
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::class_loader::class_path::ClassPath;
    use crate::runtime::class_loader::loader::load_closure;
    use crate::runtime::{Value, Vm, VmError};

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

        let mut vm = Vm::new(&registry);
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
            vm.mirror_internal_name(mirror),
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

        let mut vm = Vm::new(&registry);
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

    /// 收尾:确使未登记路径仍抛 ULE(防 dispatch 误吞)。
    #[test]
    fn unbound_reflection_native_throws_ule() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(&registry);
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
