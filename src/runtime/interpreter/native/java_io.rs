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

use crate::runtime::{Reference, Value, VmThread, VmError};

use super::super::throw_exception;

/// `java/io/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // WinNTFileSystem.initIDs()V —— <clinit> 缓存字段 ID;无 FS 访问 → 空操作返 void。
        ("java/io/WinNTFileSystem", "initIDs", "()V") => Ok(Value::Void),
        // FileDescriptor.initIDs()V —— FileDescriptor.java:65 <clinit> 首调。HotSpot
        // Java_java_io_FileDescriptor_initIDs 仅缓存字段 ID(IO_fd/handle/append),无 FS 访问 → 空操作。
        ("java/io/FileDescriptor", "initIDs", "()V") => Ok(Value::Void),
        // FileDescriptor.getHandle(I)J —— FileDescriptor.java:227 private static native。私有
        // FileDescriptor(int fd)<init>:129 调用(std 流 in=0/out=1/err=2)。HotSpot Windows 用
        // GetStdHandle 取 OS HANDLE;rustj 不接 OS stdio handle → 返 fd 作 placeholder(0/1/2,
        // 非 -1 即非 invalid handle,使 <clinit>:151 的 in/out/err 构造完成)。
        ("java/io/FileDescriptor", "getHandle", "(I)J") => {
            let fd = match args.first().copied() {
                Some(Value::Int(n)) => n as i64,
                _ => -1,
            };
            Ok(Value::Long(fd))
        }
        // FileDescriptor.getAppend(I)Z —— FileDescriptor.java:232 private static native。std 流
        // 非 append → 返 false。HotSpot 读 fd 的 O_APPEND 标志;rustj std 流恒非 append。
        ("java/io/FileDescriptor", "getAppend", "(I)Z") => Ok(Value::Int(0)),
        // WinNTFileSystem.canonicalize0(Ljava/lang/String;)Ljava/lang/String; —— 路径规范化(std::fs::canonicalize)。
        ("java/io/WinNTFileSystem", "canonicalize0", "(Ljava/lang/String;)Ljava/lang/String;") => {
            canonicalize0(vm, args)
        }
        // WinNTFileSystem.getFinalPath0(Ljava/lang/String;)Ljava/lang/String; —— canonicalize 已全解析
        // (std::fs::canonicalize 含符号链接→最终目标),此处的 reparse 再解析冗余 → 恒等返原输入。
        ("java/io/WinNTFileSystem", "getFinalPath0", "(Ljava/lang/String;)Ljava/lang/String;") => {
            get_final_path_0(vm, args)
        }
        // WinNTFileSystem.getBooleanAttributes0(Ljava/io/File;)I —— 文件属性位掩码(std::fs::metadata)。
        ("java/io/WinNTFileSystem", "getBooleanAttributes0", "(Ljava/io/File;)I") => {
            get_boolean_attributes_0(vm, args)
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
fn canonicalize0(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
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
fn get_final_path_0(_vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    Ok(match args.first().copied() {
        Some(v @ Value::Reference(_)) => v,
        _ => Value::Reference(Reference::null()),
    })
}

/// `WinNTFileSystem.getBooleanAttributes0(File)I`(WinNTFileSystem_md.c:356):读 File 实例 `path` 字段 →
/// `std::fs::metadata`(=HotSpot `getFinalAttributes`/`GetFileAttributesEx`)→ 返位掩码:
/// `BA_EXISTS=0x01`(存在)/`BA_REGULAR=0x02`(普通文件)/`BA_DIRECTORY=0x04`(目录)/`BA_HIDDEN=0x08`;
/// 不存在(Err,=INVALID_FILE_ATTRIBUTES)→ 0。File 类名常量 `FileSystem.java:123-126`。
fn get_boolean_attributes_0(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    const BA_EXISTS: i32 = 0x01;
    const BA_REGULAR: i32 = 0x02;
    const BA_DIRECTORY: i32 = 0x04;
    const BA_HIDDEN: i32 = 0x08;
    // 取 File 实参;null / 非 File → 返 0(C 层 fileToNTPath 返 NULL → rv=0)。
    let Some(Value::Reference(file_ref)) = args.first().copied() else {
        return Ok(Value::Int(0));
    };
    if file_ref.is_null() {
        return Ok(Value::Int(0));
    }
    let path = match file_path_text(vm, file_ref)? {
        Some(p) => p,
        None => return Ok(Value::Int(0)),
    };
    // getFinalAttributes(path):INVALID → rv=0;否则 EXIST + (DIR ? DIRECTORY : REGULAR) + HIDDEN。
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(Value::Int(0));
    };
    let mut rv = BA_EXISTS;
    rv |= if meta.is_dir() { BA_DIRECTORY } else { BA_REGULAR };
    if is_hidden(&meta, &path) {
        rv |= BA_HIDDEN;
    }
    Ok(Value::Int(rv))
}

/// 读 `java/io/File` 实例 `path` 字段(String)的文本。非 File 实例 / 字段缺失 / null path → `None`。
/// 沿用 `install_system_props` 的 `flattened_instance_fields().position(name)` 模式定位字段全局序号。
fn file_path_text(vm: &VmThread, file_ref: Reference) -> Result<Option<String>, VmError> {
    use crate::oops::Oop;
    use crate::runtime::Slot;

    // inst 取 owned(clone):其后读 path String 须再锁 heap,持 guard 重锁会自死锁(B.2.3b)。
    let inst = match vm.heap().get(file_ref).cloned() {
        Some(Oop::Instance(i)) if i.class_name() == "java/io/File" => i,
        _ => return Ok(None),
    };
    let Some(reg) = vm.registry() else {
        return Err(VmError::BadConstant("file_path_text 需类注册表"));
    };
    let Some(lc) = reg.get("java/io/File") else {
        return Err(VmError::BadConstant("file_path_text:java/io/File 须预载"));
    };
    let Some(ord) = reg
        .flattened_instance_fields(&lc)
        .iter()
        .position(|f| f.name == "path")
 else {
        return Ok(None);
    };
    let path_ref = match inst.field(ord) {
        Slot::Reference(r) => r,
        _ => return Ok(None),
    };
    if path_ref.is_null() {
        return Ok(None);
    }
    crate::runtime::interpreter::string::read_text(vm, path_ref)
}

/// 文件是否隐藏(`BA_HIDDEN`)。Windows:FILE_ATTRIBUTE_HIDDEN 位(`std::os::windows::fs::MetadataExt`,
/// 安全 trait,`#![deny(unsafe_code)]` 不约束);Unix:文件名首字符 `.`。两参均下划线前缀——
/// 各 cfg 分支只用其一,下划线允许"可能未用"。
fn is_hidden(_meta: &std::fs::Metadata, _path: &str) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        _meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
    }
    #[cfg(not(windows))]
    {
        std::path::Path::new(_path)
            .file_name()
            .map(|n| n.to_string_lossy().starts_with('.'))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::{Value, VmThread};

    /// **RED→GREEN**(Layer 4.25):`WinNTFileSystem.initIDs()V` native 空操作返 void。
    /// HotSpot `Java_java_io_WinNTFileSystem_initIDs` 仅缓存字段 ID,无 FS 访问。
    /// 验证 `java/io/` 路由 + arm 就位(修前 `java/io/*` 落 `_ => ULE`)。
    #[test]
    fn init_ids_returns_void() {
        let registry = ClassRegistry::new();
        let mut vm = VmThread::new(registry);
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

    /// **RED→GREEN**(Layer 4.35):`FileDescriptor.initIDs()V` native 空操作返 void。
    /// HotSpot `Java_java_io_FileDescriptor_initIDs`(`FileDescriptor_md.c`)仅缓存字段 ID
    /// (`IO_fd`/`IO_handle`/`IO_append`),无 FS 访问。`FileDescriptor.<clinit>:65` 首调 →
    /// 解锁 nio `WindowsChannelFactory.<clinit>`→`SharedSecrets.getJavaIOFileDescriptorAccess`
    /// →`ensureClassInitialized(FileDescriptor)`→`FileDescriptor.<clinit>` 链。
    #[test]
    fn file_descriptor_init_ids_returns_void() {
        let registry = ClassRegistry::new();
        let mut vm = VmThread::new(registry);
        let r = super::super::invoke(
            &mut vm,
            "java/io/FileDescriptor",
            "initIDs",
            "()V",
            None,
            &[],
        )
        .expect("FileDescriptor.initIDs 应返 void,非抛异常");
        assert!(matches!(r, Value::Void), "须返 void,得 {r:?}");
    }

    /// **RED→GREEN**(Layer 4.36):`FileDescriptor.getHandle(int)long`(FileDescriptor.java:227)
    /// 与 `getAppend(int)boolean`(FileDescriptor.java:232)——`private static native`,由私有
    /// `FileDescriptor(int fd)<init>:129` 调用以设置 std 流(in=0/out=1/err=2)的 OS handle/append。
    /// `<clinit>:151` 创建 in/out/err 触发。HotSpot Windows 用 `GetStdHandle`/读 append 标志;
    /// rustj 不接 OS stdio handle → `getHandle` 返 `fd` 作 placeholder(0/1/2,非 -1 即非 invalid),
    /// `getAppend` 返 false(std 流非 append)。
    #[test]
    fn file_descriptor_get_handle_and_get_append_return_placeholders() {
        let registry = ClassRegistry::new();
        let mut vm = VmThread::new(registry);
        // getHandle(0/1/2) → 0/1/2(placeholder;非 -1 即非 invalid handle)。
        for fd in [0i32, 1, 2] {
            let h = super::super::invoke(
                &mut vm,
                "java/io/FileDescriptor",
                "getHandle",
                "(I)J",
                None,
                &[Value::Int(fd)],
            )
            .expect("getHandle 应返 long,非抛异常");
            match h {
                Value::Long(v) => assert_eq!(v, fd as i64, "getHandle({fd}) placeholder 须 == fd"),
                other => panic!("getHandle 须返 long,得 {other:?}"),
            }
        }
        // getAppend(0/1/2) → false(std 流非 append)。
        for fd in [0i32, 1, 2] {
            let a = super::super::invoke(
                &mut vm,
                "java/io/FileDescriptor",
                "getAppend",
                "(I)Z",
                None,
                &[Value::Int(fd)],
            )
            .expect("getAppend 应返 boolean,非抛异常");
            assert_eq!(a, Value::Int(0), "getAppend({fd}) 须返 false(0)");
        }
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
        let mut vm = VmThread::new(registry);
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
