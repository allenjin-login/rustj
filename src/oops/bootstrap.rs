//! 引导异常类(对应 HotSpot 的 `java/lang/*` 核心异常桩)。
//!
//! JVM 抛出的运行时异常(NPE/CCE/ArithmeticException 等)与 `catch(Throwable)`/
//! `catch(Exception)` 的匹配需要标准 `java.lang.*` 异常层次**可加载**。本模块在
//! [`ClassRegistry::new`] 时合成最小 `ClassFile` 并注册,使既有 `supertypes_of`/
//! `is_instance`/`new_instance`/`invokespecial` 机制**零特殊分支**即可解析标准层次。
//!
//! 合成桩 = 类名 + 直接超类 + **空 `<init>()V`**(字节码仅 `return`,无副作用)。给桩
//! 配构造器而非让 `invokespecial` 对"已加载但无 `<init>`"开特判——桩与用户加载的类
//! **同构**(都带 `<init>`,`find_method` 一视同仁),`invoke_special` 路径保持单一机制。
//!
//! 为何 eager 安装:`Vm` 以不可变借用持注册表,运行时无法 `&mut` 追加;故必须在 Vm
//! 借用前(注册表构造期)装好。桩即普通 `LoadedClass`,与用户加载的类同构(单一机制)。

use crate::classfile::attributes::CodeAttribute;
use crate::classfile::Reader;
use crate::constant_pool::ConstantPool;
use crate::metadata::access_flags::ACC_PUBLIC;
use crate::metadata::{AccessFlags, ClassFile, MethodInfo};

use super::ClassRegistry;

/// 标准 `java.lang.*` 异常层次(`(类内部名, 直接超类)`;`None` = `Object` 根)。
/// 单一真相源;新增可抛异常在此追加一行即可。
const BOOTSTRAP_HIERARCHY: &[(&str, Option<&str>)] = &[
    ("java/lang/Object", None),
    ("java/lang/Throwable", Some("java/lang/Object")),
    ("java/lang/Error", Some("java/lang/Throwable")),
    ("java/lang/LinkageError", Some("java/lang/Error")),
    ("java/lang/ExceptionInInitializerError", Some("java/lang/LinkageError")),
    ("java/lang/NoClassDefFoundError", Some("java/lang/LinkageError")),
    ("java/lang/UnsatisfiedLinkError", Some("java/lang/LinkageError")),
    ("java/lang/AbstractMethodError", Some("java/lang/Error")),
    ("java/lang/StackOverflowError", Some("java/lang/Error")),
    ("java/lang/Exception", Some("java/lang/Throwable")),
    ("java/lang/RuntimeException", Some("java/lang/Exception")),
    ("java/lang/ReflectiveOperationException", Some("java/lang/Exception")),
    ("java/lang/ClassNotFoundException", Some("java/lang/ReflectiveOperationException")),
    ("java/lang/IndexOutOfBoundsException", Some("java/lang/RuntimeException")),
    ("java/lang/ArrayIndexOutOfBoundsException", Some("java/lang/IndexOutOfBoundsException")),
    ("java/lang/StringIndexOutOfBoundsException", Some("java/lang/IndexOutOfBoundsException")),
    ("java/lang/NullPointerException", Some("java/lang/RuntimeException")),
    ("java/lang/ClassCastException", Some("java/lang/RuntimeException")),
    ("java/lang/ArithmeticException", Some("java/lang/RuntimeException")),
    ("java/lang/ArrayStoreException", Some("java/lang/RuntimeException")),
    ("java/lang/NegativeArraySizeException", Some("java/lang/RuntimeException")),
];

/// 在字节流尾追加一个 `Utf8` 条目,返回它的常量池索引(并推进 `next`)。
fn push_utf8(entries: &mut Vec<u8>, next: &mut u16, s: &str) -> u16 {
    let idx = *next;
    entries.push(0x01);
    entries.extend_from_slice(&(s.len() as u16).to_be_bytes());
    entries.extend_from_slice(s.as_bytes());
    *next += 1;
    idx
}

/// 在字节流尾追加一个 `Class` 条目(指向 `name_index`),返回它的索引(并推进 `next`)。
fn push_class(entries: &mut Vec<u8>, next: &mut u16, name_index: u16) -> u16 {
    let idx = *next;
    entries.push(0x07);
    entries.extend_from_slice(&name_index.to_be_bytes());
    *next += 1;
    idx
}

/// 合成最小 `ClassFile`:常量池 = `Utf8(name)` + (`Utf8(super)`) + `<init>`/`()V` 串 +
/// 两个 `Class` 条目;方法表含一个空 `<init>()V`(`return`,无副作用)。空 fields。
/// `super_name == None` → `Object`(super_class = 0)。
fn synth_classfile(name: &str, super_name: Option<&str>) -> ClassFile {
    let mut entries: Vec<u8> = Vec::new();
    let mut next: u16 = 1;

    // ---- Utf8 条目(name / super / "<init>" / "()V")----
    let name_idx = push_utf8(&mut entries, &mut next, name);
    let super_utf8_idx = super_name.map(|sn| push_utf8(&mut entries, &mut next, sn));
    let init_name_idx = push_utf8(&mut entries, &mut next, "<init>");
    let init_desc_idx = push_utf8(&mut entries, &mut next, "()V");

    // ---- Class 条目(this / super)----
    let this_class = push_class(&mut entries, &mut next, name_idx);
    let super_class = match super_utf8_idx {
        Some(su) => push_class(&mut entries, &mut next, su),
        // Object 无超类(super_class = 0)。
        None => 0,
    };

    let count = next;
    let mut cp_bytes = count.to_be_bytes().to_vec();
    cp_bytes.extend_from_slice(&entries);
    let constant_pool = ConstantPool::parse(&mut Reader::new(&cp_bytes))
        .expect("合成引导类常量池解析失败(内部不变量)");

    // 空 <init>()V:仅 `return`(0xb1),无副作用——让桩的 <init> 链在首个桩自然终止,
    // 无需 `invokespecial` 对"已加载但无 <init>"开特判。
    let init = MethodInfo {
        access_flags: AccessFlags::from_bits(ACC_PUBLIC),
        name_index: init_name_idx,
        descriptor_index: init_desc_idx,
        attributes: Vec::new(),
        code: Some(CodeAttribute {
            max_stack: 0,
            max_locals: 1, // this
            code: vec![0xb1],
            exception_table: Vec::new(),
            attributes: Vec::new(),
        }),
    };

    ClassFile {
        minor_version: 0,
        major_version: 52,
        constant_pool,
        access_flags: AccessFlags::from_bits(0),
        this_class,
        super_class,
        interfaces: Vec::new(),
        fields: Vec::new(),
        methods: vec![init],
        attributes: Vec::new(),
    }
}

/// 把标准异常层次全部合成并加载进注册表(构造期调用)。
pub(super) fn install_bootstrap(reg: &mut ClassRegistry) {
    for &(name, super_name) in BOOTSTRAP_HIERARCHY {
        let cf = synth_classfile(name, super_name);
        // load_stub(而非 load):置 is_synthetic_stub,使闭包加载器能用真类覆盖这些桩。
        reg.load_stub(cf)
            .expect("引导类加载失败(内部不变量)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oops::ClassRegistry;

    #[test]
    fn synth_classfile_records_name_and_super() {
        let cf = synth_classfile("Foo", Some("java/lang/Object"));
        assert_eq!(cf.this_class_name(), Some("Foo"));
        assert_eq!(cf.super_class_name(), Some("java/lang/Object"));
    }

    #[test]
    fn synth_classfile_object_has_no_super() {
        let cf = synth_classfile("java/lang/Object", None);
        assert_eq!(cf.this_class_name(), Some("java/lang/Object"));
        assert_eq!(cf.super_class_name(), None);
    }

    /// 每个桩带一个空 `<init>()V`(code = 单字节 `return`)——保证 `invokespecial`
    /// 沿用户类 `<init>` 链上行到桩时 `find_method` 命中,链自然终止,无需特判。
    #[test]
    fn synth_classfile_has_empty_init() {
        let cf = synth_classfile("Foo", Some("java/lang/Object"));
        let init = cf
            .methods
            .iter()
            .find(|m| {
                let n = matches!(cf.constant_pool.get(m.name_index), Ok(crate::constant_pool::ConstantPoolEntry::Utf8(s)) if s == "<init>");
                let d = matches!(cf.constant_pool.get(m.descriptor_index), Ok(crate::constant_pool::ConstantPoolEntry::Utf8(s)) if s == "()V");
                n && d
            })
            .expect("应有 <init>()V");
        let code = init.code.as_ref().expect("<init> 应有 Code");
        assert_eq!(code.code, vec![0xb1], "<init> 应仅 return");
        assert_eq!(code.max_locals, 1, "<init> 应保留 this 槽");
    }

    #[test]
    fn load_or_replace_swaps_class() {
        let mut reg = ClassRegistry::new();
        // 先合成 Foo(超类 Object)。
        reg.load(synth_classfile("Foo", Some("java/lang/Object")))
            .unwrap();
        assert_eq!(reg.get("Foo").unwrap().super_class_name(), Some("java/lang/Object"));
        // load_or_replace 同名 Foo(超类改 Throwable)— 末胜:覆盖后超类应变。
        reg.load_or_replace(synth_classfile("Foo", Some("java/lang/Throwable")))
            .unwrap();
        assert_eq!(
            reg.get("Foo").unwrap().super_class_name(),
            Some("java/lang/Throwable"),
            "load_or_replace 须覆盖同名已注册类"
        );
    }

    #[test]
    fn install_bootstrap_loads_standard_hierarchy() {
        let reg = ClassRegistry::new();
        for name in [
            "java/lang/Object",
            "java/lang/Throwable",
            "java/lang/Exception",
            "java/lang/RuntimeException",
            "java/lang/NullPointerException",
            "java/lang/ArithmeticException",
            "java/lang/ArrayIndexOutOfBoundsException",
            "java/lang/IndexOutOfBoundsException",
            "java/lang/AbstractMethodError",
            "java/lang/StackOverflowError",
            "java/lang/LinkageError",
            "java/lang/ExceptionInInitializerError",
            "java/lang/NoClassDefFoundError",
            "java/lang/UnsatisfiedLinkError",
        ] {
            assert!(reg.get(name).is_some(), "{name} 应已加载");
        }
    }

    /// 经注册表 `find_exact_method`(同 `invoke_special` 的查法)能取到桩的 `<init>()V`——
    /// 即抛出与构造链不会因"桩无方法"而报错。镜像 invoke 路径,作集成闸门的单元级先验。
    #[test]
    fn bootstrap_stubs_have_findable_init() {
        let reg = ClassRegistry::new();
        for name in ["java/lang/Object", "java/lang/RuntimeException", "java/lang/Throwable"] {
            let (lc, m) = reg
                .find_exact_method(name, "<init>", "()V")
                .unwrap_or_else(|| panic!("{name} 应有可查 <init>()V"));
            assert_eq!(lc.name(), name);
            assert_eq!(
                m.code.as_ref().unwrap().code,
                vec![0xb1],
                "{name}.<init> 应为空 return"
            );
        }
    }

    #[test]
    fn is_instance_walks_standard_exception_hierarchy() {
        let reg = ClassRegistry::new();
        // NPE 是 RuntimeException/Exception/Throwable/Object
        assert!(reg.is_instance("java/lang/NullPointerException", "java/lang/RuntimeException"));
        assert!(reg.is_instance("java/lang/NullPointerException", "java/lang/Exception"));
        assert!(reg.is_instance("java/lang/NullPointerException", "java/lang/Throwable"));
        assert!(reg.is_instance("java/lang/NullPointerException", "java/lang/Object"));
        // NPE 不是 Error
        assert!(!reg.is_instance("java/lang/NullPointerException", "java/lang/Error"));
        // AIOOBE 是 IndexOutOfBoundsException 的子类
        assert!(reg.is_instance(
            "java/lang/ArrayIndexOutOfBoundsException",
            "java/lang/IndexOutOfBoundsException"
        ));
        // ArithmeticException 不是 NPE
        assert!(!reg.is_instance("java/lang/ArithmeticException", "java/lang/NullPointerException"));
        // Throwable 捕获一切异常(用户层常见 catch(Throwable))
        assert!(reg.is_instance("java/lang/ClassCastException", "java/lang/Throwable"));
        // Error 与 Exception 平级:Error 不是 Exception
        assert!(!reg.is_instance("java/lang/StackOverflowError", "java/lang/Exception"));
        assert!(reg.is_instance("java/lang/StackOverflowError", "java/lang/Error"));
    }
}
