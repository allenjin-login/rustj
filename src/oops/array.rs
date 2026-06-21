//! 一维数组(对应 HotSpot `typeArrayOop` / `objArrayOop`,本层统一表示)。
//!
//! 每个逻辑元素恰好一个 [`Slot`](long/double 也单槽,与实例字段模型一致;
//! cat-2 双槽语义仅在操作数栈/局部变量上成立,由 `*aload`/`*astore` 边界转换)。
//! 元素类型由指令决定,不在此记录(4.3a 不做 ArrayStoreException)。

use crate::runtime::Slot;

/// 一维数组:元素槽位向量(每元素一槽)。
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayOop {
    elements: Vec<Slot>,
}

impl ArrayOop {
    /// 由初始元素向量构造。
    pub(crate) fn new(elements: Vec<Slot>) -> Self {
        Self { elements }
    }

    /// 元素个数。
    pub fn length(&self) -> usize {
        self.elements.len()
    }

    /// 取元素槽(调用方已做越界检查)。
    pub fn element(&self, index: usize) -> Slot {
        self.elements[index]
    }

    /// 写元素槽(调用方已做越界检查)。
    pub fn set_element(&mut self, index: usize, slot: Slot) {
        self.elements[index] = slot;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Reference;

    #[test]
    fn new_array_length_and_defaults() {
        let a = ArrayOop::new(vec![Slot::Int(0); 3]);
        assert_eq!(a.length(), 3);
        assert_eq!(a.element(0), Slot::Int(0));
    }

    #[test]
    fn set_and_get_round_trip() {
        let mut a = ArrayOop::new(vec![Slot::Int(0); 2]);
        a.set_element(1, Slot::Int(42));
        assert_eq!(a.element(1), Slot::Int(42));
        assert_eq!(a.element(0), Slot::Int(0));
    }

    #[test]
    fn references_default_null() {
        let a = ArrayOop::new(vec![Slot::Reference(Reference::null()); 1]);
        assert_eq!(a.element(0), Slot::Reference(Reference::null()));
    }
}
