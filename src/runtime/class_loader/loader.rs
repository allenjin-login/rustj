//! 传递闭包加载器(closure-walker):从 [`ClassPath`] 按引用闭包**急切**预载入 [`ClassRegistry`]。
//!
//! 对应 HotSpot「解析」(符号引用→直接引用)+ 加载的并集。HotSpot 惰性解析——首次用到某
//! 符号引用时才触发其目标类的加载与解析;rustj 的 [`ClassRegistry`] 为**不可变借用**
//! ([`Vm`](crate::runtime::Vm) 构造后无法 `&mut` 追加,见
//! [`bootstrap`](crate::oops::bootstrap)),故在构造 `Vm` 前**急切**预载整个引用闭包——功能
//! 等价,代价是载入暂时未用的类(惰性仅是优化,非正确性所需)。
//!
//! **引用名抽取**(决定闭包的边):
//! 1. 所有 `Class` 常量池条目——覆盖超类、直接接口、field/method owner、`new`/`checkcast`/
//!    `instanceof`/`anewarray`/`multianewarray` 目标(数组目标 `[…]` 形式按字段描述符解析出
//!    组件类);
//! 2. field/method 描述符内的对象类型——`L…;` 不构成 `Class` 条目(仅存于描述符串),须解析
//!    描述符后取 [`FieldType::Class`](含数组组件递归)。
//!
//! 原语类型(`I`/`J`/…) 不出现于引用名(描述符为单字符原语分支,不产生 `Class`);
//! 数组名(`[…]`) 跳过(无对应 `.class`,其组件已在抽取时递归并入队列)。

use std::collections::{HashSet, VecDeque};

use crate::classfile::ClassFileError;
use crate::constant_pool::ConstantPoolEntry;
use crate::metadata::descriptor::{
    parse_field_descriptor, parse_method_descriptor, FieldType, ReturnDescriptor,
};
use crate::metadata::ClassFile;
use crate::oops::ClassRegistry;

use super::class_path::{ClassPath, ClassPathError};

/// 闭包加载错误:仅转发 [`ClassPath`] 的容器/解析失败(`.class` 解析或 zip 解压)。
#[derive(Debug)]
pub struct ClosureError {
    /// 源错误。
    pub source: ClassPathError,
}

impl From<ClassPathError> for ClosureError {
    fn from(source: ClassPathError) -> Self {
        Self { source }
    }
}

impl From<ClassFileError> for ClosureError {
    fn from(e: ClassFileError) -> Self {
        // `.class` 解析失败统一归入 ClassPath 的 ClassFile 变体。
        Self { source: ClassPathError::ClassFile(e) }
    }
}

/// 从 `root` 起 BFS 预载引用闭包进 `registry`——真 [`ClassPath`] 类用
/// [`ClassRegistry::load_or_replace`] **覆盖**同名合成桩(对应 capstone 的手动覆盖,推广到
/// 整个传递闭包)。返回新从 ClassPath 载入的类数。
///
/// ClassPath 与注册表均无的类(其他模块未含的引用)静默跳过:随模块与 native 补全再载。
/// 已处理名经 `seen` 去重,保证终止且每类至多解析一次。
///
/// **「惰性 = 构造 Vm 前急切预载」** 的兑现:`Vm` 以不可变借用持注册表,运行期无法 `&mut`
/// 追加;故本函数在 `Vm::new` 之前把可达类一次性灌入,使运行期解析恒命中已注册类。
pub fn load_closure(
    registry: &mut ClassRegistry,
    class_path: &ClassPath,
    root: &str,
) -> Result<usize, ClosureError> {
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(root.to_string());
    let mut seen: HashSet<String> = HashSet::new();
    let mut loaded = 0usize;

    while let Some(name) = queue.pop_front() {
        // 去重:每个类名至多处理一次(避免环 A→B→A 与重复解析)。
        if !seen.insert(name.clone()) {
            continue;
        }
        // 数组名([…])无可加载 .class;原语不会作为引用名出现 → 跳过。
        if name.starts_with('[') {
            continue;
        }

        // 已是真类(非合成桩)→ 无需再动。使二次调用幂等:首次把可达桩全换成真类后,
        // 二次调用对每个名都命中此分支而跳过,新载入计数恒为 0。
        let already_real = registry
            .get(&name)
            .is_some_and(|lc| !lc.is_synthetic_stub());
        if already_real {
            continue;
        }

        // 桩或缺失 → 从 ClassPath 取真类覆盖(load_or_replace 末胜,真类覆盖桩);
        // ClassPath 无但注册表已有(桩)→ 用桩的 cf 抽其引用(桩的边仅为 Object,无害);
        // 两处皆无(其他模块未含的引用)→ 跳过。
        let cf: &ClassFile = if let Some(real) = class_path.load_class(&name)? {
            let lc = registry.load_or_replace(real)?;
            loaded += 1;
            &lc.cf
        } else if let Some(lc) = registry.get(&name) {
            &lc.cf
        } else {
            continue;
        };
        // 抽取引用并入队(owned String,cf 的借用随即释放)。
        for referenced in referenced_names(cf) {
            queue.push_back(referenced);
        }
    }

    Ok(loaded)
}

/// 抽取一个 [`ClassFile`] 引用的全部类内部名(见模块文档的两条来源)。返回值含重复(由调用
/// 方的 `seen` 去重),顺序无关。
fn referenced_names(cf: &ClassFile) -> Vec<String> {
    let cp = &cf.constant_pool;
    let mut names: Vec<String> = Vec::new();

    // 1) Class 常量池条目(超类/接口/owner/new/checkcast/… 目标)。数组名解析出组件类。
    for (_, entry) in cp.iter() {
        if let ConstantPoolEntry::Class { name_index } = entry
            && let Ok(ConstantPoolEntry::Utf8(n)) = cp.get(*name_index)
        {
            if n.starts_with('[') {
                // [Ljava/lang/String; / [I / [[B —— 按字段描述符解析,取组件类。
                if let Ok(ft) = parse_field_descriptor(n) {
                    collect_class_types(&ft, &mut names);
                }
            } else {
                names.push(n.clone());
            }
        }
    }

    // 2) field 描述符内的对象类型。
    for f in &cf.fields {
        if let Ok(ConstantPoolEntry::Utf8(d)) = cp.get(f.descriptor_index)
            && let Ok(ft) = parse_field_descriptor(d)
        {
            collect_class_types(&ft, &mut names);
        }
    }

    // 3) method 描述符内的对象类型(形参 + 返回)。
    for m in &cf.methods {
        if let Ok(ConstantPoolEntry::Utf8(d)) = cp.get(m.descriptor_index)
            && let Ok(md) = parse_method_descriptor(d)
        {
            for ft in &md.parameters {
                collect_class_types(ft, &mut names);
            }
            if let ReturnDescriptor::FieldType(ft) = &md.return_type {
                collect_class_types(ft, &mut names);
            }
        }
    }

    names
}

/// 递归收集字段类型中的对象类名(数组下钻到组件)。
fn collect_class_types(ft: &FieldType, out: &mut Vec<String>) {
    match ft {
        FieldType::Class(name) => out.push(name.clone()),
        FieldType::Array(component) => collect_class_types(component, out),
        _ => {} // 原语类型无类名。
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 找到本机首个存在的 `java.base.jmod`;无则 `None`(集成闸门跳过)。
    fn find_javabase_jmod() -> Option<PathBuf> {
        for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
            let p = Path::new("C:/Program Files/Java")
                .join(ver)
                .join("jmods/java.base.jmod");
            if p.exists() {
                return Some(p);
            }
        }
        if let Ok(jh) = std::env::var("JAVA_HOME") {
            let p = Path::new(&jh).join("jmods/java.base.jmod");
            if p.exists() {
                return Some(p);
            }
        }
        None
    }

    /// 构造一个已装 `java.base.jmod` 的 [`ClassPath`];无 JDK 则 `None`。
    fn jmod_classpath() -> Option<ClassPath> {
        let jmod = find_javabase_jmod()?;
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add(
            jmod.file_name().and_then(|n| n.to_str()).unwrap_or("java.base.jmod"),
            &bytes,
        )
        .unwrap();
        Some(cp)
    }

    /// 抽取器单测:`Object.getClass()Ljava/lang/Class;` 的返回类(仅存于描述符,非 Class CP
    /// 条目)须被解析出——验证「描述符来源」这条边的正确性。无 JDK 则跳过。
    #[test]
    fn referenced_names_object_includes_class_from_descriptor() {
        let cp = match jmod_classpath() {
            Some(cp) => cp,
            None => {
                eprintln!("跳过:本机未找到 java.base.jmod");
                return;
            }
        };
        let obj = cp.load_class("java/lang/Object").unwrap().expect("Object 须在 jmod 内");
        let names = referenced_names(&obj);
        assert!(
            names.contains(&"java/lang/Class".to_string()),
            "Object.getClass 返回 java/lang/Class(描述符来源)须被抽出,实际:{names:?}"
        );
    }

    /// **集成闸门(4.10f)**:`load_closure(Object)` 从真 jmod 覆盖合成桩 Object,并按传递闭包
    /// 把 Object 的引用(如 `java/lang/Class`)一并预载——把 capstone 的手动单类覆盖推广到
    /// 整个闭包。无 JDK 则跳过。
    #[test]
    fn gate_closure_loads_object_and_transitive_deps() {
        let cp = match jmod_classpath() {
            Some(cp) => cp,
            None => {
                eprintln!("跳过:本机未找到 java.base.jmod");
                return;
            }
        };

        let mut registry = ClassRegistry::new(); // 含合成桩(Object 等)。

        // Object 是合成桩:load_closure 须用真 jmod 类覆盖,并预载其引用闭包。
        let loaded = load_closure(&mut registry, &cp, "java/lang/Object").unwrap();
        assert!(loaded >= 1, "至少应载入 Object 本身,实际:{loaded}");

        // 真 Object.hashCode 须为 ACC_NATIVE(桩无此法 → 覆盖成功)。
        let obj = registry.get("java/lang/Object").expect("Object 须已注册");
        let hash = obj
            .cf
            .methods
            .iter()
            .find(|m| {
                let n = matches!(obj.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "hashCode");
                let d = matches!(obj.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
                n && d
            })
            .expect("真 Object 须有 hashCode()I");
        assert!(hash.access_flags.is_native(), "真 Object.hashCode 须为 native");

        // 传递闭包:Object.getClass()Ljava/lang/Class; → java/lang/Class 须被预载
        // (Class 非合成桩,故其存在只能来自 ClassPath,证 BFS 跨了传递边)。
        assert!(
            registry.get("java/lang/Class").is_some(),
            "传递闭包须预载 java/lang/Class"
        );
    }

    /// 幂等:第二次 `load_closure` 不再新载(已全部注册,seen 去重)。
    #[test]
    fn closure_is_idempotent() {
        let cp = match jmod_classpath() {
            Some(cp) => cp,
            None => {
                eprintln!("跳过:本机未找到 java.base.jmod");
                return;
            }
        };
        let mut registry = ClassRegistry::new();
        let first = load_closure(&mut registry, &cp, "java/lang/Object").unwrap();
        let second = load_closure(&mut registry, &cp, "java/lang/Object").unwrap();
        assert!(first >= 1, "首次应载入 ≥1");
        assert_eq!(second, 0, "第二次不应新载(已注册,真类优先但不再计数为「新载」)");
    }
}
