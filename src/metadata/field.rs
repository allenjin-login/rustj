//! 字段信息(JVMS §4.5)。对应 HotSpot `fieldInfo.*`。

use crate::classfile::attributes::{parse_attributes, Attribute};
use crate::classfile::{ClassFileError, Reader};

use super::access_flags::AccessFlags;

/// 一个字段。本层只解析结构与原始属性;`ConstantValue` 等深解析留给后续层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldInfo {
    pub access_flags: AccessFlags,
    pub name_index: u16,
    pub descriptor_index: u16,
    pub attributes: Vec<Attribute>,
}

impl FieldInfo {
    /// 从读取器解析一个字段。
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::access_flags::{ACC_PRIVATE, ACC_STATIC};

    #[test]
    fn parses_field_with_no_attributes() {
        // access=PRIVATE|STATIC, name_index=5, descriptor_index=6, attributes_count=0
        let bytes = [0x00, 0x0A, 0x00, 0x05, 0x00, 0x06, 0x00, 0x00];
        let mut r = Reader::new(&bytes);
        let f = FieldInfo::parse(&mut r).unwrap();
        assert_eq!(f.access_flags.bits(), ACC_PRIVATE | ACC_STATIC);
        assert_eq!(f.name_index, 5);
        assert_eq!(f.descriptor_index, 6);
        assert!(f.attributes.is_empty());
    }
}
