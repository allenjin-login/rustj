//! 类元数据:字段/方法/类信息。对应 HotSpot `oops/{constMethod,method,instanceKlass}.*`。

pub mod access_flags;
pub mod class_file;
pub mod descriptor;
pub mod field;
pub mod method;
pub mod module;

pub use access_flags::AccessFlags;
pub use class_file::ClassFile;
pub use descriptor::{FieldType, MethodDescriptor, ReturnDescriptor};
pub use field::FieldInfo;
pub use method::MethodInfo;
pub use module::{
    ModuleDescriptor, ModuleExports, ModuleProvides, ModuleRequires,
};
// `CodeAttribute` / `ExceptionTableEntry` 定义在 `crate::classfile::attributes`,
// 该处即为它们的规范导出位置。
