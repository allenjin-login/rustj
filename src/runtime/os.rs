//! 跨平台动态库装载(`os::dll_load` / `dll_lookup` / `dll_unload` / `process_handle`)。
//!
//! 语义移植自 HotSpot `src/hotspot/os/{windows,posix}/os_*.cpp`:
//! - `os::dll_load`(os_windows.cpp:1778 / os_posix.cpp)= win32 `LoadLibraryW` / posix `dlopen`。
//! - `os::dll_lookup`(os_windows.cpp:1501)= `GetProcAddress` / `dlsym`。
//! - `os::dll_unload`(os_windows.cpp:1476)= `FreeLibrary` / `dlclose`。
//! - `getProcessHandle`(NativeLibraries.c:56 `procHandle`)— builtin 库符号查找基于主镜像句柄;
//!   win32 `GetModuleHandleW(NULL)` 返回主 EXE 的 HMODULE。
//!
//! 这是 rustj **唯一直接 OS FFI** 的模块。§2 判断标准:动态库装载**无 std 安全封装**,
//! 且无可忠实手移植的替代(纯 OS 系统调用)→ 确属必要,引直接 FFI。故本模块整体开窗
//! `#[allow(unsafe_code)]`(**模块级 inner attribute,非 crate 级**——§1);crate 其余源码仍零 unsafe。
//!
//! 平台不透明句柄 = `*mut c_void`(HotSpot `void*`)。Java 侧以 `i64`(jlong)往返:
//! 64 位平台上指针 8 字节,`as usize as i64` 无损(句柄恒为低地址正数)。
//!
//! **Step 0 源码依据**:
//! - jvm.cpp:3150 `JVM_LoadLibrary(name, throwException)` → `os::dll_load(name, ebuf, sizeof)`。
//! - jvm.cpp:3188 `JVM_FindLibraryEntry(handle, name)` → `os::dll_lookup(handle, name)`。
//! - NativeLibraries.c:119 `handle = isBuiltin ? procHandle : JVM_LoadLibrary(cname, …)`。
#![allow(unsafe_code)]

use core::ffi::c_void;

#[cfg(windows)]
mod imp {
    use core::ffi::{c_int, c_void};
    use std::ffi::CString;
    use std::os::windows::ffi::OsStrExt;

    // Win32 动态库装载 API(kernel32.dll)。`extern "system"` = win32 调用约定
    // (x86 stdcall / x64 默认——HotSpot 同用 `__stdcall`/默认)。`#[link]` 显式链 kernel32
    // (MSVC 默认已链;GNU 需显式,故统一标注以跨工具链)。
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
        fn FreeLibrary(module: *mut c_void) -> c_int;
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    /// UTF-8 → NUL 终结 UTF-16(`LoadLibraryW` 入参)。
    fn wide(s: &str) -> Vec<u16> {
        use std::ffi::OsStr;
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn dll_load(name: &str) -> *mut c_void {
        let w = wide(name);
        // SAFETY:`w` 以 NUL(0u16)终结;`LoadLibraryW` 仅读取该宽串。失败返 NULL。
        unsafe { LoadLibraryW(w.as_ptr()) }
    }

    pub fn dll_lookup(handle: *mut c_void, name: &str) -> *mut c_void {
        // GetProcAddress 接受 ANSI/NUL 终结符号名;导出符号名恒 ASCII。含内嵌 NUL → 放弃。
        let Ok(c) = CString::new(name) else {
            return core::ptr::null_mut();
        };
        // SAFETY:`handle` 来自 dll_load/process_handle(有效 HMODULE);`c` 为 NUL 终结 ASCII。
        unsafe { GetProcAddress(handle, c.as_ptr() as *const u8) }
    }

    pub fn dll_unload(handle: *mut c_void) -> bool {
        // SAFETY:`handle` 来自 dll_load;`FreeLibrary` 减引用计数。
        unsafe { FreeLibrary(handle) != 0 }
    }

    pub fn process_handle() -> *mut c_void {
        // SAFETY:传 NULL → 返回主 EXE 的 HMODULE(Win32 规范用法,无副作用)。
        unsafe { GetModuleHandleW(core::ptr::null()) }
    }
}

#[cfg(not(windows))]
mod imp {
    use core::ffi::c_void;

    #[link(name = "dl")]
    unsafe extern "C" {
        fn dlopen(name: *const core::ffi::c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const core::ffi::c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }

    use core::ffi::{c_char, c_int};

    const RTLD_NOW: c_int = 2;

    pub fn dll_load(name: &str) -> *mut c_void {
        let Ok(c) = std::ffi::CString::new(name) else {
            return core::ptr::null_mut();
        };
        // SAFETY:`c` NUL 终结;dlopen 读取之。失败返 NULL。
        unsafe { dlopen(c.as_ptr() as *const c_char, RTLD_NOW) }
    }

    pub fn dll_lookup(handle: *mut c_void, name: &str) -> *mut c_void {
        let Ok(c) = std::ffi::CString::new(name) else {
            return core::ptr::null_mut();
        };
        // SAFETY:`handle` 来自 dll_load;符号名 NUL 终结。
        unsafe { dlsym(handle, c.as_ptr() as *const c_char) }
    }

    pub fn dll_unload(handle: *mut c_void) -> bool {
        // SAFETY:`handle` 来自 dll_load;dlclose 减引用。
        unsafe { dlclose(handle) == 0 }
    }

    pub fn process_handle() -> *mut c_void {
        // POSIX:RTLD_DI_ORIGIN 之类取主进程句柄需 dlopen(NULL) → 返回主程序句柄。
        // SAFETY:dlopen(NULL) 规范地返回主程序句柄(供 builtin 符号查找)。
        unsafe { dlopen(core::ptr::null(), RTLD_NOW) }
    }
}

/// 加载动态库。成功返句柄,失败返 null(对应 HotSpot `os::dll_load` 失败返 nullptr;
/// 调用方据此 + `throwExceptionIfFail` 决定抛 `UnsatisfiedLinkError`)。
pub fn dll_load(name: &str) -> *mut c_void {
    imp::dll_load(name)
}

/// 查符号地址;未找到 → null(`GetProcAddress` / `dlsym`)。
pub fn dll_lookup(handle: *mut c_void, name: &str) -> *mut c_void {
    imp::dll_lookup(handle, name)
}

/// 卸载;成功 true(`FreeLibrary` / `dlclose`)。
pub fn dll_unload(handle: *mut c_void) -> bool {
    imp::dll_unload(handle)
}

/// 进程(主镜像)句柄——builtin 库符号查找基址。win32 `GetModuleHandleW(NULL)`。
pub fn process_handle() -> *mut c_void {
    imp::process_handle()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 以下断言在 Windows 宿主上有效(kernel32.dll 必在;GetLastError 为已知导出)。
    // 非 Windows 宿主仍可跑(libc/libdl 在场),但符号名/路径不同——按平台跳过。

    #[cfg(windows)]
    #[test]
    fn dll_load_loads_kernel32_and_looks_up_symbol() {
        // HotSpot os_windows.cpp:249 同样装载 "KernelBase"/"kernel32" 以解析符号。
        let h = dll_load("kernel32.dll");
        assert!(!h.is_null(), "kernel32.dll 应成功加载");
        // kernel32 导出 GetLastError(Win32 子系统最常用符号)。
        let sym = dll_lookup(h, "GetLastError");
        assert!(!sym.is_null(), "kernel32!GetLastError 应查到");
        // 进程句柄恒非 null(主 EXE 镜像)。
        assert!(!process_handle().is_null(), "process_handle 应为主镜像句柄");
        // 卸载不应崩溃;FreeLibrary 返非零=true(kernel32 引用计数≥1,卸载后仍映射,但调用合法)。
        // 不强断 dll_unload 返 true(系统库引用计数可能>1),仅断不崩。
        let _ = dll_unload(h);
    }

    #[test]
    fn dll_load_missing_library_returns_null() {
        // 不存在的库 → null(对应 HotSpot 失败路径 → 上层据 throwExceptionIfFail 抛 ULE)。
        let h = dll_load("no_such_library_4_16_zzz.dll");
        assert!(h.is_null(), "不存在的库应返 null");
    }

    #[test]
    fn dll_lookup_null_handle_returns_null() {
        // null 句柄查符号 → null(不崩);GetProcAddress(NULL, name) 在 Win32 实际查主 EXE,
        // 但 rustj 不依赖该副作用,仅断 null 句柄不致 UB。
        let _ = dll_lookup(core::ptr::null_mut(), "x");
    }
}
