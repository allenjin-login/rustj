//! 字符串 intern 池:文本 → 堆引用的纯备忘。对应 HotSpot StringTable(全局/每堆一份)。
//!
//! Layer 4.10i:`Oop::String` 特殊变体退役后,intern 表退化为**纯文本 → 引用**备忘;
//! 真 `java/lang/String` 实例的**构造**移入 interpreter(需 `clinit`),本结构仅负责
//! 「同文本恒同引用」的查表/登记。`ldc`/`ldc_w` 与 `String.intern()` 经
//! [`crate::runtime::interpreter::string::intern`] 调用本结构的 `get`/`insert`。

use std::collections::HashMap;

use crate::runtime::Reference;

/// 字符串 intern 池:文本 → 堆引用。每 Vm 一份(纯备忘,不触堆)。
pub struct StringPool {
    table: HashMap<String, Reference>,
}

impl StringPool {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
        }
    }

    /// 查既有 intern 引用(无则 `None`)。
    pub fn get(&self, text: &str) -> Option<&Reference> {
        self.table.get(text)
    }

    /// 登记 `text → r`(调用方保证 `r` 是 `text` 对应的真 String 实例引用)。
    pub fn insert(&mut self, text: String, r: Reference) {
        self.table.insert(text, r);
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

    #[test]
    fn get_miss_returns_none() {
        let pool = StringPool::new();
        assert!(pool.get("hi").is_none());
    }

    #[test]
    fn insert_then_get_hits() {
        let mut pool = StringPool::new();
        let r = Reference::from_id(7);
        pool.insert("hi".to_string(), r);
        assert_eq!(pool.get("hi"), Some(&r));
    }

    #[test]
    fn distinct_text_distinct_entry() {
        let mut pool = StringPool::new();
        let a = Reference::from_id(0);
        let b = Reference::from_id(1);
        pool.insert("a".to_string(), a);
        pool.insert("b".to_string(), b);
        assert_eq!(pool.get("a"), Some(&a));
        assert_eq!(pool.get("b"), Some(&b));
    }
}
