//! 字符串 intern 池:文本 → 堆引用。对应 HotSpot StringTable(全局/每堆一份)。
//!
//! Layer 4.8:`ldc`/`ldc_w` 取 `CONSTANT_String` 时经 [`StringPool::intern`] 把解码文本
//! 映射为**唯一**堆引用——同一字面量恒得同一引用,故 `"x" == "x"` 成立。
//! 池以所属 [`Vm`](super::vm::Vm) 的 [`Heap`] 为后盾:首次出现时在堆上分配
//! `Oop::String`,再次出现直接返回既有引用。

use std::collections::HashMap;

use crate::oops::{Oop, StringOop};
use crate::runtime::heap::Heap;
use crate::runtime::Reference;

/// 字符串 intern 池:文本 → 堆引用。每 Vm 一份。
pub struct StringPool {
    table: HashMap<String, Reference>,
}

impl StringPool {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
        }
    }

    /// 返回 `text` 的 intern 引用:已有则复用,否则在 `heap` 上分配 `Oop::String` 并登记。
    pub fn intern(&mut self, heap: &mut Heap, text: &str) -> Reference {
        if let Some(&r) = self.table.get(text) {
            return r;
        }
        let r = heap.alloc(Oop::String(StringOop::new(text.to_string())));
        self.table.insert(text.to_string(), r);
        r
    }
}

impl Default for StringPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oops::Oop;

    #[test]
    fn intern_allocates_on_first_use() {
        let mut pool = StringPool::new();
        let mut heap = Heap::new();
        let r = pool.intern(&mut heap, "hi");
        assert!(!r.is_null());
        match heap.get(r).unwrap() {
            Oop::String(s) => assert_eq!(s.text(), "hi"),
            _ => panic!("应为 Oop::String"),
        }
    }

    #[test]
    fn intern_returns_same_ref_for_same_text() {
        let mut pool = StringPool::new();
        let mut heap = Heap::new();
        let a = pool.intern(&mut heap, "x");
        let b = pool.intern(&mut heap, "x");
        assert_eq!(a, b, "同文本应得同引用");
        assert_eq!(a.id(), Some(0), "只分配一次");
    }

    #[test]
    fn intern_distinct_ref_for_distinct_text() {
        let mut pool = StringPool::new();
        let mut heap = Heap::new();
        let a = pool.intern(&mut heap, "a");
        let b = pool.intern(&mut heap, "b");
        assert_ne!(a, b);
        assert_eq!(a.id(), Some(0));
        assert_eq!(b.id(), Some(1));
    }
}
