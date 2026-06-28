//! 字符串对象(对应 HotSpot `java/lang/String` 实例的值载体)。
//!
//! Layer 4.8:字符串字面量(`ldc`/`ldc_w` 取 `CONSTANT_String`)的堆表示。仅持有
//! 解码后的文本(常量池解析器已从 modified-UTF-8 解码为 Rust `String`)。
//!
//! **不合成 `java/lang/String` 类桩**——对 String 调方法 / `instanceof` / `checkcast`
//! 的完整语义顺延到"加载真实 String 类"层,以免引入一次性技术债。本层只承诺:
//! 同一字面量经 intern 得同一引用(故 `"x" == "x"` 成立)。

/// 字符串对象:解码文本的值载体。
#[derive(Debug, Clone, PartialEq)]
pub struct StringOop {
    text: String,
}

impl StringOop {
    /// 由解码文本构造。
    pub(crate) fn new(text: String) -> Self {
        Self { text }
    }

    /// 解码后的文本。
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_text_round_trip() {
        let s = StringOop::new("hello".into());
        assert_eq!(s.text(), "hello");
    }

    #[test]
    fn eq_by_text() {
        assert_eq!(StringOop::new("a".into()), StringOop::new("a".into()));
        assert_ne!(StringOop::new("a".into()), StringOop::new("b".into()));
    }
}
