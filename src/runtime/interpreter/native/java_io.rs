//! `java/io/*` 的 native 桥。语义移植自 `src/java.base/{windows,unix}/native/libjava/` 的
//! `Java_java_io_*` 桥 + `prims/jvm.cpp` 的 `JVM_*`。由 [`super::dispatch`] 按声明类路由至此
//! (`java/io/` 前缀;4.25 起新增,此前 `native/mod.rs::dispatch` 仅路由 `java/lang/`)。
//!
//! **`WinNTFileSystem.initIDs()V`**(WinNTFileSystem.java:632 `private static native`,
//! `<clinit>:634` 首调):HotSpot `Java_java_io_WinNTFileSystem_initIDs`(`WinNTFileSystem_md.c`)
//! 仅缓存字段 ID(`File.path` 等)供后续 native 用,**无文件系统访问** → rustj 空操作。解锁
//! `File.<clinit>:160` → `DefaultFileSystem.getFileSystem:40` → `new WinNTFileSystem()` 链。
//!
//! **`WinNTFileSystem.canonicalize0(String)String`**(WinNTFileSystem.java:488 `private native`,
//! throws IOException;4.27):HotSpot `Java_java_io_WinNTFileSystem_canonicalize0` 委派
//! `wcanonicalize`(`canonicalize_md.c:204`,Windows 经 `GetFullPathName` 规范化 + currentDir
//! 前置)。rustj 委派 `std::fs::canonicalize`(同 `availableProcessors`→`std::thread` 思路),
//! 剥 `\\?\` verbatim 前缀(Java canonicalize0 返普通路径)。解锁 `File.getCanonicalPath` →
//! `URLClassPath.toFileURL`(`ClassLoaders.<clinit>` 链)。

use crate::runtime::{Reference, Value, Vm, VmError};

use super::super::throw_exception;

/// `java/io/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // WinNTFileSystem.initIDs()V —— <clinit> 缓存字段 ID;无 FS 访问 → 空操作返 void。
        ("java/io/WinNTFileSystem", "initIDs", "()V") => Ok(Value::Void),
        // WinNTFileSystem.canonicalize0(Ljava/lang/String;)Ljava/lang/String; —— 路径规范化(std::fs::canonicalize)。
        ("java/io/WinNTFileSystem", "canonicalize0", "(Ljava/lang/String;)Ljava/lang/String;") => {
            canonicalize0(vm, args)
        }
        // WinNTFileSystem.getFinalPath0(Ljava/lang/String;)Ljava/lang/String; —— canonicalize 已全解析
        // (std::fs::canonicalize 含符号链接→最终目标),此处的 reparse 再解析冗余 → 恒等返原输入。
        ("java/io/WinNTFileSystem", "getFinalPath0", "(Ljava/lang/String;)Ljava/lang/String;") => {
            get_final_path_0(vm, args)
        }
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

/// `WinNTFileSystem.canonicalize0(String path)String`(throws IOException):把路径解析为规范绝对
/// 形式(消除 `.`/`..`、相对→绝对、解析符号链接)。委派 `std::fs::canonicalize`,剥 `\\?\` 前缀。
///
/// **`path=""`**:`new File("")` = 当前目录;`File.getCanonicalPath` 先 `resolve`(用 `user.dir`)→
/// 绝对 cwd 路径再传入,故本处通常收绝对路径。但 HotSpot `wcanonicalize` 对空串亦经
/// `currentDirLength` 前置 currentDir,故空串时退化为 `.`(canonicalize cwd)以匹配。
///
/// **失败**:`std::fs::canonicalize` 要求路径存在;不存在 / 无权限 → `IOException`(对应
/// `Java_java_io_..._canonicalize0` 失败时 `JNU_ThrowIOExceptionWithLastError`)。
fn canonicalize0(vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    // 取 path 实参(Java String → Rust String)。非 Reference / null → NPE(JNI 解引用 jstring)。
    let path = match args.first().copied() {
        Some(Value::Reference(r)) if !r.is_null() => {
            match crate::runtime::interpreter::string::read_text(vm, r)? {
                Some(s) => s,
                None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
            }
        }
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // 空串 → 当前目录(匹配 HotSpot currentDirLength 前置)。
    let target = if path.is_empty() { ".".to_string() } else { path };
    match std::fs::canonicalize(&target) {
        Ok(p) => {
            // std::fs::canonicalize 在 Windows 返 `\\?\C:\...` / `\\?\UNC\server\share` verbatim 前缀;
            // Java canonicalize0 返普通路径(C:\... / \\server\share)。剥前缀以匹配(否则 file: URL 畸形)。
            let s = strip_verbatim_prefix(&p.display().to_string());
            let r = crate::runtime::interpreter::string::intern(vm, &s)?;
            Ok(Value::Reference(r))
        }
        Err(_) => Err(throw_exception(vm, "java/io/IOException")),
    }
}

/// 剥 Windows verbatim 前缀:`\\?\C:\…` → `C:\…`、`\\?\UNC\server\share\…` → `\\server\share\…`。
/// 非 Windows / 无前缀 → 原样。匹配 Java `WinNTFileSystem.canonicalize0` 的普通路径输出。
fn strip_verbatim_prefix(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// `WinNTFileSystem.getFinalPath0(String)String`(throws IOException):解析 reparse 点 / 符号链接的
/// 最终目标(HotSpot `GetFinalPathNameByHandle`)。`canonicalize`(jdk-25.0.2 wrapper)在 `canonicalize0`
/// 后无条件调之,IOException → 回退 `canonicalize0` 结果。rustj 的 `canonicalize0` 已用
/// `std::fs::canonicalize` 全解析(含符号链接 → 最终目标),故此处再解析冗余 → **恒等返原输入**。
fn get_final_path_0(_vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    Ok(match args.first().copied() {
        Some(v @ Value::Reference(_)) => v,
        _ => Value::Reference(Reference::null()),
    })
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

    /// **`strip_verbatim_prefix` 纯逻辑**(Layer 4.27):剥 Windows `\\?\` / `\\?\UNC\` 前缀。
    #[test]
    fn strip_verbatim_prefix_handles_windows_forms() {
        assert_eq!(super::strip_verbatim_prefix(r"\\?\C:\foo\bar"), r"C:\foo\bar");
        assert_eq!(
            super::strip_verbatim_prefix(r"\\?\UNC\server\share\dir"),
            r"\\server\share\dir"
        );
        // 无前缀 / Unix → 原样。
        assert_eq!(super::strip_verbatim_prefix(r"C:\foo"), r"C:\foo");
        assert_eq!(super::strip_verbatim_prefix("/usr/local/bin"), "/usr/local/bin");
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
