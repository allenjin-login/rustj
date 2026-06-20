//! 常量池:对应 HotSpot `oops/constantPool.*`。
//!
//! - [`tag::ConstantTag`]:JVMS §4.4 标签种类。
//! - [`entry::ConstantPoolEntry`]:owned 数据的带标签条目。
//! - [`ConstantPool`]:已解析常量池容器,1-based 索引。

pub mod entry;
pub mod pool;
pub mod tag;

pub use entry::ConstantPoolEntry;
pub use pool::ConstantPool;
pub use tag::ConstantTag;
