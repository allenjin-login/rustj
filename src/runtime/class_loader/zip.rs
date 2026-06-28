//! zip 容器读取(中心目录 + 条目解压)。
//!
//! 对应 HotSpot `classfile/classLoader.cpp` 的 `ClassPathZipEntry`:`open_stream(name)` →
//! `find_entry`(中心目录查)→ `read_entry`(DEFLATE 则解压)→ 字节流。zip 文件格式依
//! PKWARE APPNOTE(EOCD + 中心目录 + 本地文件头);HotSpot 经 vendored `libzip/zip_util.c`
//! 读之(rustj 手移植,format 细节为标准)。**仅读**(不解 zip、不写)。
//!
//! 关键:jmod/jar 通用标准 zip;jmod 首 4 字节为 magic 前缀(`JM..`),使 zip 内记录的
//! 偏移(zip 起点相对)与文件内偏移差一个前缀长度——用 `cd_start - cd_off_recorded` 求得
//! base 偏移修正(对齐自解压/SFX 归档的标准做法),故 `ZipReader` 对 jar/jmod 同构。

use super::inflate::{self, InflateError};

/// 本地文件头签名。
const LFH_SIG: u32 = 0x0403_4b50;
/// 中心目录条目签名。
const CDH_SIG: u32 = 0x0201_4b50;
/// EOCD(End of Central Directory)签名。
const EOCD_SIG: u32 = 0x0605_4b50;

/// zip 读取错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// 数据截断(偏移越界)。
    Truncated,
    /// 找不到 EOCD(非 zip 或损坏)。
    NoEocd,
    /// 期望的签名不符(附带实际读到的值)。
    BadSignature(u32),
    /// 不支持的压缩方法(仅支持 STORED=0 / DEFLATED=8)。
    UnsupportedMethod(u16),
    /// DEFLATE 解压失败。
    Inflate(InflateError),
}

/// 中心目录解析出的一条 zip 条目。
#[derive(Debug)]
struct Entry {
    name: String,
    method: u16,
    comp_size: u64,
    data_offset: u64, // 压缩数据在文件内的实际字节偏移
}

/// zip 容器只读视图。构造时一次性解析中心目录;`read` 按需取条目(惰性解压)。
///
/// 持有容器字节的**拥有副本**(`Vec<u8>`)——`ClassPath` 需把打开的容器长期存活(随注册表/
/// Vm 同寿),拥有副本免去"数据与其 `&` 借用同处一结构"的自引用难题(安全 Rust 无法表达)。
/// `new(&[u8])` 仍按借用构造,内部拷贝;对外为零寿命依赖。
#[derive(Debug)]
pub struct ZipReader {
    data: Vec<u8>,
    entries: Vec<Entry>,
}

fn u16le(d: &[u8], o: usize) -> Result<u16, ZipError> {
    d.get(o..o + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or(ZipError::Truncated)
}

fn u32le(d: &[u8], o: usize) -> Result<u32, ZipError> {
    d.get(o..o + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or(ZipError::Truncated)
}

/// 从尾部倒找 EOCD 签名(注释段 ≤ 65535,故只扫末尾 22+65535 字节内)。
fn find_eocd(d: &[u8]) -> Result<usize, ZipError> {
    if d.len() < 22 {
        return Err(ZipError::Truncated);
    }
    let from = d.len() - 22;
    let stop = d.len().saturating_sub(22 + 65535);
    for pos in (stop..=from).rev() {
        if u32le(d, pos)? == EOCD_SIG {
            return Ok(pos);
        }
    }
    Err(ZipError::NoEocd)
}

impl ZipReader {
    /// 解析 zip 字节流:定位 EOCD → 读中心目录 → 建条目索引(含 base 偏移修正)。
    pub fn new(data: &[u8]) -> Result<Self, ZipError> {
        let eocd = find_eocd(data)?;
        let num = u16le(data, eocd + 10)? as usize;
        let cd_size = u32le(data, eocd + 12)? as u64;
        let cd_off_rec = u32le(data, eocd + 16)? as u64;
        // 中心目录实际文件偏移 = EOCD 起点减其长度(EOCD 紧随中心目录)。
        let cd_start = (eocd as u64)
            .checked_sub(cd_size)
            .ok_or(ZipError::Truncated)? as usize;
        // base 修正:jmod 前缀等使记录偏移(zip 起点相对)≠ 文件偏移;两者之差即前缀长度。
        let base = (cd_start as u64).saturating_sub(cd_off_rec) as usize;

        let mut entries = Vec::with_capacity(num);
        let mut p = cd_start;
        for _ in 0..num {
            if u32le(data, p)? != CDH_SIG {
                return Err(ZipError::BadSignature(u32le(data, p).unwrap_or(0)));
            }
            let method = u16le(data, p + 10)?;
            let comp_size = u32le(data, p + 20)? as u64;
            let name_len = u16le(data, p + 28)? as usize;
            let extra_len = u16le(data, p + 30)? as usize;
            let comment_len = u16le(data, p + 32)? as usize;
            let local_off = u32le(data, p + 42)? as usize;
            let name_bytes = data
                .get(p + 46..p + 46 + name_len)
                .ok_or(ZipError::Truncated)?;
            let name = String::from_utf8_lossy(name_bytes).into_owned();

            // 由本地文件头(name_len/extra_len)定位数据起点——本地头是数据偏移的权威来源。
            let lh = base + local_off;
            if u32le(data, lh)? != LFH_SIG {
                return Err(ZipError::BadSignature(u32le(data, lh).unwrap_or(0)));
            }
            let lname = u16le(data, lh + 26)? as usize;
            let lextra = u16le(data, lh + 28)? as usize;
            let data_offset = (lh + 30 + lname + lextra) as u64;

            entries.push(Entry { name, method, comp_size, data_offset });
            p += 46 + name_len + extra_len + comment_len;
        }
        Ok(Self { data: data.to_vec(), entries })
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 是否存在名为 `name` 的条目。
    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|e| e.name == name)
    }

    /// 所有条目名。
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// 读取并解压名为 `name` 的条目。
    ///
    /// 返回 `Ok(None)` 表示条目不存在;`Err` 表示损坏/解压失败。
    /// STORED 直读、DEFLATED 调 [`inflate::inflate`]。
    pub fn read(&self, name: &str) -> Result<Option<Vec<u8>>, ZipError> {
        let Some(e) = self.entries.iter().find(|e| e.name == name) else {
            return Ok(None);
        };
        let start = e.data_offset as usize;
        let end = start
            .checked_add(e.comp_size as usize)
            .ok_or(ZipError::Truncated)?;
        let comp = self.data.get(start..end).ok_or(ZipError::Truncated)?;
        match e.method {
            0 => Ok(Some(comp.to_vec())),
            8 => Ok(Some(inflate::inflate(comp).map_err(ZipError::Inflate)?)),
            m => Err(ZipError::UnsupportedMethod(m)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 向量由 Python zipfile 生成并自校验;覆盖 STORED / DEFLATED / 带 4 字节前缀(模拟 jmod)。

    const STORED_ZIP: &[u8] = &[
        80, 75, 3, 4, 20, 0, 0, 0, 0, 0, 197, 160, 220, 92, 134, 166, 16, 54, 5, 0, 0, 0, 5, 0, 0,
        0, 5, 0, 0, 0, 97, 46, 116, 120, 116, 104, 101, 108, 108, 111, 80, 75, 3, 4, 20, 0, 0, 0,
        0, 0, 197, 160, 220, 92, 1, 17, 203, 48, 7, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0, 0, 100, 105,
        114, 47, 98, 46, 116, 120, 116, 119, 111, 114, 108, 100, 33, 33, 80, 75, 1, 2, 20, 0, 20,
        0, 0, 0, 0, 0, 197, 160, 220, 92, 134, 166, 16, 54, 5, 0, 0, 0, 5, 0, 0, 0, 5, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 97, 46, 116, 120, 116, 80, 75, 1, 2, 20, 0, 20,
        0, 0, 0, 0, 0, 197, 160, 220, 92, 1, 17, 203, 48, 7, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 128, 1, 40, 0, 0, 0, 100, 105, 114, 47, 98, 46, 116, 120, 116, 80,
        75, 5, 6, 0, 0, 0, 0, 2, 0, 2, 0, 106, 0, 0, 0, 86, 0, 0, 0, 0, 0,
    ];
    const DEFLATED_ZIP: &[u8] = &[
        80, 75, 3, 4, 20, 0, 0, 0, 8, 0, 197, 160, 220, 92, 176, 185, 4, 100, 70, 0, 0, 0, 68, 0,
        0, 0, 22, 0, 0, 0, 106, 97, 118, 97, 47, 108, 97, 110, 103, 47, 79, 98, 106, 101, 99, 116,
        46, 99, 108, 97, 115, 115, 59, 245, 111, 215, 62, 6, 70, 38, 102, 22, 86, 54, 118, 14, 78,
        46, 110, 30, 94, 62, 126, 1, 65, 33, 97, 17, 81, 49, 113, 9, 73, 41, 105, 25, 89, 57, 121,
        5, 69, 37, 101, 21, 85, 53, 117, 13, 77, 45, 109, 29, 93, 61, 125, 3, 67, 35, 99, 19, 83,
        51, 115, 11, 75, 43, 107, 27, 91, 59, 123, 0, 80, 75, 1, 2, 20, 0, 20, 0, 0, 0, 8, 0, 197,
        160, 220, 92, 176, 185, 4, 100, 70, 0, 0, 0, 68, 0, 0, 0, 22, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 128, 1, 0, 0, 0, 0, 106, 97, 118, 97, 47, 108, 97, 110, 103, 47, 79, 98, 106, 101,
        99, 116, 46, 99, 108, 97, 115, 115, 80, 75, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0, 68, 0, 0, 0,
        122, 0, 0, 0, 0, 0,
    ];
    const DEFLATED_EXPECTED: &[u8] = &[
        202, 254, 186, 190, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
        20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42,
        43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
    ];
    // 4 字节 magic 前缀 + DEFLATED zip:模拟 jmod 布局,验证 base 偏移修正。
    const PREFIXED_ZIP: &[u8] = &[
        74, 77, 0, 0, 80, 75, 3, 4, 20, 0, 0, 0, 8, 0, 197, 160, 220, 92, 176, 185, 4, 100, 70, 0,
        0, 0, 68, 0, 0, 0, 22, 0, 0, 0, 106, 97, 118, 97, 47, 108, 97, 110, 103, 47, 79, 98, 106,
        101, 99, 116, 46, 99, 108, 97, 115, 115, 59, 245, 111, 215, 62, 6, 70, 38, 102, 22, 86,
        54, 118, 14, 78, 46, 110, 30, 94, 62, 126, 1, 65, 33, 97, 17, 81, 49, 113, 9, 73, 41, 105,
        25, 89, 57, 121, 5, 69, 37, 101, 21, 85, 53, 117, 13, 77, 45, 109, 29, 93, 61, 125, 3, 67,
        35, 99, 19, 83, 51, 115, 11, 75, 43, 107, 27, 91, 59, 123, 0, 80, 75, 1, 2, 20, 0, 20, 0,
        0, 0, 8, 0, 197, 160, 220, 92, 176, 185, 4, 100, 70, 0, 0, 0, 68, 0, 0, 0, 22, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 106, 97, 118, 97, 47, 108, 97, 110, 103, 47, 79,
        98, 106, 101, 99, 116, 46, 99, 108, 97, 115, 115, 80, 75, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0,
        68, 0, 0, 0, 122, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn reads_stored_entries() {
        let z = ZipReader::new(STORED_ZIP).unwrap();
        assert_eq!(z.len(), 2);
        assert!(z.has("a.txt"));
        assert!(z.has("dir/b.txt"));
        assert!(!z.has("missing"));
        assert_eq!(z.read("a.txt").unwrap().unwrap().as_slice(), b"hello");
        assert_eq!(z.read("dir/b.txt").unwrap().unwrap().as_slice(), b"world!!");
        // 缺失条目返回 Ok(None)。
        assert_eq!(z.read("missing").unwrap(), None);
    }

    #[test]
    fn reads_deflated_entry() {
        let z = ZipReader::new(DEFLATED_ZIP).unwrap();
        assert_eq!(z.len(), 1);
        assert!(z.has("java/lang/Object.class"));
        assert_eq!(
            z.read("java/lang/Object.class").unwrap().unwrap().as_slice(),
            DEFLATED_EXPECTED
        );
    }

    #[test]
    fn handles_prefix_offset_like_jmod() {
        // 4 字节 magic 前缀:base 偏移修正须令读取仍正确。
        let z = ZipReader::new(PREFIXED_ZIP).unwrap();
        assert_eq!(
            z.read("java/lang/Object.class").unwrap().unwrap().as_slice(),
            DEFLATED_EXPECTED
        );
    }

    #[test]
    fn rejects_non_zip() {
        assert_eq!(ZipReader::new(&[]).unwrap_err(), ZipError::Truncated);
        // ≥22 字节但无 EOCD 签名 → NoEocd。
        assert_eq!(ZipReader::new(&[0u8; 30]).unwrap_err(), ZipError::NoEocd);
    }

    #[test]
    fn names_lists_all_entries() {
        let z = ZipReader::new(STORED_ZIP).unwrap();
        let mut names: Vec<&str> = z.names().collect();
        names.sort();
        assert_eq!(names, vec!["a.txt", "dir/b.txt"]);
    }
}
