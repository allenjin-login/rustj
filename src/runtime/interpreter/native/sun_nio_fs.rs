//! `sun/nio/fs/*` 的 native 桥。语义移植自 `src/java.base/windows/native/libnio/fs/`
//! 的 `Java_sun_nio_fs_*` 桥(底层 Win32:CreateFile/ReadFile/…)。由 [`super::dispatch`] 按
//! 声明类路由至此(`sun/nio/fs/` 前缀;4.39 起新增)。
//!
//! **设计(同 java/io 决策)**:`sun/nio/fs/WindowsNativeDispatcher` 的 native 是 **JNI native**
//! (取 `JNIEnv*`,回调 `GetFieldID`/`GetStringChars` 等),与 HotSpot ABI/JNIEnv 绑死,**不可
//! 经 dll_load 复用真 JDK 的 net.dll/nio.dll**(CLAUDE.md §3)。rustj 移植 = 重写语义,OS 系统
//! 调用部分委派 `std::fs`(同 `availableProcessors`→`std::thread`、`canonicalize0`→
//! `std::fs::canonicalize`)。
//!
//! **`WindowsNativeDispatcher.initIDs()V`**(`private static native`,本机 jmod jdk-25.0.2
//! `<clinit>:1100` 调用;jdk-master 源码已重构为 `init()I` 但本机 jmod 仍为 `initIDs`——
//! 版本错位,以 jmod 实测为准,memory `jdk-master-source-vs-jmod-version-mismatch.md`):
//! HotSpot 历史 `Java_sun_nio_fs_WindowsNativeDispatcher_initIDs` 仅缓存 field ID
//! (`WindowsException`/`NativeBuffer`/`FirstFile`/… 供后续 native 用),**无 FS 访问、无 Win32
//! 调用** → rustj 空操作返 void(同 `WinNTFileSystem.initIDs` 4.25、`FileDescriptor.initIDs`
//! 4.35)。解锁 `WindowsNativeDispatcher.<clinit>` 完整跑通(本版本 `<clinit>` 仅 `initIDs()`,
//! 无 `capabilities = init()` 赋值)。

use crate::runtime::{Reference, Value, Vm, VmError};

use super::super::throw_exception;

/// `sun/nio/fs/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    _args: &[Value],
) -> Result<Value, VmError> {
    let _ = _this;
    let _ = _args;
    match (class, name, desc) {
        // WindowsNativeDispatcher.initIDs()V —— 本机 jmod <clinit>:1100 调用(jdk-master 源码
        // 已重构为 init()I,本机 jmod 仍为 initIDs——版本错位以 jmod 为准)。HotSpot 历史语义
        // 仅缓存 field ID(WindowsException/NativeBuffer/FirstFile/…),无 FS/Win32 访问 → 空操作
        // 返 void(同 WinNTFileSystem.initIDs 4.25、FileDescriptor.initIDs 4.35)。解锁
        // WindowsNativeDispatcher.<clinit> 完整跑通。
        ("sun/nio/fs/WindowsNativeDispatcher", "initIDs", "()V") => Ok(Value::Void),

        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::{Value, Vm};

    /// **RED→GREEN**(Layer 4.39):`WindowsNativeDispatcher.initIDs()V` native 空操作返 void。
    /// 本机 jmod `<clinit>:1100` 调用(经 `BootLoader.loadLibrary("net"/"nio")` 后);HotSpot
    /// 历史语义仅缓存 field ID,无 FS/Win32 访问 → 空操作。验证 `sun/nio/fs/` 路由 + arm 就位
    /// (修前落 `_ => ULE`)。
    #[test]
    fn windows_native_dispatcher_init_ids_returns_void() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(&registry);
        let r = super::super::invoke(
            &mut vm,
            "sun/nio/fs/WindowsNativeDispatcher",
            "initIDs",
            "()V",
            None,
            &[],
        )
        .expect("initIDs 应返 void,非抛异常");
        assert!(matches!(r, Value::Void), "须返 void,得 {r:?}");
    }
}
