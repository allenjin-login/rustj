//! 对象指针(oop):堆中对象的标记联合(对应 HotSpot `oopDesc`)。
//!
//! 4.1 仅实例对象;数组变体(`TypeArray`/`ObjArray`)留待 4.3。

use super::instance::InstanceOop;

/// 堆中的对象。
#[derive(Debug, Clone, PartialEq)]
pub enum Oop {
    /// 对象实例。
    Instance(InstanceOop),
}
