//! 属性(attribute)解析(JVMS §4.7)。
//!
//! - [`Attribute`]:原始属性(名称索引 + 字节);除 `Code` 外多数属性本层只存原始字节。
//! - [`CodeAttribute`]:对 `Code` 属性的深解析(解释器必需)。

use crate::classfile::{ClassFileError, Reader};

/// 原始属性:`attribute_name_index` 指向常量池中的 Utf8,`info` 为属性体字节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name_index: u16,
    pub info: Vec<u8>,
}

/// `Code` 属性中的异常表条目(JVMS §4.7.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionTableEntry {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    /// 捕获的异常类的常量池索引;0 表示 catch-all(`finally`)。
    pub catch_type: u16,
}

/// `LineNumberTable` 条目(JVMS §4.7.12):`start_pc` 处的字节码归属 `line_number` 源行。
/// 供栈轨迹行号解析(取最大的 `start_pc ≤ bci`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineNumberEntry {
    pub start_pc: u16,
    pub line_number: u16,
}

/// `BootstrapMethods` 属性条目(JVMS §4.7.21):供 `invokedynamic` / `CONSTANT_Dynamic`
/// 解析引导方法。`bootstrap_method_ref` = `CONSTANT_MethodHandle` 常量池索引;
/// `bootstrap_arguments` = 引导方法附加实参的常量池索引(recipe 常为首个 `CONSTANT_String`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapMethodEntry {
    pub bootstrap_method_ref: u16,
    pub bootstrap_arguments: Vec<u16>,
}

/// `Code` 属性深解析结果。解释器执行方法的必需信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeAttribute {
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    pub exception_table: Vec<ExceptionTableEntry>,
    /// `Code` 属性内嵌的属性(如 `LineNumberTable`)。本层只存原始字节。
    pub attributes: Vec<Attribute>,
    /// `LineNumberTable` 子属性解码(JVMS §4.7.12);无该属性则空。供栈轨迹行号解析。
    pub line_number_table: Vec<LineNumberEntry>,
}

/// 读取 `attributes_count` 个原始属性。
pub fn parse_attributes(reader: &mut Reader) -> Result<Vec<Attribute>, ClassFileError> {
    let count = usize::from(reader.u2()?);
    let mut attrs = Vec::with_capacity(count);
    for _ in 0..count {
        let name_index = reader.u2()?;
        let length = usize::try_from(reader.u4()?)
            .map_err(|_| ClassFileError::InvalidAttribute {
                reason: "attribute length overflows usize".to_string(),
            })?;
        let info = reader.take(length)?.to_vec();
        attrs.push(Attribute { name_index, info });
    }
    Ok(attrs)
}

/// 解码 `LineNumberTable` 属性体(JVMS §4.7.12):`u2 length` 后接
/// `{u2 start_pc; u2 line_number}[]`。cp 无关纯解码(属性名识别在 `resolve_code` 经 cp 做)。
pub fn parse_line_number_table(info: &[u8]) -> Result<Vec<LineNumberEntry>, ClassFileError> {
    let mut reader = Reader::new(info);
    let count = usize::from(reader.u2()?);
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        entries.push(LineNumberEntry {
            start_pc: reader.u2()?,
            line_number: reader.u2()?,
        });
    }
    Ok(entries)
}

/// 解码 `BootstrapMethods` 属性体(JVMS §4.7.21):`u2 num_bootstrap_methods` 后接
/// `{ u2 bootstrap_method_ref; u2 num_bootstrap_arguments; u2[num] bootstrap_arguments }[]`。
/// cp 无关纯解码(属性名识别在 `ClassFile::bootstrap_methods` 经 cp 做)。
pub fn parse_bootstrap_methods(info: &[u8]) -> Result<Vec<BootstrapMethodEntry>, ClassFileError> {
    let mut reader = Reader::new(info);
    let count = usize::from(reader.u2()?);
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let bootstrap_method_ref = reader.u2()?;
        let nargs = usize::from(reader.u2()?);
        let mut bootstrap_arguments = Vec::with_capacity(nargs);
        for _ in 0..nargs {
            bootstrap_arguments.push(reader.u2()?);
        }
        entries.push(BootstrapMethodEntry {
            bootstrap_method_ref,
            bootstrap_arguments,
        });
    }
    Ok(entries)
}

/// `Module` 属性的 `requires` 项(JVMS §4.7.25):CP `Module` 索引 + 标志位 + 版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleRequiresEntry {
    pub requires_index: u16,
    /// `ACC_TRANSITIVE`(0x20)/`ACC_STATIC_PHASE`(0x40)/`ACC_SYNTHETIC`(0x1000)/`ACC_MANDATED`(0x8000)。
    pub requires_flags: u16,
    pub requires_version_index: u16,
}

/// `Module` 属性的 `exports` 项:CP `Package` 索引 + 标志位 + 限定导出目标模块(`Module` 索引列表)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleExportsEntry {
    pub exports_index: u16,
    pub exports_flags: u16,
    pub exports_to: Vec<u16>,
}

/// `Module` 属性的 `opens` 项:结构同 [`ModuleExportsEntry`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleOpensEntry {
    pub opens_index: u16,
    pub opens_flags: u16,
    pub opens_to: Vec<u16>,
}

/// `Module` 属性的 `provides` 项:CP `Class`(服务)索引 + 实现类(`Class` 索引列表)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleProvidesEntry {
    pub provides_index: u16,
    pub provides_with: Vec<u16>,
}

/// `Module` 属性深解析结果(JVMS §4.7.25,**常量池索引形式**)。常量池 `Module`/`Package`/`Utf8`
/// 名解析在调用方(`ModuleDescriptor::from_class_file`)经 cp 做——本结构保持 cp 无关纯解码
/// (镜像 `BootstrapMethodEntry`,属性名识别亦在调用方)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleAttribute {
    pub module_name_index: u16,
    /// `ACC_OPEN`(0x20)/`ACC_SYNTHETIC`(0x1000)/`ACC_MANDATED`(0x8000)。
    pub module_flags: u16,
    pub module_version_index: u16,
    pub requires: Vec<ModuleRequiresEntry>,
    pub exports: Vec<ModuleExportsEntry>,
    pub opens: Vec<ModuleOpensEntry>,
    /// CP `Class` 索引(服务接口)。
    pub uses: Vec<u16>,
    pub provides: Vec<ModuleProvidesEntry>,
}

/// 解码 `Module` 属性体字节(JVMS §4.7.25)。cp 无关纯解码;属性名识别在
/// `ModuleDescriptor::from_class_file` 经 cp 做。
pub fn parse_module_attribute(info: &[u8]) -> Result<ModuleAttribute, ClassFileError> {
    let mut reader = Reader::new(info);
    let module_name_index = reader.u2()?;
    let module_flags = reader.u2()?;
    let module_version_index = reader.u2()?;

    let requires_count = usize::from(reader.u2()?);
    let mut requires = Vec::with_capacity(requires_count);
    for _ in 0..requires_count {
        let requires_index = reader.u2()?;
        let requires_flags = reader.u2()?;
        let requires_version_index = reader.u2()?;
        requires.push(ModuleRequiresEntry {
            requires_index,
            requires_flags,
            requires_version_index,
        });
    }

    let exports_count = usize::from(reader.u2()?);
    let mut exports = Vec::with_capacity(exports_count);
    for _ in 0..exports_count {
        let exports_index = reader.u2()?;
        let exports_flags = reader.u2()?;
        let to_count = usize::from(reader.u2()?);
        let mut exports_to = Vec::with_capacity(to_count);
        for _ in 0..to_count {
            exports_to.push(reader.u2()?);
        }
        exports.push(ModuleExportsEntry {
            exports_index,
            exports_flags,
            exports_to,
        });
    }

    let opens_count = usize::from(reader.u2()?);
    let mut opens = Vec::with_capacity(opens_count);
    for _ in 0..opens_count {
        let opens_index = reader.u2()?;
        let opens_flags = reader.u2()?;
        let to_count = usize::from(reader.u2()?);
        let mut opens_to = Vec::with_capacity(to_count);
        for _ in 0..to_count {
            opens_to.push(reader.u2()?);
        }
        opens.push(ModuleOpensEntry {
            opens_index,
            opens_flags,
            opens_to,
        });
    }

    let uses_count = usize::from(reader.u2()?);
    let mut uses = Vec::with_capacity(uses_count);
    for _ in 0..uses_count {
        uses.push(reader.u2()?);
    }

    let provides_count = usize::from(reader.u2()?);
    let mut provides = Vec::with_capacity(provides_count);
    for _ in 0..provides_count {
        let provides_index = reader.u2()?;
        let with_count = usize::from(reader.u2()?);
        let mut provides_with = Vec::with_capacity(with_count);
        for _ in 0..with_count {
            provides_with.push(reader.u2()?);
        }
        provides.push(ModuleProvidesEntry {
            provides_index,
            provides_with,
        });
    }

    Ok(ModuleAttribute {
        module_name_index,
        module_flags,
        module_version_index,
        requires,
        exports,
        opens,
        uses,
        provides,
    })
}

/// 深解析 `Code` 属性体字节。
pub fn parse_code(info: &[u8]) -> Result<CodeAttribute, ClassFileError> {
    let mut reader = Reader::new(info);
    let max_stack = reader.u2()?;
    let max_locals = reader.u2()?;
    let code_length = usize::try_from(reader.u4()?)
        .map_err(|_| ClassFileError::InvalidAttribute {
            reason: "code_length overflows usize".to_string(),
        })?;
    let code = reader.take(code_length)?.to_vec();

    let ex_len = usize::from(reader.u2()?);
    let mut exception_table = Vec::with_capacity(ex_len);
    for _ in 0..ex_len {
        exception_table.push(ExceptionTableEntry {
            start_pc: reader.u2()?,
            end_pc: reader.u2()?,
            handler_pc: reader.u2()?,
            catch_type: reader.u2()?,
        });
    }

    let attributes = parse_attributes(&mut reader)?;

    Ok(CodeAttribute {
        max_stack,
        max_locals,
        code,
        exception_table,
        attributes,
        line_number_table: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_attributes_reads_name_index_and_info() {
        // attributes_count=2
        // attr1: name_index=1, len=2, bytes [0xAA,0xBB]
        // attr2: name_index=2, len=0
        let bytes = [
            0x00, 0x02, // count
            0x00, 0x01, 0x00, 0x00, 0x00, 0x02, 0xAA, 0xBB, // attr1
            0x00, 0x02, 0x00, 0x00, 0x00, 0x00, // attr2
        ];
        let mut r = Reader::new(&bytes);
        let attrs = parse_attributes(&mut r).unwrap();
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].name_index, 1);
        assert_eq!(attrs[0].info, vec![0xAA, 0xBB]);
        assert_eq!(attrs[1].name_index, 2);
        assert!(attrs[1].info.is_empty());
    }

    #[test]
    fn parse_code_decodes_stack_locals_and_bytecode() {
        // max_stack=2, max_locals=1, code_length=3
        // code = iload_0(0x1a) iload_1(0x1b) iadd(0x60)
        // exception_table_length=0, attributes_count=0
        let info = [
            0x00, 0x02, // max_stack
            0x00, 0x01, // max_locals
            0x00, 0x00, 0x00, 0x03, // code_length
            0x1A, 0x1B, 0x60, // code
            0x00, 0x00, // exception_table_length
            0x00, 0x00, // attributes_count
        ];
        let code = parse_code(&info).unwrap();
        assert_eq!(code.max_stack, 2);
        assert_eq!(code.max_locals, 1);
        assert_eq!(code.code, vec![0x1A, 0x1B, 0x60]);
        assert!(code.exception_table.is_empty());
        assert!(code.attributes.is_empty());
    }

    #[test]
    fn parse_code_decodes_exception_table_entry() {
        // max_stack=0, max_locals=0, code_length=0,
        // exception_table_length=1: start=0, end=5, handler=10, catch_type=7
        // attributes_count=0
        let info = [
            0x00, 0x00, 0x00, 0x00, // max_stack, max_locals
            0x00, 0x00, 0x00, 0x00, // code_length
            0x00, 0x01, // exception_table_length
            0x00, 0x00, // start_pc
            0x00, 0x05, // end_pc
            0x00, 0x0A, // handler_pc
            0x00, 0x07, // catch_type
            0x00, 0x00, // attributes_count
        ];
        let code = parse_code(&info).unwrap();
        assert_eq!(
            code.exception_table,
            vec![ExceptionTableEntry {
                start_pc: 0,
                end_pc: 5,
                handler_pc: 10,
                catch_type: 7,
            }]
        );
    }

    #[test]
    fn parse_line_number_table_decodes_start_pc_and_line() {
        // LineNumberTable 体:length=2,then (start_pc=0,line=3)、(start_pc=4,line=7)
        let info = [
            0x00, 0x02, // length
            0x00, 0x00, 0x00, 0x03, // start_pc=0, line=3
            0x00, 0x04, 0x00, 0x07, // start_pc=4, line=7
        ];
        let entries = parse_line_number_table(&info).unwrap();
        assert_eq!(
            entries,
            vec![
                LineNumberEntry { start_pc: 0, line_number: 3 },
                LineNumberEntry { start_pc: 4, line_number: 7 },
            ]
        );
    }

    #[test]
    fn parse_code_rejects_truncated() {
        let info = [0x00, 0x00]; // 只有 max_stack
        assert!(matches!(
            parse_code(&info),
            Err(ClassFileError::Truncated { .. })
        ));
    }

    #[test]
    fn parse_bootstrap_methods_decodes_entries_and_args() {
        // num_bootstrap_methods=2
        // bsm0: ref=#29, num_args=1, args=[#27]
        // bsm1: ref=#40, num_args=0
        let info = [
            0x00, 0x02, // count
            0x00, 0x1D, 0x00, 0x01, 0x00, 0x1B, // bsm0
            0x00, 0x28, 0x00, 0x00, // bsm1
        ];
        let entries = parse_bootstrap_methods(&info).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0],
            BootstrapMethodEntry {
                bootstrap_method_ref: 0x1D,
                bootstrap_arguments: vec![0x1B],
            }
        );
        assert_eq!(
            entries[1],
            BootstrapMethodEntry {
                bootstrap_method_ref: 0x28,
                bootstrap_arguments: vec![],
            }
        );
    }

    #[test]
    fn parse_bootstrap_methods_rejects_truncated() {
        let info = [0x00, 0x01, 0x00, 0x1D]; // count=1 但 ref 后缺 num_args
        assert!(matches!(
            parse_bootstrap_methods(&info),
            Err(ClassFileError::Truncated { .. })
        ));
    }

    #[test]
    fn parse_module_attribute_decodes_requires_exports_uses_provides() {
        // 最小 Module 属性体(JVMS §4.7.25):
        //   module_name_index=6, module_flags=0, module_version_index=0
        //   requires_count=1: { requires_index=10, flags=0x0040(STATIC_PHASE), version=0 }
        //   exports_count=1:   { exports_index=20, flags=0, to_count=0 }
        //   opens_count=0
        //   uses_count=1:    uses_index=30
        //   provides_count=1:{ provides_index=40, with_count=1, with=[50] }
        let info = [
            0x00, 0x06, // module_name_index
            0x00, 0x00, // module_flags
            0x00, 0x00, // module_version_index
            0x00, 0x01, // requires_count
            0x00, 0x0A, 0x00, 0x40, 0x00, 0x00, // requires[0]
            0x00, 0x01, // exports_count
            0x00, 0x14, 0x00, 0x00, 0x00, 0x00, // exports[0]
            0x00, 0x00, // opens_count
            0x00, 0x01, // uses_count
            0x00, 0x1E, // uses[0]
            0x00, 0x01, // provides_count
            0x00, 0x28, 0x00, 0x01, 0x00, 0x32, // provides[0]
        ];
        let m = parse_module_attribute(&info).unwrap();
        assert_eq!(m.module_name_index, 6);
        assert_eq!(m.module_flags, 0);
        assert_eq!(m.module_version_index, 0);
        assert_eq!(m.requires.len(), 1);
        assert_eq!(m.requires[0].requires_index, 10);
        assert_eq!(m.requires[0].requires_flags, 0x0040);
        assert_eq!(m.requires[0].requires_version_index, 0);
        assert_eq!(m.exports.len(), 1);
        assert_eq!(m.exports[0].exports_index, 20);
        assert!(m.exports[0].exports_to.is_empty());
        assert!(m.opens.is_empty());
        assert_eq!(m.uses, vec![30]);
        assert_eq!(m.provides.len(), 1);
        assert_eq!(m.provides[0].provides_index, 40);
        assert_eq!(m.provides[0].provides_with, vec![50]);
    }

    #[test]
    fn parse_module_attribute_decodes_qualified_export() {
        // exports 带 to 列表:exports_index=7, flags=0, to_count=2, to=[11, 13]
        let info = [
            0x00, 0x06, // module_name_index
            0x00, 0x00, // module_flags
            0x00, 0x00, // module_version_index
            0x00, 0x00, // requires_count=0
            0x00, 0x01, // exports_count=1
            0x00, 0x07, 0x00, 0x00, 0x00, 0x02, 0x00, 0x0B, 0x00, 0x0D,
            0x00, 0x00, // opens_count=0
            0x00, 0x00, // uses_count=0
            0x00, 0x00, // provides_count=0
        ];
        let m = parse_module_attribute(&info).unwrap();
        assert_eq!(m.exports.len(), 1);
        assert_eq!(m.exports[0].exports_to, vec![11, 13]);
    }

    #[test]
    fn parse_module_attribute_rejects_truncated() {
        // 仅 module_name_index,缺后续
        let info = [0x00, 0x06];
        assert!(matches!(
            parse_module_attribute(&info),
            Err(ClassFileError::Truncated { .. })
        ));
    }
}
