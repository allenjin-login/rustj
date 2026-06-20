//! 类文件解析错误类型。库内全程用 `Result<_, ClassFileError>` 传播,不 panic。

use std::fmt;

/// 解析 `.class` 时可能遇到的所有结构化错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassFileError {
    /// 魔数不是 0xCAFEBABE。
    BadMagic { actual: u32 },
    /// 数据被截断:还需要 `needed` 字节,但只剩 `remaining`。
    Truncated { needed: usize, remaining: usize },
    /// 未知/非法的常量池标签字节。
    InvalidConstantPoolTag(u8),
    /// 常量池索引越界。
    BadConstantPoolIndex { index: u16, length: u16 },
    /// 修改版 UTF-8(JVMS §4.4.7)解码失败。
    InvalidUtf8,
    /// access_flags 中出现非法位。
    InvalidAccessFlags { flags: u16, context: &'static str },
    /// 不支持的 class 文件版本。
    UnsupportedClassVersion { major: u16, minor: u16 },
    /// 属性内容非法。
    InvalidAttribute { reason: String },
    /// 字段/方法描述符非法。
    InvalidDescriptor { descriptor: String },
    /// 其余尚未支持的特性(占位,便于渐进迁移)。
    Unsupported(&'static str),
}

impl fmt::Display for ClassFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic { actual } => write!(
                f,
                "bad magic: expected 0xCAFEBABE, got 0x{actual:08X}"
            ),
            Self::Truncated { needed, remaining } => write!(
                f,
                "truncated: needed {needed} bytes, only {remaining} remaining"
            ),
            Self::InvalidConstantPoolTag(t) => {
                write!(f, "invalid constant pool tag: {t} (0x{t:02X})")
            }
            Self::BadConstantPoolIndex { index, length } => {
                write!(f, "constant pool index {index} out of range (len {length})")
            }
            Self::InvalidUtf8 => write!(f, "invalid modified UTF-8"),
            Self::InvalidAccessFlags { flags, context } => {
                write!(f, "invalid {context} access flags: 0x{flags:04X}")
            }
            Self::UnsupportedClassVersion { major, minor } => {
                write!(f, "unsupported class version: {major}.{minor}")
            }
            Self::InvalidAttribute { reason } => write!(f, "invalid attribute: {reason}"),
            Self::InvalidDescriptor { descriptor } => {
                write!(f, "invalid descriptor: {descriptor}")
            }
            Self::Unsupported(what) => write!(f, "unsupported: {what}"),
        }
    }
}

impl std::error::Error for ClassFileError {}
