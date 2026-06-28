//! 有界大端字节读取器,对应 HotSpot `ClassFileStream`。
//!
//! 全部用 safe 切片索引,绝不指针强转。class 文件是大端的(JVMS §4.1)。

use super::error::ClassFileError;

/// 顺序读取 `&[u8]` 的游标,所有读取方法返回 `Result`,越界即 `Truncated`。
pub struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// 在给定字节切片上构造读取器,游标起始为 0。
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// 当前游标位置。
    pub fn position(&self) -> usize {
        self.pos
    }

    /// 剩余可读字节数。
    pub fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }

    /// 底层切片(便于测试/诊断)。
    pub fn as_slice(&self) -> &'a [u8] {
        self.bytes
    }

    /// 读 1 字节无符号整数。
    pub fn u1(&mut self) -> Result<u8, ClassFileError> {
        if self.pos >= self.bytes.len() {
            return Err(ClassFileError::Truncated {
                needed: 1,
                remaining: 0,
            });
        }
        let v = self.bytes[self.pos];
        self.pos += 1;
        Ok(v)
    }

    /// 读 2 字节大端无符号整数。
    pub fn u2(&mut self) -> Result<u16, ClassFileError> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }

    /// 读 4 字节大端无符号整数。
    pub fn u4(&mut self) -> Result<u32, ClassFileError> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }

    /// 原样读取 `n` 字节切片。
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], ClassFileError> {
        if self.pos + n > self.bytes.len() {
            return Err(ClassFileError::Truncated {
                needed: n,
                remaining: self.remaining(),
            });
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// 读取 JVM 修改版 UTF-8 字符串(JVMS §4.4.7),长度由调用方给出。
    pub fn modified_utf8(&mut self, len: usize) -> Result<String, ClassFileError> {
        let bytes = self.take(len)?;
        decode_modified_utf8(bytes)
    }
}

/// 解码 JVM 修改版 UTF-8(JVMS §4.4.7)。
///
/// 与标准 UTF-8 的两点差异:
/// 1. U+0000 编码为 0xC0 0x80(两字节),而非单字节 0x00;
/// 2. 辅助平面字符(U+10000 以上)以 UTF-16 代理对形式,各按 3 字节序列编码。
///
/// 实现:先把每段序列解码为 BMP 码元(`Vec<u16>`),再把代理对合并为 `char`。
///
/// **孤立代理(lone surrogate)的处理**:JVMS 允许 `Utf8` 含孤立代理单元(如 `GB18030` 把映射
/// 表存为含原始 UTF-16 单元的 Java 串,javac 逐单元按 3 字节编码,jvm 容忍读取)。但 Rust
/// `String`/`char` **不能表示代理**,故本解码器把孤立代理**损余替换**为 `U+FFFD`——仅影响含
/// 孤立代理的串常量之**值**(名字/描述符/方法名绝不含代理,不受影响)。忠实往返(保留原始字节)
/// 见后续「原始字节 Utf8 存储」层。
fn decode_modified_utf8(bytes: &[u8]) -> Result<String, ClassFileError> {
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x00 {
            // 修改版 UTF-8 中单字节 0x00 非法(null 必须写成 0xC0 0x80)
            return Err(ClassFileError::InvalidUtf8);
        }
        if b < 0x80 {
            // 1 字节:0x01..0x7F
            units.push(u16::from(b));
            i += 1;
        } else if b & 0xE0 == 0xC0 {
            // 2 字节:0x80..0x7FF(及 U+0000 的 0xC0 0x80)
            if i + 1 >= bytes.len() {
                return Err(ClassFileError::InvalidUtf8);
            }
            let b1 = bytes[i + 1];
            if b1 & 0xC0 != 0x80 {
                return Err(ClassFileError::InvalidUtf8);
            }
            let cp = ((u16::from(b) & 0x1F) << 6) | (u16::from(b1) & 0x3F);
            units.push(cp);
            i += 2;
        } else if b & 0xF0 == 0xE0 {
            // 3 字节:BMP 字符,或代理对的一半
            if i + 2 >= bytes.len() {
                return Err(ClassFileError::InvalidUtf8);
            }
            let b1 = bytes[i + 1];
            let b2 = bytes[i + 2];
            if b1 & 0xC0 != 0x80 || b2 & 0xC0 != 0x80 {
                return Err(ClassFileError::InvalidUtf8);
            }
            let cp = ((u16::from(b) & 0x0F) << 12)
                | ((u16::from(b1) & 0x3F) << 6)
                | (u16::from(b2) & 0x3F);
            units.push(cp);
            i += 3;
        } else {
            return Err(ClassFileError::InvalidUtf8);
        }
    }

    // 合并 UTF-16 代理对,得到 `String`。孤立代理(无配对)→ U+FFFD(Rust char 不可表示代理)。
    let mut out = String::with_capacity(units.len());
    let mut j = 0;
    while j < units.len() {
        let u = units[j];
        if (0xD800..=0xDBFF).contains(&u) {
            // 高代理:紧随低代理则合并为辅助平面字符;否则孤立 → U+FFFD。
            if j + 1 < units.len() && (0xDC00..=0xDFFF).contains(&units[j + 1]) {
                let hi = u32::from(u);
                let lo = u32::from(units[j + 1]);
                let cp = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                out.push(char::from_u32(cp).ok_or(ClassFileError::InvalidUtf8)?);
                j += 2;
            } else {
                out.push('\u{FFFD}');
                j += 1;
            }
        } else if (0xDC00..=0xDFFF).contains(&u) {
            // 孤立低代理(无前导高代理)→ U+FFFD。
            out.push('\u{FFFD}');
            j += 1;
        } else {
            out.push(char::from_u32(u32::from(u)).ok_or(ClassFileError::InvalidUtf8)?);
            j += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_u1_and_advances() {
        let mut r = Reader::new(&[0x01, 0x02, 0x03]);
        assert_eq!(r.u1().unwrap(), 1);
        assert_eq!(r.u1().unwrap(), 2);
        assert_eq!(r.position(), 2);
        assert_eq!(r.remaining(), 1);
    }

    #[test]
    fn reads_u2_big_endian() {
        let mut r = Reader::new(&[0x12, 0x34, 0x00, 0xFF]);
        assert_eq!(r.u2().unwrap(), 0x1234);
        assert_eq!(r.u2().unwrap(), 0x00FF);
    }

    #[test]
    fn reads_u4_big_endian() {
        let mut r = Reader::new(&[0xCA, 0xFE, 0xBA, 0xBE]);
        assert_eq!(r.u4().unwrap(), 0xCAFE_BABE);
    }

    #[test]
    fn u1_truncated_reports_error() {
        let mut r = Reader::new(&[]);
        let err = r.u1().unwrap_err();
        assert_eq!(err, ClassFileError::Truncated { needed: 1, remaining: 0 });
    }

    #[test]
    fn u2_truncated_reports_error() {
        let mut r = Reader::new(&[0x12]);
        let err = r.u2().unwrap_err();
        assert_eq!(err, ClassFileError::Truncated { needed: 2, remaining: 1 });
    }

    #[test]
    fn take_returns_slice_and_advances() {
        let mut r = Reader::new(&[0xA, 0xB, 0xC, 0xD]);
        let s = r.take(3).unwrap();
        assert_eq!(s, &[0xA, 0xB, 0xC]);
        assert_eq!(r.position(), 3);
    }

    #[test]
    fn take_truncated() {
        let mut r = Reader::new(&[0xA]);
        assert_eq!(
            r.take(2).unwrap_err(),
            ClassFileError::Truncated { needed: 2, remaining: 1 }
        );
    }

    #[test]
    fn modified_utf8_ascii() {
        let mut r = Reader::new(b"Hello");
        let s = r.modified_utf8(5).unwrap();
        assert_eq!(s, "Hello");
    }

    #[test]
    fn modified_utf8_null_as_c080() {
        // U+0000 在修改版 UTF-8 里编码为 0xC0 0x80
        let mut r = Reader::new(&[0xC0, 0x80]);
        let s = r.modified_utf8(2).unwrap();
        assert_eq!(s, "\u{0000}");
    }

    #[test]
    fn modified_utf8_two_byte() {
        // U+00E9 (é) = 0xC3 0xA9
        let mut r = Reader::new(&[0xC3, 0xA9]);
        let s = r.modified_utf8(2).unwrap();
        assert_eq!(s, "é");
    }

    #[test]
    fn modified_utf8_three_byte_bmp() {
        // U+4E2D (中) = 0xE4 0xB8 0xAD
        let mut r = Reader::new(&[0xE4, 0xB8, 0xAD]);
        let s = r.modified_utf8(3).unwrap();
        assert_eq!(s, "中");
    }

    #[test]
    fn modified_utf8_supplementary_via_surrogate_pair() {
        // U+1F600: UTF-16 代理对 D83D DE00,各自按 3 字节编码。
        // D83D = 0xED 0xA0 0xBD ; DE00 = 0xED 0xB8 0x80
        let mut r = Reader::new(&[0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80]);
        let s = r.modified_utf8(6).unwrap();
        assert_eq!(s, "😀");
    }

    #[test]
    fn modified_utf8_lone_surrogates_become_replacement() {
        // GB18030 等把映射表存为含孤立代理单元的 Java 串;javac 逐单元按 3 字节编码。
        // Rust String 不可表示代理 → 损余为 U+FFFD(配对代理仍正确合并,见上)。
        // 孤立低代理 U+DE9A = 0xED 0xBA 0x9A(无前导高代理):
        let mut r = Reader::new(&[0xED, 0xBA, 0x9A]);
        assert_eq!(r.modified_utf8(3).unwrap(), "\u{FFFD}");
        // 孤立高代理 U+D83D = 0xED 0xA0 0xBD(无后随低代理;配对时 D83D DE00 合为 😀):
        let mut r = Reader::new(&[0xED, 0xA0, 0xBD]);
        assert_eq!(r.modified_utf8(3).unwrap(), "\u{FFFD}");
    }
}
