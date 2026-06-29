//! 对象模型(对应 HotSpot `src/hotspot/share/oops/`)。
//!
//! 4.1:实例对象 + 字段布局。堆(分配)见 [`crate::runtime::heap`],
//! 数组(`typeArrayOop`/`objArrayOop`)留待 4.3。

pub mod array;
pub mod bootstrap;
pub mod class_oop;
pub mod instance;
pub mod klass;
pub mod oop;
pub mod string;

pub use array::ArrayOop;
pub use class_oop::ClassOop;
pub use instance::InstanceOop;
pub use klass::{ClassRegistry, InitState, LoadedClass, ResolvedField};
pub use oop::Oop;
pub use string::StringOop;
