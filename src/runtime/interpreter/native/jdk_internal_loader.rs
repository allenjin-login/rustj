//! `jdk/internal/loader/{NativeLibraries,NativeLibrary}` 的 native 桥。
//!
//! 语义移植自 `src/java.base/share/native/libjava/NativeLibraries.c` 的
//! `Java_jdk_internal_loader_NativeLibraries_{load,unload,findBuiltinLib}` 与
//! `NativeLibrary_findEntry0`。这些 JNI 函数底层调 HotSpot 的
//! `JVM_{LoadLibrary,UnloadLibrary,FindLibraryEntry}`(jvm.cpp:3150/3182/3188),即
//! `os::dll_load` / `dll_unload` / `dll_lookup`——rustj 移植为 [`crate::runtime::os`]
//! (跨平台 `LoadLibraryW`/`GetProcAddress`/`FreeLibrary`/`GetModuleHandleW`)。
//!
//! 由 [`super`] 的 `NativeRegistry` 按 (class,name,desc) 命中——`register_all` 时调本模块
//! `register`([`natives!`] 生成)登记各条;`invoke_inner` 查表得 fn 指针即调
//! (`jdk/internal/loader/NativeLibraries` / `NativeLibrary` / `BootLoader`)。
//!
//! **Step 0 源码依据**:
//! - NativeLibraries.c:102 `load(impl, name, isBuiltin, throwExceptionIfFail)Z`:
//!   `handle = isBuiltin ? procHandle : JVM_LoadLibrary(cname, throwExceptionIfFail)`;
//!   成功 → 查 `JNI_OnLoad`/`JNI_OnLoad_<libname>`,有则调取 JNI 版本(`jniVersion = (*OnLoad)(jvm,NULL)`),
//!   无则 `jniVersion = 0x00010001`(JNI 1.1);写 `impl.jniVersion` + `impl.handle`;返 true。
//!   失败 + throwExceptionIfFail → 抛 `UnsatisfiedLinkError`;否则返 false。
//! - NativeLibraries.c:179 `unload(name, isBuiltin, handle)V`:查 `JNI_OnUnload`;!isBuiltin 时
//!   `JVM_UnloadLibrary(handle)`。
//! - NativeLibraries.c:214 `NativeLibrary.findEntry0(handle, name)J` = `JVM_FindLibraryEntry`。
//! - NativeLibraries.c:234 `findBuiltinLib(name)String`:剥前/后缀,查 `JNI_OnLoad_<libname>` 于
//!   进程句柄;rustj **无静态链接 builtin 库** → 恒 null(规范:非 builtin 返 null,上层据此走 dll_load)。
//!
//! **JNI_OnLoad 处理(rustj 取舍)**:rustj 无 JNIEnv/JavaVM*,无法真正调用库的 `JNI_OnLoad`。
//! 故按 NativeLibraries.c:130 的"无 OnLoad 符号"分支:`jniVersion = 0x00010001`(JNI 1.1)。
//! java.base 自身的 native 已由 rustj 编译期表覆盖(不依赖库的 `Java_*` 符号派发),故此取舍
//! 不影响 java.base 加载——动态库装载子系统此处只负责"装载 + 句柄 + 符号查找"。

use crate::oops::Oop;
use crate::runtime::{os, Reference, Slot, Value, VmThread, VmError};

use super::super::{string, throw_exception};

natives! {
    // NativeLibraries.load(impl, name, isBuiltin, throwExceptionIfFail)Z —— 描述符经 jmod 实测
    // (NativeLibraries.class,major 69)。isBuiltin→process_handle;否则 os::dll_load(name)。
    (
        "jdk/internal/loader/NativeLibraries",
        "load",
        "(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z",
    ) => |vm, _this, args| native_load(vm, args);

    // NativeLibraries.unload(name, isBuiltin, handle)V —— 描述符经 jmod 实测。rustj 无
    // JNI_OnUnload 调用环境;仅 !isBuiltin 时 os::dll_unload。恒空操作返回。
    (
        "jdk/internal/loader/NativeLibraries",
        "unload",
        "(Ljava/lang/String;ZJ)V",
    ) => |vm, _this, args| native_unload(vm, args);

    // NativeLibraries.findBuiltinLib(name)String —— 描述符经 jmod 实测。rustj 无静态链接
    // builtin 库 → 恒 null(上层 loadLibrary 据 null 走 !isBuiltin 分支 = dll_load 路径)。
    (
        "jdk/internal/loader/NativeLibraries",
        "findBuiltinLib",
        "(Ljava/lang/String;)Ljava/lang/String;",
    ) => |_vm, _this, _args| Ok(Value::Reference(Reference::null()));

    // NativeLibrary.findEntry0(handle, name)J —— 描述符经 jmod 实测 = os::dll_lookup。
    (
        "jdk/internal/loader/NativeLibrary",
        "findEntry0",
        "(JLjava/lang/String;)J",
    ) => |vm, _this, args| find_entry0(vm, args);

    // BootLoader.setBootLoaderUnnamedModule0(Module)V —— BootLoader.java:334 private static
    // native。<clinit>:71 调用。HotSpot JVM_SetBootLoaderUnnamedModule 把 boot loader 关联其
    // unnamed module 到原生模块层;rustj 纯 Rust 模型无原生模块层 → 空操作(Module 已 Java 侧
    // 经 jla.defineUnnamedModule 建)。解锁 WindowsNativeDispatcher.<clinit>→BootLoader.<clinit>。
    (
        "jdk/internal/loader/BootLoader",
        "setBootLoaderUnnamedModule0",
        "(Ljava/lang/Module;)V",
    ) => |_vm, _this, _args| Ok(Value::Void);
}

/// 取实例的声明类内部名(写回字段时按类反查扁平布局序号)。
fn instance_class_name(vm: &VmThread, r: Reference) -> Option<String> {
    match vm.heap().get(r) {
        Some(Oop::Instance(i)) => Some(i.class_name().to_string()),
        _ => None,
    }
}

/// 按字段名查它在声明类扁平实例布局中的序号(ord)。flattened_instance_fields 含继承字段;
/// NativeLibraryImpl 的 handle/jniVersion 声明在自身(NativeLibrary 父类无实例字段)。
fn named_field_ord(vm: &VmThread, class_name: &str, field_name: &str) -> Option<usize> {
    vm.registry().and_then(|reg| {
        reg.get(class_name).and_then(|lc| {
            reg.flattened_instance_fields(&lc)
                .iter()
                .position(|f| f.name == field_name)
        })
    })
}

/// `NativeLibraries.load(impl, name, isBuiltin, throwExceptionIfFail)Z` 的实现。
///
/// - `name` 读自第 1 参(真 String;**不读** impl.name 字段)。
/// - `handle = isBuiltin ? process_handle() : dll_load(name)`;null = 失败。
/// - 成功 → 写 `impl.handle`(ord by name)= handle、`impl.jniVersion` = JNI 1.1(0x00010001);返 1。
/// - 失败 + throwExceptionIfFail → 抛 `UnsatisfiedLinkError`;否则返 0。
///
/// JNI 版本取 0x00010001(NativeLibraries.c:130 "无 OnLoad 符号"分支;rustj 无 JNIEnv 不调 OnLoad)。
fn native_load(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let (impl_ref, name_ref, is_builtin, throw_if_fail) =
        match (args.first(), args.get(1), args.get(2), args.get(3)) {
            (
                Some(Value::Reference(i)),
                Some(Value::Reference(n)),
                Some(Value::Int(b)),
                Some(Value::Int(t)),
            ) => (*i, *n, *b != 0, *t != 0),
            _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
        };
    let name = match string::read_text(vm, name_ref)? {
        Some(t) => t,
        None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // handle = isBuiltin ? procHandle : dll_load(name)。两分支均不触堆写,纯 FFI。
    let handle = if is_builtin {
        os::process_handle()
    } else {
        os::dll_load(&name)
    };
    if handle.is_null() {
        if throw_if_fail {
            // NativeLibraries.c:149/161:加载失败 + throwExceptionIfFail → UnsatisfiedLinkError。
            return Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError"));
        }
        return Ok(Value::Int(0));
    }
    // 写 impl.jniVersion(I) + impl.handle(J):借注册表按名定位 ord(§6:'a 不绑 &self),
    // 出块后 heap_mut 写——与 objectFieldOffset1 / casReflectionData 同模式。
    let cn = instance_class_name(vm, impl_ref)
        .ok_or(VmError::BadConstant("NativeLibraryImpl:非实例"))?;
    let handle_ord = named_field_ord(vm, &cn, "handle")
        .ok_or(VmError::BadConstant("NativeLibraryImpl:缺 handle 字段"))?;
    let jni_ord = named_field_ord(vm, &cn, "jniVersion")
        .ok_or(VmError::BadConstant("NativeLibraryImpl:缺 jniVersion 字段"))?;
    match vm.heap_mut().get_mut(impl_ref) {
        Some(Oop::Instance(i)) => {
            i.set_field(handle_ord, Slot::Long(handle as i64));
            i.set_field(jni_ord, Slot::Int(0x0001_0001));
        }
        _ => return Err(VmError::BadConstant("NativeLibraryImpl:写字段非实例")),
    }
    Ok(Value::Int(1))
}

/// `NativeLibraries.unload(name, isBuiltin, handle)V` 的实现。!isBuiltin 时 `os::dll_unload`。
/// 不调 JNI_OnUnload(rustj 无 JNIEnv);恒空操作返回 Void。
fn native_unload(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let (name_ref, is_builtin, handle) = match (args.first(), args.get(1), args.get(2)) {
        (Some(Value::Reference(n)), Some(Value::Int(b)), Some(Value::Long(h))) => (*n, *b != 0, *h),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // name 仅用于 JNI_OnUnload_<libname> 查找(rustj 不调);读一下以校验非 null。
    let _ = string::read_text(vm, name_ref)?;
    if !is_builtin {
        os::dll_unload(handle as usize as *mut core::ffi::c_void);
    }
    Ok(Value::Void)
}

/// `NativeLibrary.findEntry0(handle, name)J` = `os::dll_lookup`。未找到 → 0(null 指针)。
fn find_entry0(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let (handle, name_ref) = match (args.first(), args.get(1)) {
        (Some(Value::Long(h)), Some(Value::Reference(n))) => (*h, *n),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let Some(sym) = string::read_text(vm, name_ref)? else {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    };
    let addr = os::dll_lookup(handle as usize as *mut core::ffi::c_void, &sym);
    Ok(Value::Long(addr as usize as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oops::{ClassRegistry, Oop};
    use crate::runtime::class_loader::class_path::ClassPath;
    use crate::runtime::class_loader::loader::load_closure;
    use crate::runtime::{Reference, Slot, Value, VmThread};

    use std::path::{Path, PathBuf};

    /// 找本机 java.base.jmod(同 tests/ 集成闸门的多版本回退)。
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

    /// 读实例 Long 字段(按名定位 ord)——用于断言 native_load 写回了 handle。
    fn read_long_field(vm: &VmThread, r: Reference, field_name: &str) -> Option<i64> {
        let cn = match vm.heap().get(r)? {
            Oop::Instance(i) => i.class_name().to_string(),
            _ => return None,
        };
        let ord = named_field_ord(vm, &cn, field_name)?;
        match vm.heap().get(r)? {
            Oop::Instance(i) => match i.field(ord) {
                Slot::Long(v) => Some(v),
                _ => None,
            },
            _ => None,
        }
    }

    /// 读实例 Int 字段——用于断言 native_load 写回了 jniVersion(= JNI 1.1)。
    fn read_int_field(vm: &VmThread, r: Reference, field_name: &str) -> Option<i32> {
        let cn = match vm.heap().get(r)? {
            Oop::Instance(i) => i.class_name().to_string(),
            _ => return None,
        };
        let ord = named_field_ord(vm, &cn, field_name)?;
        match vm.heap().get(r)? {
            Oop::Instance(i) => match i.field(ord) {
                Slot::Int(v) => Some(v),
                _ => None,
            },
            _ => None,
        }
    }

    /// 构造一个 bare `NativeLibraryImpl` 实例(load native 只写 handle/jniVersion,不读其他字段)。
    fn new_native_library_impl(vm: &mut VmThread) -> Reference {
        let inst = {
            let reg = vm.registry().expect("须有注册表");
            let lc = reg
                .get("jdk/internal/loader/NativeLibraries$NativeLibraryImpl")
                .expect("NativeLibraryImpl 须已加载");
            reg.new_instance(&lc)
        };
        vm.heap_mut().alloc(Oop::Instance(inst))
    }

    /// **集成闸门**:Layer 4.16 动态库装载 native 桥(load + findEntry0 + 真实 os::dll_load)。
    ///
    /// 直接经 [`super::super::invoke`] 调私有 native(无需 javac/Thread/ConcurrentHashMap/
    /// canonicalize——那些属 Thread/FileSystem 层)。证明:os 模块装载真 DLL → native 桥写回
    /// handle/jniVersion → findEntry0 经同一句柄查到真符号。
    #[test]
    fn native_libraries_load_loads_real_dll_and_finds_symbol() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };

        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        for c in [
            "java/lang/Object",
            "java/lang/Class",
            "java/lang/String",
            "jdk/internal/loader/NativeLibraries",
            "jdk/internal/loader/NativeLibraries$NativeLibraryImpl",
            "jdk/internal/loader/NativeLibrary",
        ] {
            load_closure(&mut registry, &cp, c).unwrap();
        }

        let mut vm = VmThread::new(registry);
        let impl_ref = new_native_library_impl(&mut vm);
        let name_ref = string::intern(&mut vm, "kernel32.dll").expect("intern 路径名");

        // NativeLibraries.load(impl, "kernel32.dll", isBuiltin=false, throwExceptionIfFail=true)Z
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/loader/NativeLibraries",
            "load",
            "(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z",
            None,
            &[
                Value::Reference(impl_ref),
                Value::Reference(name_ref),
                Value::Int(0),
                Value::Int(1),
            ],
        )
        .expect("load native 应成功加载 kernel32.dll");
        assert_eq!(r, Value::Int(1), "load 须返 true(成功)");
        // handle 写回非零;jniVersion = JNI 1.1(0x00010001,NativeLibraries.c:130 无 OnLoad 分支)。
        let handle = read_long_field(&vm, impl_ref, "handle").expect("handle 须被写为 Long");
        assert_ne!(handle, 0, "impl.handle 须为非零句柄");
        assert_eq!(
            read_int_field(&vm, impl_ref, "jniVersion"),
            Some(0x0001_0001),
            "impl.jniVersion 须为 JNI 1.1(0x00010001)"
        );

        // NativeLibrary.findEntry0(handle, "GetLastError")J → 非零地址(dll_lookup 命中)。
        let sym = string::intern(&mut vm, "GetLastError").expect("intern 符号名");
        let r2 = super::super::invoke(
            &mut vm,
            "jdk/internal/loader/NativeLibrary",
            "findEntry0",
            "(JLjava/lang/String;)J",
            None,
            &[Value::Long(handle), Value::Reference(sym)],
        )
        .expect("findEntry0 应成功");
        let Value::Long(addr) = r2 else {
            panic!("findEntry0 须返 Long,得 {r2:?}");
        };
        assert_ne!(addr, 0, "findEntry0 须查到 kernel32!GetLastError");

        // 加载不存在的库 + throwExceptionIfFail=true → UnsatisfiedLinkError(非 native 未登记)。
        let impl2 = new_native_library_impl(&mut vm);
        let missing = string::intern(&mut vm, "Z:/no/such/lib_4_16_zzz.dll").unwrap();
        let err = super::super::invoke(
            &mut vm,
            "jdk/internal/loader/NativeLibraries",
            "load",
            "(Ljdk/internal/loader/NativeLibraries$NativeLibraryImpl;Ljava/lang/String;ZZ)Z",
            None,
            &[
                Value::Reference(impl2),
                Value::Reference(missing),
                Value::Int(0),
                Value::Int(1),
            ],
        )
        .unwrap_err();
        match err {
            VmError::ThrownException(r) => match vm.heap().get(r) {
                Some(Oop::Instance(i)) => assert_eq!(
                    i.class_name(),
                    "java/lang/UnsatisfiedLinkError",
                    "加载失败须抛 UnsatisfiedLinkError"
                ),
                o => panic!("须 Instance 异常,得 {o:?}"),
            },
            e => panic!("须 ThrownException(ULE),得 {e:?}"),
        }
    }

    /// **RED→GREEN**(Layer 4.37):`BootLoader.setBootLoaderUnnamedModule0(Module)V`
    /// (BootLoader.java:334 `private static native`)——`<clinit>:71` 调用。HotSpot 把 boot loader
    /// 关联其 unnamed module 到原生模块层(JVM_SetBootLoaderUnnamedModule);rustj 纯 Rust 模型
    /// 无原生模块层 → 空操作(Module 已 Java 侧经 `jla.defineUnnamedModule` 建)。
    /// 解锁 `WindowsNativeDispatcher.<clinit>`→`BootLoader.<clinit>:66` 链。
    #[test]
    fn bootloader_set_unnamed_module_returns_void() {
        let registry = ClassRegistry::new();
        let mut vm = VmThread::new(registry);
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/loader/BootLoader",
            "setBootLoaderUnnamedModule0",
            "(Ljava/lang/Module;)V",
            None,
            &[Value::Reference(Reference::null())],
        )
        .expect("setBootLoaderUnnamedModule0 应返 void,非抛异常");
        assert!(matches!(r, Value::Void), "须返 void,得 {r:?}");
    }
}
