//! `java/io/*` 的 native 桥。语义移植自 `src/java.base/{windows,unix}/native/libjava/` 的
//! `Java_java_io_*` 桥 + `prims/jvm.cpp` 的 `JVM_*`。由 [`super::dispatch`] 按声明类路由至此
//!(`java/io/` 前缀;4.25 起新增,此前 `native/mod.rs::dispatch` 仅路由 `java/lang/`)。
//!
//! **`WinNTFileSystem.initIDs()V`**(WinNTFileSystem.java:632 `private static native`,
//! `<clinit>:634` 首调):HotSpot `Java_java_io_WinNTFileSystem_initIDs`(`WinNTFileSystem_md.c`)
//! 仅缓存字段 ID(`File.path` 等)供后续 native 用,**无文件系统访问** → rustj 空操作。
//! 解锁 `File.<clinit>:160` → `DefaultFileSystem.getFileSystem:40` → `new WinNTFileSystem()` 链。
//! **后续**:构造器读 `System.getProperty("file.separator"/"path.separator"/"user.dir")`,
//! 空 props 将 NPE → 须在 Phase 1 `initialize_system_class` 补真 launcher 系统属性(顺延)。

use crate::runtime::{Reference, Value, Vm, VmError};

use super::super::throw_exception;

/// `java/io/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    _args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // WinNTFileSystem.initIDs()V —— <clinit> 缓存字段 ID;无 FS 访问 → 空操作返 void。
        ("java/io/WinNTFileSystem", "initIDs", "()V") => Ok(Value::Void),
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::{Value, Vm};

    /// **RED→GREEN**(Layer 4.25):`WinNTFileSystem.initIDs()V` native 空操作返 void。
    /// HotSpot `Java_java_io_WinNTFileSystem_initIDs` 仅缓存字段 ID,无 FS 访问。
    /// 验证 `java/io/` 路由 + arm 就位(修前 `java/io/*` 落 `_ => ULE`)。
    #[test]
    fn init_ids_returns_void() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(&registry);
        let r = super::super::invoke(
            &mut vm,
            "java/io/WinNTFileSystem",
            "initIDs",
            "()V",
            None,
            &[],
        )
        .expect("initIDs 应返 void,非抛异常");
        assert!(matches!(r, Value::Void), "须返 void,得 {r:?}");
    }

    /// 收尾:确使未登记路径仍抛 ULE(防 dispatch 误吞)。
    #[test]
    fn unbound_java_io_native_throws_ule() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(&registry);
        let err = super::super::invoke(
            &mut vm,
            "java/io/WinNTFileSystem",
            "unknownNative",
            "()V",
            None,
            &[],
        )
        .unwrap_err();
        match err {
            crate::runtime::VmError::ThrownException(r) => match vm.heap().get(r) {
                Some(crate::oops::Oop::Instance(i)) => {
                    assert_eq!(i.class_name(), "java/lang/UnsatisfiedLinkError")
                }
                o => panic!("须 Instance,得 {o:?}"),
            },
            e => panic!("须 ThrownException,得 {e:?}"),
        }
    }
}
