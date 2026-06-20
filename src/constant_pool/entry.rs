//! 常量池条目:带标签的、owned 数据的枚举。

use super::tag::ConstantTag;

/// 常量池中的一个条目。
///
/// 采用 owned `String` 而非 HotSpot 的 `Symbol*` intern,使本层无 unsafe、无生命周期纠缠。
/// 将来解释器层需要符号相等比较时,可在不破坏本类型 API 的前提下替换内部表示。
#[derive(Debug, Clone, PartialEq)]
pub enum ConstantPoolEntry {
    /// JVM_CONSTANT_Utf8
    Utf8(String),
    /// JVM_CONSTANT_Integer
    Integer(i32),
    /// JVM_CONSTANT_Float
    Float(f32),
    /// JVM_CONSTANT_Long(占用其后一个槽位)
    Long(i64),
    /// JVM_CONSTANT_Double(占用其后一个槽位)
    Double(f64),
    /// JVM_CONSTANT_Class
    Class { name_index: u16 },
    /// JVM_CONSTANT_String
    String { string_index: u16 },
    /// JVM_CONSTANT_Fieldref
    Fieldref {
        class_index: u16,
        name_and_type_index: u16,
    },
    /// JVM_CONSTANT_Methodref
    Methodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    /// JVM_CONSTANT_InterfaceMethodref
    InterfaceMethodref {
        class_index: u16,
        name_and_type_index: u16,
    },
    /// JVM_CONSTANT_NameAndType
    NameAndType {
        name_index: u16,
        descriptor_index: u16,
    },
    /// JVM_CONSTANT_MethodHandle
    MethodHandle {
        reference_kind: u8,
        reference_index: u16,
    },
    /// JVM_CONSTANT_MethodType
    MethodType { descriptor_index: u16 },
    /// JVM_CONSTANT_Dynamic
    Dynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    /// JVM_CONSTANT_InvokeDynamic
    InvokeDynamic {
        bootstrap_method_attr_index: u16,
        name_and_type_index: u16,
    },
    /// JVM_CONSTANT_Module
    Module { name_index: u16 },
    /// JVM_CONSTANT_Package
    Package { name_index: u16 },
    /// `Long`/`Double` 占据的第二个槽位,不可用。仅为保持 1-based 索引正确。
    Unusable,
}

impl ConstantPoolEntry {
    /// 该条目对应的标签。
    pub fn tag(&self) -> ConstantTag {
        match self {
            Self::Utf8(_) => ConstantTag::Utf8,
            Self::Integer(_) => ConstantTag::Integer,
            Self::Float(_) => ConstantTag::Float,
            Self::Long(_) => ConstantTag::Long,
            Self::Double(_) => ConstantTag::Double,
            Self::Class { .. } => ConstantTag::Class,
            Self::String { .. } => ConstantTag::String,
            Self::Fieldref { .. } => ConstantTag::Fieldref,
            Self::Methodref { .. } => ConstantTag::Methodref,
            Self::InterfaceMethodref { .. } => ConstantTag::InterfaceMethodref,
            Self::NameAndType { .. } => ConstantTag::NameAndType,
            Self::MethodHandle { .. } => ConstantTag::MethodHandle,
            Self::MethodType { .. } => ConstantTag::MethodType,
            Self::Dynamic { .. } => ConstantTag::Dynamic,
            Self::InvokeDynamic { .. } => ConstantTag::InvokeDynamic,
            Self::Module { .. } => ConstantTag::Module,
            Self::Package { .. } => ConstantTag::Package,
            Self::Unusable => ConstantTag::Utf8, // 占位,不会被用到
        }
    }

    /// 占用的槽位数。`Unusable` 自身不算独立逻辑条目。
    pub fn slot_count(&self) -> usize {
        match self {
            Self::Long(_) | Self::Double(_) => 2,
            Self::Unusable => 0,
            _ => 1,
        }
    }
}
