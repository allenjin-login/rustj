//! 栈帧槽位与运行时值类型。

/// 一个栈帧槽位(JVMS §2.6.1 局部变量 / §2.6.2 操作数栈)。
///
/// long/double 为 category-2 类型,占**两个连续槽位**:第一个持有完整值
/// (`Long`/`Double`),第二个为 [`Slot::Top`] 占位,仅用于保持索引/深度正确。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Slot {
    Int(i32),
    Float(f32),
    Long(i64),
    Double(f64),
    Reference(Reference),
    /// `ret` 指令使用的返回地址(字节码偏移)。
    ReturnAddress(u16),
    /// long/double 的第二个槽位,或未初始化槽。
    Top,
}

impl Slot {
    /// 是否为 category-2 类型(long/double,占两槽)。
    pub const fn is_category_2(self) -> bool {
        matches!(self, Self::Long(_) | Self::Double(_))
    }
}

/// 对象引用。`None` 表示 `null`。
///
/// 本层为不透明句柄(`u32` id),与堆解耦;Layer 4 的堆将赋予其真实含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reference(Option<u32>);

impl Reference {
    /// null 引用。
    pub const fn null() -> Self {
        Self(None)
    }

    /// 由堆分配的 id 构造。
    pub const fn from_id(id: u32) -> Self {
        Self(Some(id))
    }

    /// 是否为 null。
    pub const fn is_null(self) -> bool {
        self.0.is_none()
    }

    /// 底层 id;null 返回 `None`。
    pub const fn id(self) -> Option<u32> {
        self.0
    }
}

impl Default for Reference {
    fn default() -> Self {
        Self::null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_detection() {
        assert!(Slot::Long(5).is_category_2());
        assert!(Slot::Double(1.0).is_category_2());
        assert!(!Slot::Int(5).is_category_2());
        assert!(!Slot::Float(1.0).is_category_2());
        assert!(!Slot::Top.is_category_2());
    }

    #[test]
    fn reference_null_round_trip() {
        let n = Reference::null();
        assert!(n.is_null());
        assert_eq!(n.id(), None);
        let r = Reference::from_id(42);
        assert!(!r.is_null());
        assert_eq!(r.id(), Some(42));
    }
}
