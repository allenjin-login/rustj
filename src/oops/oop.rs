//! 对象指针(oop):堆中对象的标记联合(对应 HotSpot `oopDesc`)。
//!
//! 4.1 实例对象;4.3 增一维数组(统一 [`ArrayOop`])。多维数组顺延。
//! 4.10i:字符串不再有特殊变体——`java/lang/String` 即普通 `Instance`(实例字段
//! `value: byte[]` + `coder: byte`),其方法跑真字节码。见
//! `runtime/interpreter/string`。

use super::array::ArrayOop;
use super::class_oop::ClassOop;
use super::instance::InstanceOop;

/// 堆中的对象。
#[derive(Debug, Clone, PartialEq)]
pub enum Oop {
    /// 对象实例。
    Instance(InstanceOop),
    /// 一维数组(基本类型或引用类型,统一表示)。
    Array(ArrayOop),
    /// Class 镜像对象(4.10g:`Class.getPrimitiveClass` 等的返回值载体,见 [`ClassOop`])。
    Class(ClassOop),
}
