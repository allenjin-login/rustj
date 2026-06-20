//! 对象实例(对应 HotSpot `instanceOop`)。
//!
//! 4.1:实例字段按声明序每字段一个 [`Slot`](long/double 也只占一槽;
//! 类型在 `getfield`/`putfield` 指令边界按描述符转换)。

use crate::runtime::Slot;

/// 一个对象实例:所属类内部名 + 实例字段槽位数组(按声明序,每字段一槽)。
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceOop {
    class_name: String,
    fields: Vec<Slot>,
}

impl InstanceOop {
    /// 由所属类名与初始字段槽构造。
    pub(crate) fn new(class_name: String, fields: Vec<Slot>) -> Self {
        Self {
            class_name,
            fields,
        }
    }

    /// 取实例字段槽(按声明序的序号)。
    pub fn field(&self, ordinal: usize) -> Slot {
        self.fields[ordinal]
    }

    /// 写实例字段槽。
    pub fn set_field(&mut self, ordinal: usize, slot: Slot) {
        self.fields[ordinal] = slot;
    }

    /// 所属类的内部名。
    pub fn class_name(&self) -> &str {
        &self.class_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Slot;

    #[test]
    fn field_get_set_round_trip() {
        let mut o = InstanceOop::new("P".into(), vec![Slot::Int(0), Slot::Long(0)]);
        // 初始默认值
        assert_eq!(o.field(0), Slot::Int(0));
        assert_eq!(o.field(1), Slot::Long(0));
        // 写后读回
        o.set_field(0, Slot::Int(42));
        assert_eq!(o.field(0), Slot::Int(42));
        o.set_field(1, Slot::Long(99));
        assert_eq!(o.field(1), Slot::Long(99));
        assert_eq!(o.class_name(), "P");
    }
}
