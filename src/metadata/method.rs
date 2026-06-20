//! 方法信息(JVMS §4.6)与 `Code` 属性解析。对应 HotSpot `method.*` / `constMethod.*`。

use crate::classfile::attributes::{parse_attributes, parse_code, Attribute, CodeAttribute};
use crate::classfile::{ClassFileError, Reader};
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};

use super::access_flags::AccessFlags;

/// 一个方法。
///
/// `code` 在解析常量池后通过 [`MethodInfo::resolve_code`] 填充,
/// 以保持本结构对常量池的解耦(`Code` 属性是按名称在常量池中识别的)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodInfo {
    pub access_flags: AccessFlags,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub attributes: Vec<Attribute>,
    /// 深解析后的 `Code` 属性;抽象方法/原生方法为 `None`。
    pub code: Option<CodeAttribute>,
}

impl MethodInfo {
    /// 从读取器解析一个方法的原始结构(不含 `Code` 深解析)。
    pub fn parse(reader: &mut Reader) -> Result<Self, ClassFileError> {
        let access_flags = AccessFlags::from_bits(reader.u2()?);
        let name_index = reader.u2()?;
        let descriptor_index = reader.u2()?;
        let attributes = parse_attributes(reader)?;
        Ok(Self {
            access_flags,
            name_index,
            descriptor_index,
            attributes,
            code: None,
        })
    }

    /// 在常量池中查找名为 `"Code"` 的属性并深解析。
    /// 找不到则保持 `code = None`(抽象/原生方法)。
    pub fn resolve_code(&mut self, cp: &ConstantPool) -> Result<(), ClassFileError> {
        for attr in &self.attributes {
            if let ConstantPoolEntry::Utf8(name) = cp.get(attr.name_index)?
                && name == "Code"
            {
                self.code = Some(parse_code(&attr.info)?);
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::access_flags::ACC_PUBLIC;

    #[test]
    fn parses_method_raw_without_resolving_code() {
        // access=PUBLIC, name_index=1, descriptor_index=2, attributes_count=0
        let bytes = [0x00, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00];
        let mut r = Reader::new(&bytes);
        let m = MethodInfo::parse(&mut r).unwrap();
        assert_eq!(m.access_flags.bits(), ACC_PUBLIC);
        assert_eq!(m.name_index, 1);
        assert_eq!(m.descriptor_index, 2);
        assert!(m.attributes.is_empty());
        assert!(m.code.is_none());
    }

    /// 构造常量池:[1]=Utf8("map"), [2]=Utf8("()I"), [3]=Utf8("Code")
    fn pool_with_code_name() -> ConstantPool {
        let cp_bytes = [
            0x00, 0x04, // count
            0x01, 0x00, 0x03, b'm', b'a', b'p', // [1] "map"
            0x01, 0x00, 0x03, b'(', b')', b'I', // [2] "()I"
            0x01, 0x00, 0x04, b'C', b'o', b'd', b'e', // [3] "Code"
        ];
        let mut r = Reader::new(&cp_bytes);
        ConstantPool::parse(&mut r).unwrap()
    }

    #[test]
    fn resolve_code_deep_parses_named_code_attribute() {
        let cp = pool_with_code_name();

        // Code 属性体:max_stack=2, max_locals=1, code=[1a 1b 60], 无异常表/属性
        let code_info = [
            0x00, 0x02, 0x00, 0x01, // max_stack, max_locals
            0x00, 0x00, 0x00, 0x03, 0x1A, 0x1B, 0x60, // code_length + code
            0x00, 0x00, // exception_table_length
            0x00, 0x00, // attributes_count
        ];
        // 方法:access=PUBLIC|STATIC, name=1, desc=2, attributes_count=1, attr{name=3,len,info}
        let mut bytes = vec![0x00, 0x09, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01];
        bytes.extend_from_slice(&3u16.to_be_bytes()); // attr name_index = 3
        bytes.extend_from_slice(&(code_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&code_info);

        let mut r = Reader::new(&bytes);
        let mut m = MethodInfo::parse(&mut r).unwrap();
        assert!(m.code.is_none());
        m.resolve_code(&cp).unwrap();

        let code = m.code.expect("code should be resolved");
        assert_eq!(code.max_stack, 2);
        assert_eq!(code.max_locals, 1);
        assert_eq!(code.code, vec![0x1A, 0x1B, 0x60]);
    }

    #[test]
    fn resolve_code_leaves_abstract_method_without_code() {
        let cp = pool_with_code_name();
        // 无任何属性的方法;access = PUBLIC|ABSTRACT = 0x0401
        let bytes = [0x04, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00];
        let mut r = Reader::new(&bytes);
        let mut m = MethodInfo::parse(&mut r).unwrap();
        m.resolve_code(&cp).unwrap();
        assert!(m.code.is_none());
    }

    #[test]
    fn ignores_non_code_attributes_during_resolve() {
        let cp = pool_with_code_name();
        // 一个名为 [1]="map" 的非 Code 属性(空 info)
        let mut bytes = vec![0x00, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01];
        bytes.extend_from_slice(&1u16.to_be_bytes()); // name_index=1 ("map")
        bytes.extend_from_slice(&0u32.to_be_bytes()); // length=0
        let mut r = Reader::new(&bytes);
        let mut m = MethodInfo::parse(&mut r).unwrap();
        m.resolve_code(&cp).unwrap();
        assert!(m.code.is_none()); // 没有 Code 属性
    }
}
