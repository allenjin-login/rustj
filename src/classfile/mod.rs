//! 类文件(.class)二进制解析。
//!
//! 对应 HotSpot `classfile/` 模块:`ClassFileStream`、`ClassFileParser`。

pub mod attributes;
pub mod error;
pub mod parser;
pub mod reader;

pub use error::ClassFileError;
pub use parser::{parse, parse_from_reader, MAGIC};
pub use reader::Reader;
