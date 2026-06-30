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
}
