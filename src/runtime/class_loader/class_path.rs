//! 类路径(`ClassPath`):容器(jar/jmod)列表 + 按需 [`Self::load_class`]。
//!
//! 对应 HotSpot `classfile/classLoader.cpp` 的 `ClassLoader` 持有的 `ClassPathEntry` 列表 +
//! `load_classfile`(类内部名 → `name + ".class"` → 逐容器 `open_stream` 取字节 →
//! `ClassFileParser` 解析)。jmod 布局把类放在 `classes/` 前缀下(jar 无此前缀),故每容器
//! 同时尝试裸名与 `classes/` 前缀两种条目名。
//!
//! **源码依据(Step 0)**:`classLoader.cpp` `file_name_for_class_name`(987-1003)把
//! `java.lang.Object` → `java/lang/Object.class`;`ClassPathZipEntry::open_stream`
//! (361-434)经中心目录取条目、DEFLATE 则解压。本模块组合 4.10b 的 [`ZipReader`] +
//! `classfile::parse`。

use crate::classfile::{self, ClassFileError};
use crate::metadata::{ClassFile, ModuleDescriptor};

use std::collections::HashMap;

use super::zip::{ZipError, ZipReader};

/// 类路径错误:容器(zip)损坏 或 `.class` 解析失败。
#[derive(Debug)]
pub enum ClassPathError {
    /// 容器(zip)读取/解压错误。
    Zip(ZipError),
    /// `.class` 字节解析错误。
    ClassFile(ClassFileError),
}

impl From<ZipError> for ClassPathError {
    fn from(e: ZipError) -> Self {
        Self::Zip(e)
    }
}
impl From<ClassFileError> for ClassPathError {
    fn from(e: ClassFileError) -> Self {
        Self::ClassFile(e)
    }
}

/// 一个已打开的容器(jar/jmod):显示名(诊断用)+ 已解析的 zip 视图 + 模块名(若有)。
///
/// `module_name`:`module-info.class`(`classes/module-info.class`)经 [`ModuleDescriptor`]
/// 解析得的模块名(jmod 布局);非模块容器(jar/裸 .class)为 `None`。供 `load_class` 把源
/// 容器的模块名带回 `load_closure`,使每个加载的类与所属模块关联(`Class.getModule()` 用)。
struct Container {
    #[allow(dead_code)] // 仅诊断/调试用,当前未读
    name: String,
    zip: ZipReader,
    module_name: Option<String>,
}

/// 类路径:容器列表。`load_class` 按内部名逐容器查条目并解析为 [`ClassFile`]。
pub struct ClassPath {
    containers: Vec<Container>,
    /// 模块名 → 完整 Rust [`ModuleDescriptor`](`module-info.class` 经 [`ModuleDescriptor::from_class_file`]
    /// 解析得)。`add()` 解析后登记;供 `load_closure` 经 [`Self::module_descriptor`] 回带进注册表,
    /// 使 Layer 4.14c bootstrap `populate_module_exports` 能填 java `Module.descriptor`/`exportedPackages`
    /// (访问检查读实例字段 `exportedPackages`,非 `descriptor.exports()`;详见 spec)。
    modules: HashMap<String, ModuleDescriptor>,
}

impl ClassPath {
    /// 空类路径。
    pub fn new() -> Self {
        Self {
            containers: Vec::new(),
            modules: HashMap::new(),
        }
    }

    /// 追加一个容器(原始 zip 字节;显示名仅诊断用)。内部解析中心目录一次,持有副本。
    ///
    /// 若容器含 `classes/module-info.class`(jmod 布局)或 `module-info.class`(jar 布局),
    /// 解析其 `Module` 属性得模块名,存于容器(供 [`Self::load_class`] 回带源模块)。
    pub fn add(&mut self, name: impl Into<String>, bytes: &[u8]) -> Result<(), ClassPathError> {
        let zip = ZipReader::new(bytes)?;
        // 尝试两种布局取 module-info.class;解析其完整 Module 属性(4.11 ModuleDescriptor)。
        // 保留完整描述符(不只 name):登记 name→desc 供 4.14c bootstrap 填 Module.exportedPackages。
        let module_name = (|| -> Result<Option<String>, ClassPathError> {
            let raw = zip
                .read("classes/module-info.class")
                .ok()
                .flatten()
                .or_else(|| zip.read("module-info.class").ok().flatten());
            let Some(raw) = raw else {
                return Ok(None);
            };
            let cf = classfile::parse(&raw)?;
            let Some(desc) = ModuleDescriptor::from_class_file(&cf)? else {
                return Ok(None);
            };
            let mod_name = desc.name().to_string();
            self.modules.insert(mod_name.clone(), desc);
            Ok(Some(mod_name))
        })()?;
        self.containers.push(Container {
            name: name.into(),
            zip,
            module_name,
        });
        Ok(())
    }

    /// 容器数。
    pub fn len(&self) -> usize {
        self.containers.len()
    }

    /// 是否无容器。
    pub fn is_empty(&self) -> bool {
        self.containers.is_empty()
    }

    /// 模块名 → 完整 Rust [`ModuleDescriptor`](`add()` 解析自 `module-info.class`)。
    /// 供 `load_closure` 把源容器模块的完整描述符回带进注册表(Layer 4.14c:解锁 Module
    /// 反射访问检查;bootstrap 据其 exports 填 `Module.exportedPackages`)。无名模块 → `None`。
    pub fn module_descriptor(&self, module_name: &str) -> Option<ModuleDescriptor> {
        self.modules.get(module_name).cloned()
    }

    /// 按内部名加载类:逐容器查 `name.class`(jar 布局)与 `classes/name.class`(jmod 布局)。
    ///
    /// 返回 `Ok(Some((cf, module_name)))` = 找到且解析成功(`module_name` = 源容器的模块名,
    /// `None` 表示该容器非模块容器,类属无名模块);`Ok(None)` = 所有容器均无此条目;
    /// `Err` = 容器损坏或 `.class` 解析失败。
    pub fn load_class(
        &self,
        internal_name: &str,
    ) -> Result<Option<(ClassFile, Option<String>)>, ClassPathError> {
        let bare = format!("{internal_name}.class");
        let under_classes = format!("classes/{internal_name}.class");
        for c in &self.containers {
            for candidate in [bare.as_str(), under_classes.as_str()] {
                if let Some(raw) = c.zip.read(candidate)? {
                    let cf = classfile::parse(&raw)?;
                    return Ok(Some((cf, c.module_name.clone())));
                }
            }
        }
        Ok(None)
    }
}

impl Default for ClassPath {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 找到本机首个存在的 `java.base.jmod`;无则 `None`(集成闸门跳过)。
    /// 试 `C:/Program Files/Java/<ver>/jmods/java.base.jmod` 与 `$JAVA_HOME`。
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

    #[test]
    fn classpath_default_is_empty() {
        assert!(ClassPath::new().is_empty());
        assert_eq!(ClassPath::new().len(), 0);
    }

    #[test]
    fn add_rejects_non_zip() {
        let mut cp = ClassPath::new();
        // ≥22 字节但无 EOCD 签名 → 非法容器。
        assert!(cp.add("bad", &[0u8; 30]).is_err());
        assert!(cp.is_empty(), "失败不应残留容器");
    }

    #[test]
    fn load_class_missing_returns_none() {
        // 用真 jmod(已知合法容器)验证缺类返回 Ok(None)(非 Err)。无 JDK 则跳过。
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:本机未找到 java.base.jmod");
            return;
        };
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        assert!(cp.load_class("no/such/Class").unwrap().is_none());
    }

    /// **集成闸门(4.10d)**:从真实 JDK 容器加载 `java/lang/Object`,走 zip → DEFLATE →
    /// `classfile::parse` 全链;断言类名匹配 + 非空方法表(含 native registerNatives/
    /// hashCode 等)。无 JDK jmod 则跳过。
    #[test]
    fn gate_loads_real_object_from_javabase_jmod() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:本机未找到 java.base.jmod");
            return;
        };
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add(
            jmod.file_name().and_then(|n| n.to_str()).unwrap_or("java.base.jmod"),
            &bytes,
        )
        .unwrap();
        let (cf, _module) = cp
            .load_class("java/lang/Object")
            .expect("jmod 读取/解析不应失败")
            .expect("Object.class 须在 java.base.jmod 内");
        assert_eq!(cf.this_class_name(), Some("java/lang/Object"));
        assert!(
            !cf.methods.is_empty(),
            "Object 须有方法(registerNatives/hashCode/<init>/…)"
        );
    }
}
