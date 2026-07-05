//! 模块描述符(JVMS §4.7.25 的 `Module` 属性高层视图)。
//!
//! 从 [`ClassFile`] 的 `Module` 属性经 [`parse_module_attribute`](crate::classfile::attributes::parse_module_attribute)
//! 取得**常量池索引形式**的 [`ModuleAttribute`],再经 cp 把各 `Module`/`Package`/`Class` 索引解析
//! 为 owned 内部名 → [`ModuleDescriptor`]。对应 `java.lang.module.ModuleDescriptor`(JDK 侧)。
//!
//! 仅解析(JVMS §4.7.25);模块图解析(`Configuration`)/可读性边(`addReads0`)/运行期 `Module`
//! 对象在后续层(模块系统集成)。

use crate::classfile::attributes::{parse_module_attribute, ModuleAttribute};
use crate::classfile::ClassFileError;
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::ClassFile;

/// 一条 `requires`:被依赖模块名 + 标志位(`ACC_TRANSITIVE`/`ACC_STATIC_PHASE`/`ACC_SYNTHETIC`/
/// `ACC_MANDATED`)+ 可选版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleRequires {
    pub name: String,
    pub flags: u16,
    pub version: Option<String>,
}

/// 一条 `exports`(或 `opens`):包名(`java/lang` 形式)+ 标志位 + 限定导出/开放目标模块名列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExports {
    pub package: String,
    pub flags: u16,
    pub to_modules: Vec<String>,
}

/// 一条 `provides`:服务接口内部名 + 实现类内部名列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleProvides {
    pub service: String,
    pub implementations: Vec<String>,
}

/// 模块描述符:模块名 + 标志位(`ACC_OPEN`/`ACC_SYNTHETIC`/`ACC_MANDATED`)+ 版本 + requires/
/// exports/opens/uses/provides。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDescriptor {
    name: String,
    flags: u16,
    version: Option<String>,
    requires: Vec<ModuleRequires>,
    exports: Vec<ModuleExports>,
    opens: Vec<ModuleExports>,
    uses: Vec<String>,
    provides: Vec<ModuleProvides>,
}

impl ModuleDescriptor {
    /// 模块名(如 `java.base`)。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 模块标志位。
    pub fn flags(&self) -> u16 {
        self.flags
    }

    /// 模块版本(如 `25.0.2`)。
    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// `requires` 列表。
    pub fn requires(&self) -> &[ModuleRequires] {
        &self.requires
    }

    /// `exports` 列表。
    pub fn exports(&self) -> &[ModuleExports] {
        &self.exports
    }

    /// `opens` 列表(结构同 exports)。
    pub fn opens(&self) -> &[ModuleExports] {
        &self.opens
    }

    /// `uses` 列表(服务接口内部名)。
    pub fn uses(&self) -> &[String] {
        &self.uses
    }

    /// `provides` 列表。
    pub fn provides(&self) -> &[ModuleProvides] {
        &self.provides
    }

    /// 从 `module-info.class` 的 [`ClassFile`] 解析模块描述符。
    ///
    /// 返回 `Ok(None)` = 该类文件无 `Module` 属性(非 `module-info`);`Err` = 属性体损坏或
    /// 常量池索引非法。
    pub fn from_class_file(cf: &ClassFile) -> Result<Option<Self>, ClassFileError> {
        let cp = &cf.constant_pool;
        // 扫类属性,经 cp 识别名为 "Module" 的属性体。
        let mut module_info: Option<&[u8]> = None;
        for attr in &cf.attributes {
            let Ok(ConstantPoolEntry::Utf8(name)) = cp.get(attr.name_index) else {
                continue;
            };
            if name == "Module" {
                module_info = Some(&attr.info);
                break;
            }
        }
        let Some(info) = module_info else {
            return Ok(None);
        };
        let m = parse_module_attribute(info)?;
        Ok(Some(Self::resolve(&m, cp)?))
    }

    /// 把常量池索引形式的 [`ModuleAttribute`] 解析为 owned 名字形式。
    fn resolve(m: &ModuleAttribute, cp: &ConstantPool) -> Result<Self, ClassFileError> {
        let name = cp_module_name(cp, m.module_name_index)?;
        let version = if m.module_version_index == 0 {
            None
        } else {
            Some(cp_utf8(cp, m.module_version_index)?)
        };

        let mut requires = Vec::with_capacity(m.requires.len());
        for r in &m.requires {
            requires.push(ModuleRequires {
                name: cp_module_name(cp, r.requires_index)?,
                flags: r.requires_flags,
                version: if r.requires_version_index == 0 {
                    None
                } else {
                    Some(cp_utf8(cp, r.requires_version_index)?)
                },
            });
        }

        let exports = resolve_pkg_entries(
            m.exports
                .iter()
                .map(|e| (e.exports_index, e.exports_flags, &e.exports_to[..])),
            cp,
        )?;
        let opens = resolve_pkg_entries(
            m.opens
                .iter()
                .map(|e| (e.opens_index, e.opens_flags, &e.opens_to[..])),
            cp,
        )?;

        let mut uses = Vec::with_capacity(m.uses.len());
        for &u in &m.uses {
            uses.push(cp_class_name(cp, u)?);
        }

        let mut provides = Vec::with_capacity(m.provides.len());
        for p in &m.provides {
            let mut impls = Vec::with_capacity(p.provides_with.len());
            for &w in &p.provides_with {
                impls.push(cp_class_name(cp, w)?);
            }
            provides.push(ModuleProvides {
                service: cp_class_name(cp, p.provides_index)?,
                implementations: impls,
            });
        }

        Ok(Self {
            name,
            flags: m.module_flags,
            version,
            requires,
            exports,
            opens,
            uses,
            provides,
        })
    }
}

/// 解析 exports/opens 列表(两者结构同形:`(索引, 标志, to 列表)`),经 cp 解 Package→模块名。
fn resolve_pkg_entries<'a, I>(
    entries: I,
    cp: &ConstantPool,
) -> Result<Vec<ModuleExports>, ClassFileError>
where
    I: IntoIterator<Item = (u16, u16, &'a [u16])>,
{
    let mut out = Vec::new();
    for (pkg_index, flags, to) in entries {
        let mut to_modules = Vec::with_capacity(to.len());
        for &t in to {
            to_modules.push(cp_module_name(cp, t)?);
        }
        out.push(ModuleExports {
            package: cp_package_name(cp, pkg_index)?,
            flags,
            to_modules,
        });
    }
    Ok(out)
}

/// cp `Utf8` 索引 → owned 名字。
fn cp_utf8(cp: &ConstantPool, idx: u16) -> Result<String, ClassFileError> {
    match cp.get(idx)? {
        ConstantPoolEntry::Utf8(s) => Ok(s.clone()),
        _ => Err(ClassFileError::Unsupported("期望 CONSTANT_Utf8")),
    }
}

/// cp `Module` 索引 → 模块名(Module→name_index→Utf8)。
fn cp_module_name(cp: &ConstantPool, idx: u16) -> Result<String, ClassFileError> {
    match cp.get(idx)? {
        ConstantPoolEntry::Module { name_index } => cp_utf8(cp, *name_index),
        _ => Err(ClassFileError::Unsupported("期望 CONSTANT_Module")),
    }
}

/// cp `Package` 索引 → 包名(Package→name_index→Utf8)。
fn cp_package_name(cp: &ConstantPool, idx: u16) -> Result<String, ClassFileError> {
    match cp.get(idx)? {
        ConstantPoolEntry::Package { name_index } => cp_utf8(cp, *name_index),
        _ => Err(ClassFileError::Unsupported("期望 CONSTANT_Package")),
    }
}

/// cp `Class` 索引 → 类内部名(Class→name_index→Utf8)。
fn cp_class_name(cp: &ConstantPool, idx: u16) -> Result<String, ClassFileError> {
    match cp.get(idx)? {
        ConstantPoolEntry::Class { name_index } => cp_utf8(cp, *name_index),
        _ => Err(ClassFileError::Unsupported("期望 CONSTANT_Class")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classfile::Reader;
    use crate::constant_pool::ConstantPool;

    /// 构造常量池:`utf8s`(从 1 起)→ `Module`s(各指 utf8)→ `Package`s → `Class`es。
    /// 类似 klass.rs 的 mk_cp 但同时支持 Module/Package/Class 三种条目。
    fn mk_cp_module(utf8s: &[&str], modules: &[u16], packages: &[u16], classes: &[u16]) -> ConstantPool {
        let count = (utf8s.len() + modules.len() + packages.len() + classes.len() + 1) as u16;
        let mut b = count.to_be_bytes().to_vec();
        for s in utf8s {
            b.push(0x01); // Utf8
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        for &ni in modules {
            b.push(0x13); // Module (19)
            b.extend_from_slice(&ni.to_be_bytes());
        }
        for &ni in packages {
            b.push(0x14); // Package (20)
            b.extend_from_slice(&ni.to_be_bytes());
        }
        for &ni in classes {
            b.push(0x07); // Class (7)
            b.extend_from_slice(&ni.to_be_bytes());
        }
        ConstantPool::parse(&mut Reader::new(&b)).unwrap()
    }

    /// 一个最小 module-info 的 ClassFile:CP 含 module "m"、requires "base"、export "p"。
    fn mk_module_cf() -> ClassFile {
        use crate::metadata::{AccessFlags, ClassFile as Cf};
        // utf8: [1]="module-info" [2]="Module" [3]="m" [4]="base" [5]="p"
        // Module: [6]→3 "m", [7]→4 "base"
        // Package: [8]→5 "p"
        // Class(this): [9]→1 "module-info"
        let cp = mk_cp_module(
            &["module-info", "Module", "m", "base", "p"],
            &[3, 4],   // [6]=Module"m", [7]=Module"base"
            &[5],      // [8]=Package"p"
            &[1],      // [9]=Class"module-info"
        );
        // Module 属性体:name=[6], flags=0, version=0
        //   requires=1: { [7], 0x40(static), 0 }
        //   exports=1: { [8], 0, to=0 }
        //   opens=0, uses=0, provides=0
        let module_info: Vec<u8> = vec![
            0x00, 0x06, // module_name_index=[6]
            0x00, 0x00, // flags
            0x00, 0x00, // version=0
            0x00, 0x01, // requires_count=1
            0x00, 0x07, 0x00, 0x40, 0x00, 0x00,
            0x00, 0x01, // exports_count=1
            0x00, 0x08, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, // opens_count=0
            0x00, 0x00, // uses_count=0
            0x00, 0x00, // provides_count=0
        ];
        Cf {
            minor_version: 0,
            major_version: 69,
            constant_pool: cp,
            access_flags: AccessFlags::from_bits(0x8000), // ACC_MODULE
            this_class: 9,
            super_class: 0,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: vec![crate::classfile::attributes::Attribute { name_index: 2, info: module_info }],
        }
    }

    #[test]
    fn from_class_file_resolves_module_requires_and_exports() {
        let cf = mk_module_cf();
        let desc = ModuleDescriptor::from_class_file(&cf).unwrap().unwrap();
        assert_eq!(desc.name(), "m");
        assert_eq!(desc.requires().len(), 1);
        assert_eq!(desc.requires()[0].name, "base");
        assert_eq!(desc.requires()[0].flags, 0x0040);
        assert_eq!(desc.requires()[0].version, None);
        assert_eq!(desc.exports().len(), 1);
        assert_eq!(desc.exports()[0].package, "p");
        assert!(desc.exports()[0].to_modules.is_empty());
    }

    #[test]
    fn from_class_file_none_without_module_attribute() {
        use crate::metadata::{AccessFlags, ClassFile as Cf};
        let cp = mk_cp_module(&["module-info"], &[], &[], &[1]);
        let cf = Cf {
            minor_version: 0,
            major_version: 69,
            constant_pool: cp,
            access_flags: AccessFlags::from_bits(0x8000),
            this_class: 1,
            super_class: 0,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(), // 无 Module 属性
        };
        assert!(ModuleDescriptor::from_class_file(&cf).unwrap().is_none());
    }
}
