//! 集成闸门(Phase G.0 探针):**BoundMethodHandle 类初始化** —— `BoundMethodHandle.<clinit>`
//! 触发 `ClassSpecializer.<clinit>` → `ConstantUtils.referenceClassDesc` → `Class.descriptorString`
//! → `Class.isHidden`(native)。本探针隔离「<clinit> 墙」(isHidden 等 native 缺口)于「物种类生成墙」
//! (defineClass,G.1)之前。
//!
//! **RED**:`Class.isHidden` native 缺 → `ExceptionInInitializerError`(ULE at isHidden)。
//! **GREEN**:补 `isHidden`→false(+ <clinit> 暴露的其余 native)后,BoundMethodHandle 类初始化成。
//!
//! 需 javac + 本机 jmod;缺一跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::clinit::ensure_class_initialized;
use rustj::runtime::interpreter::launch::{
    bootstrap_java_lang_invoke, bootstrap_module_system, initialize_system_class,
};
use rustj::runtime::{Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

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

/// 载入 BMH 物种生成依赖:java.lang.invoke 核心 + Class-File API(java.lang.classfile)+
/// 常量描述符(java.lang.constant / jdk.internal.constant)。
fn setup_vm() -> Option<Vm> {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return None;
    }
    let jmod = find_javabase_jmod()?;
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let mut registry = ClassRegistry::new();
    // java.lang.invoke 核心(BMH/ClassSpecializer/DMH/...)——load_closure 传递性拉依赖。
    for c in [
        "java/lang/invoke/BoundMethodHandle",
        "java/lang/invoke/ClassSpecializer",
        "java/lang/invoke/DirectMethodHandle",
        "java/lang/invoke/MethodHandle",
        "java/lang/invoke/MethodHandleImpl",
        "java/lang/invoke/MethodHandles",
        "java/lang/invoke/MemberName",
        "java/lang/invoke/MethodType",
        "java/lang/invoke/MethodHandleNatives",
        "java/lang/invoke/LambdaForm",
        // Class-File API 入口(ClassSpecializer.generateConcreteSpeciesCode 用 ClassFile.of().build)。
        "java/lang/classfile/ClassFile",
        // 常量描述符(ConstantUtils.referenceClassDesc / ClassOrInterfaceDescImpl)。
        "jdk/internal/constant/ConstantUtils",
        "jdk/internal/constant/ClassOrInterfaceDescImpl",
        "java/lang/constant/ClassDesc",
        // 基础。
        "java/lang/Class",
        "java/lang/Object",
        "java/lang/String",
        "jdk/internal/misc/VM",
    ] {
        load_closure(&mut registry, &cp, c).unwrap();
    }
    let mut vm = Vm::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 应成功");
    bootstrap_java_lang_invoke(&mut vm).expect("Phase 3 lite 应成功");
    Some(vm)
}

/// **RED→GREEN**(G.0→G.1):`BoundMethodHandle.<clinit>` 经 ClassSpecializer.<clinit> →
/// Class.descriptorString → Class.isHidden 不抛 EIIE。
///
/// **G.0** 修了 isHidden/LangReflectAccess/invokestatic/getChar 四墙;物种字节码经 Class-File API
/// 全量生成成功。**G.1b RED**:`ClassLoader.defineClass0`(装载生成字节)native 缺 → ULE。
/// **G.1b GREEN**:绑 defineClass0(byte[]→classfile::parse→define_class→intern mirror)后,
/// BMH.<clinit> 物种类(`BoundMethodHandle$Species_*`)被 defineClass 注册,类初始化成。
#[test]
fn bound_method_handle_clinit_succeeds() {
    let Some(mut vm) = setup_vm() else { return };
    match ensure_class_initialized(&mut vm, "java/lang/invoke/BoundMethodHandle") {
        Ok(()) => {}
        Err(VmError::ThrownException(r)) => {
            let trace = vm.format_trace(r);
            panic!("BoundMethodHandle.<clinit> 应成,实抛:\n{trace}");
        }
        Err(e) => panic!("BoundMethodhandle.<clinit> 内部错误:{e:?}"),
    }
}
