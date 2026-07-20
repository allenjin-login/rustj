//! 常量池条目解析公用工具(VM 内部,跨 field/invoke 共享)。
//!
//! 提取自 `field.rs:48-74`/`invoke.rs:988-1014` 各自私有定义的逐字重复。

use crate::constant_pool::{ConstantPool, ConstantPoolEntry};

use super::VmError;

/// 取 `Utf8` 条目的字符串(owned)。
pub(crate) fn utf8(cp: &ConstantPool, index: u16) -> Result<String, VmError> {
    match cp.get(index)? {
        ConstantPoolEntry::Utf8(s) => Ok(s.clone()),
        _ => Err(VmError::BadConstant("期望 Utf8 条目")),
    }
}

/// 解析 `Class` 条目 → 类内部名。
pub(crate) fn class_name(cp: &ConstantPool, class_index: u16) -> Result<String, VmError> {
    let ConstantPoolEntry::Class { name_index } = cp.get(class_index)?
    else {
        return Err(VmError::BadConstant("常量池条目须为 Class"));
    };
    utf8(cp, *name_index)
}

/// 解析 `NameAndType` 条目 → `(名字, 描述符)`。
pub(crate) fn name_and_type(cp: &ConstantPool, index: u16) -> Result<(String, String), VmError> {
    let ConstantPoolEntry::NameAndType {
        name_index,
        descriptor_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("常量池条目须含 NameAndType"));
    };
    Ok((utf8(cp, *name_index)?, utf8(cp, *descriptor_index)?))
}

#[cfg(test)]
mod tests {
    use crate::classfile::Reader;
    use crate::constant_pool::ConstantPool;

    /// [1]Utf8"Pt" [2]Class{1} [3]Utf8"x" [4]Utf8"I" [5]NameAndType{3,4}
    fn cp_with_names() -> ConstantPool {
        let bytes = [
            0x00, 0x06, // count=6 (indices 1-5 valid)
            0x01, 0x00, 0x02, b'P', b't', // [1] "Pt"
            0x07, 0x00, 0x01, // [2] Class{1}
            0x01, 0x00, 0x01, b'x', // [3] "x"
            0x01, 0x00, 0x01, b'I', // [4] "I"
            0x0C, 0x00, 0x03, 0x00, 0x04, // [5] NameAndType{3,4}
        ];
        ConstantPool::parse(&mut Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn utf8_decodes_string_entry() {
        let cp = cp_with_names();
        assert_eq!(super::utf8(&cp, 1).unwrap(), "Pt");
    }

    #[test]
    fn class_name_decodes_class_entry() {
        let cp = cp_with_names();
        assert_eq!(super::class_name(&cp, 2).unwrap(), "Pt");
    }

    #[test]
    fn name_and_type_decodes_name_and_descriptor() {
        let cp = cp_with_names();
        let (n, d) = super::name_and_type(&cp, 5).unwrap();
        assert_eq!(n, "x");
        assert_eq!(d, "I");
    }
}
