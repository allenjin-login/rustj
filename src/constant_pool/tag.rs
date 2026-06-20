//! 常量池标签(JVMS §4.4)。

use crate::classfile::ClassFileError;

/// 常量池条目的种类。判别值即 class 文件中的标签字节。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantTag {
    Utf8 = 1,
    Integer = 3,
    Float = 4,
    Long = 5,
    Double = 6,
    Class = 7,
    String = 8,
    Fieldref = 9,
    Methodref = 10,
    InterfaceMethodref = 11,
    NameAndType = 12,
    MethodHandle = 15,
    MethodType = 16,
    Dynamic = 17,
    InvokeDynamic = 18,
    Module = 19,
    Package = 20,
}

impl ConstantTag {
    /// 由 class 文件中的标签字节构造;非法字节返回错误。
    pub fn from_u8(b: u8) -> Result<Self, ClassFileError> {
        Ok(match b {
            1 => Self::Utf8,
            3 => Self::Integer,
            4 => Self::Float,
            5 => Self::Long,
            6 => Self::Double,
            7 => Self::Class,
            8 => Self::String,
            9 => Self::Fieldref,
            10 => Self::Methodref,
            11 => Self::InterfaceMethodref,
            12 => Self::NameAndType,
            15 => Self::MethodHandle,
            16 => Self::MethodType,
            17 => Self::Dynamic,
            18 => Self::InvokeDynamic,
            19 => Self::Module,
            20 => Self::Package,
            other => return Err(ClassFileError::InvalidConstantPoolTag(other)),
        })
    }

    /// `Long`/`Double` 在常量池中各占两个槽位,其余占一个。
    pub fn slot_count(self) -> usize {
        match self {
            Self::Long | Self::Double => 2,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_all_valid_tag_bytes() {
        assert_eq!(ConstantTag::from_u8(1).unwrap(), ConstantTag::Utf8);
        assert_eq!(ConstantTag::from_u8(12).unwrap(), ConstantTag::NameAndType);
        assert_eq!(ConstantTag::from_u8(18).unwrap(), ConstantTag::InvokeDynamic);
        assert_eq!(ConstantTag::from_u8(20).unwrap(), ConstantTag::Package);
    }

    #[test]
    fn rejects_unknown_tag_bytes() {
        assert_eq!(
            ConstantTag::from_u8(2).unwrap_err(),
            ClassFileError::InvalidConstantPoolTag(2)
        );
        assert_eq!(
            ConstantTag::from_u8(13).unwrap_err(),
            ClassFileError::InvalidConstantPoolTag(13)
        );
    }

    #[test]
    fn long_and_double_take_two_slots() {
        assert_eq!(ConstantTag::Long.slot_count(), 2);
        assert_eq!(ConstantTag::Double.slot_count(), 2);
        assert_eq!(ConstantTag::Utf8.slot_count(), 1);
        assert_eq!(ConstantTag::Methodref.slot_count(), 1);
    }
}
