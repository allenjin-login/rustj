//! DEFLATE 解压(RFC 1951)。
//!
//! 对应 HotSpot 解 zip 内 DEFLATE 条目——HotSpot 调 vendored zlib(`libzip/zlib/inflate.c`);
//! rustj **不引依赖**、`#![deny(unsafe_code)]`,手移植 RFC 1951 解压器(纯 safe Rust)。
//!
//! **算法依据**:RFC 1951(DEFLATE 权威规范);结构对齐 zlib 参考解压器 `contrib/puff/puff.c`
//! (Mark Adler 的精简参考实现:canonical Huffman 构造 + 逐位解码)。
//!
//! 仅做**解压**(`inflate`),不做压缩。输入为裸 DEFLATE 流(RFC 1951 §3,**不含** zlib/gzip 头尾)。

/// DEFLATE 解压错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InflateError {
    /// 输入意外结束(位/字节越界)。
    UnexpectedEof,
    /// 数据损坏:非法块类型、非法 Huffman 编码、LEN/NLEN 不匹配、回溯越界等。
    InvalidData(&'static str),
}

/// 最大 Huffman 码长(RFC 1951:≤15 位)。
const MAXBITS: usize = 15;

/// canonical Huffman 表:`count[len]` = 码长为 `len` 的符号数;`symbol` 按 (码长, 符号序)
/// 规范排列的符号序列。构造与解码对齐 `puff.c` 的 `construct` / `decode`。
struct Huffman {
    count: [u16; MAXBITS + 1],
    symbol: Vec<u16>,
}

/// 由各符号的码长数组构造 canonical Huffman 表。
///
/// 返回 `Err` 仅当**超订阅**(码长集合无法构成合法前缀码);**不完整**(left>0)允许返回
/// (解码时若命中未定义码则报错)——对齐 `puff.c:construct`。
fn construct(lengths: &[u16]) -> Result<Huffman, InflateError> {
    let mut count = [0u16; MAXBITS + 1];
    for &l in lengths {
        if l as usize > MAXBITS {
            return Err(InflateError::InvalidData("码长 > 15"));
        }
        count[l as usize] += 1;
    }
    // 超订阅检查:逐码长累计剩余码空间。
    let mut left: i32 = 1;
    for &c in count[1..=MAXBITS].iter() {
        left <<= 1;
        left -= c as i32;
        if left < 0 {
            return Err(InflateError::InvalidData("Huffman 码超订阅"));
        }
    }
    // 各码长在符号表中的起始偏移(按码长递增)。
    let mut offs = [0u16; MAXBITS + 1];
    for len in 1..MAXBITS {
        offs[len + 1] = offs[len] + count[len];
    }
    let mut symbol = vec![0u16; lengths.iter().filter(|&&l| l != 0).count()];
    let mut cursor = offs;
    for (sym, &l) in lengths.iter().enumerate() {
        if l != 0 {
            let pos = cursor[l as usize] as usize;
            cursor[l as usize] += 1;
            symbol[pos] = sym as u16;
        }
    }
    Ok(Huffman { count, symbol })
}

/// LSB-first 逐位读取器。DEFLATE 把位从每字节的最低有效位起打包(RFC 1951 §3.1.1);
/// Huffman 码以每次读一位、向高位移位的方式重建为 canonical 码。
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u32, // 0..8,当前字节内位序(LSB-first)
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, byte_pos: 0, bit_pos: 0 }
    }

    /// 读 `n` 位(LSB-first,n ≤ 24),结果的 bit0 = 流中下一个位。
    fn read_bits(&mut self, n: u32) -> Result<u32, InflateError> {
        let mut val = 0u32;
        for i in 0..n {
            if self.byte_pos >= self.data.len() {
                return Err(InflateError::UnexpectedEof);
            }
            let bit = (self.data[self.byte_pos] >> self.bit_pos) & 1;
            val |= (bit as u32) << i;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Ok(val)
    }

    /// 丢弃当前字节内剩余位,对齐到下一字节边界(stored 块的字节对齐数据)。
    fn align(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
    }

    /// 取 `n` 个对齐字节(stored 块的原始数据),借用切片并推进读位置。
    fn take_bytes(&mut self, n: usize) -> Result<&'a [u8], InflateError> {
        if self.byte_pos.checked_add(n).is_none_or(|end| end > self.data.len()) {
            return Err(InflateError::UnexpectedEof);
        }
        let s = &self.data[self.byte_pos..self.byte_pos + n];
        self.byte_pos += n;
        Ok(s)
    }
}

/// 用 canonical 表逐位解码一个符号(对齐 `puff.c:decode`)。
fn decode(br: &mut BitReader<'_>, h: &Huffman) -> Result<u16, InflateError> {
    let mut code: i32 = 0;
    let mut first: i32 = 0;
    let mut index: i32 = 0;
    for len in 1..=MAXBITS {
        code |= br.read_bits(1)? as i32;
        let count = h.count[len] as i32;
        if code - first < count {
            return Ok(h.symbol[(index + (code - first)) as usize]);
        }
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    Err(InflateError::InvalidData("非法 Huffman 编码"))
}

// 长度码 257..285 的(基础长度, 额外位数)——RFC 1951 §3.2.5。
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u16; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
// 距离码 0..29 的(基础距离, 额外位数)——RFC 1951 §3.2.5。
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u16; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// 构造 fixed-Huffman 的字面/长度表(288 符号;RFC 1951 §3.2.6)。
fn fixed_litlen() -> Huffman {
    let mut lengths = [0u16; 288];
    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..288].fill(8);
    construct(&lengths).expect("fixed litlen 码长集合合法,构造不会失败")
}

/// 构造 fixed-Huffman 的距离表(30 符号,皆 5 位;RFC 1951 §3.2.6)。
fn fixed_dist() -> Huffman {
    let lengths = [5u16; 30];
    construct(&lengths).expect("fixed dist 码长集合合法,构造不会失败")
}

/// 读动态 Huffman 头(RFC 1951 §3.2.7),返回 (字面/长度表, 距离表)。
fn read_dynamic(br: &mut BitReader<'_>) -> Result<(Huffman, Huffman), InflateError> {
    // 码长码的读取顺序(RFC 1951 §3.2.7)。
    const CL_ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = br.read_bits(5)? as usize + 257;
    let hdist = br.read_bits(5)? as usize + 1;
    let hclen = br.read_bits(4)? as usize + 4;
    if hlit > 286 || hdist > 30 {
        return Err(InflateError::InvalidData("HLIT/HDIST 越界"));
    }
    let mut cl_lengths = [0u16; 19];
    for i in 0..hclen {
        cl_lengths[CL_ORDER[i]] = br.read_bits(3)? as u16;
    }
    let cl_huff = construct(&cl_lengths)?;

    // 读 hlit + hdist 个码长(字面/长度在前,距离在后),含重复码 16/17/18。
    let total = hlit + hdist;
    let mut lengths: Vec<u16> = Vec::with_capacity(total);
    while lengths.len() < total {
        let sym = decode(br, &cl_huff)?;
        match sym {
            0..=15 => lengths.push(sym),
            16 => {
                let Some(&prev) = lengths.last() else {
                    return Err(InflateError::InvalidData("重复码 16 无前值"));
                };
                let rep = br.read_bits(2)? as usize + 3;
                if lengths.len() + rep > total {
                    return Err(InflateError::InvalidData("长度重复 16 越界"));
                }
                lengths.resize(lengths.len() + rep, prev);
            }
            17 => {
                let rep = br.read_bits(3)? as usize + 3;
                if lengths.len() + rep > total {
                    return Err(InflateError::InvalidData("零重复 17 越界"));
                }
                lengths.resize(lengths.len() + rep, 0);
            }
            18 => {
                let rep = br.read_bits(7)? as usize + 11;
                if lengths.len() + rep > total {
                    return Err(InflateError::InvalidData("零重复 18 越界"));
                }
                lengths.resize(lengths.len() + rep, 0);
            }
            _ => return Err(InflateError::InvalidData("非法码长符号")),
        }
    }
    let lit_huff = construct(&lengths[..hlit])?;
    let dist_huff = construct(&lengths[hlit..])?;
    Ok((lit_huff, dist_huff))
}

/// 解一个压缩块:循环解码字面/长度符号,遇 256 结束;长度码触发回溯拷贝。
fn decode_block(
    br: &mut BitReader<'_>,
    lit: &Huffman,
    dist: &Huffman,
    out: &mut Vec<u8>,
) -> Result<(), InflateError> {
    loop {
        let sym = decode(br, lit)?;
        if sym < 256 {
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else if sym <= 285 {
            let lc = (sym - 257) as usize;
            let len = LENGTH_BASE[lc] as usize + br.read_bits(LENGTH_EXTRA[lc] as u32)? as usize;
            let dsym = decode(br, dist)? as usize;
            if dsym >= 30 {
                return Err(InflateError::InvalidData("非法距离码 ≥30"));
            }
            let distance = DIST_BASE[dsym] as usize + br.read_bits(DIST_EXTRA[dsym] as u32)? as usize;
            if distance > out.len() {
                return Err(InflateError::InvalidData("回溯距离越过输出起点"));
            }
            let start = out.len() - distance;
            // 逐字节拷贝以正确处理重叠(distance < length 的 RLE 式回溯)。
            for i in 0..len {
                let b = out[start + i];
                out.push(b);
            }
        } else {
            return Err(InflateError::InvalidData("非法长度码 286/287"));
        }
    }
}

/// 解压裸 DEFLATE 流(RFC 1951 §3)。
///
/// 输入须为**不含** zlib/gzip 头尾的裸 DEFLATE 字节(zip 条目的压缩数据即此)。
pub fn inflate(input: &[u8]) -> Result<Vec<u8>, InflateError> {
    let mut br = BitReader::new(input);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let final_block = br.read_bits(1)? != 0;
        let btype = br.read_bits(2)?;
        match btype {
            0 => {
                // stored:字节对齐后 LEN/NLEN/原始数据。
                br.align();
                let len = br.read_bits(16)? as usize;
                let nlen = br.read_bits(16)? as u16;
                if nlen != !(len as u16) {
                    return Err(InflateError::InvalidData("LEN/NLEN 不匹配"));
                }
                out.extend_from_slice(br.take_bytes(len)?);
            }
            1 => {
                let lit = fixed_litlen();
                let dist = fixed_dist();
                decode_block(&mut br, &lit, &dist, &mut out)?;
            }
            2 => {
                let (lit, dist) = read_dynamic(&mut br)?;
                decode_block(&mut br, &lit, &dist, &mut out)?;
            }
            _ => return Err(InflateError::InvalidData("非法块类型 3")),
        }
        if final_block {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 测试向量由 Python `zlib.compressobj(level, zlib.DEFLATED, -15)` 生成(裸 DEFLATE,
    // 无 zlib 头尾),并以 `zlib.decompress(comp, -15) == 原` 自校验。覆盖:
    //   stored(BTYPE=0)/ fixed Huffman(1)/ dynamic Huffman(2,含 LZ77 回溯)。

    const STORED_EMPTY_COMP: &[u8] = &[1, 0, 0, 255, 255];
    const STORED_SHORT_COMP: &[u8] = &[1, 2, 0, 253, 255, 72, 105];
    const FIXED_HELLO_COMP: &[u8] = &[
        243, 72, 205, 201, 201, 215, 81, 8, 207, 47, 202, 73, 81, 4, 0,
    ];
    const FIXED_FOX_COMP: &[u8] = &[
        11, 201, 72, 85, 40, 44, 205, 76, 206, 86, 72, 42, 202, 47, 207, 83, 72, 203, 175, 80, 200,
        42, 205, 45, 40, 86, 200, 47, 75, 45, 82, 40, 201, 72, 85, 200, 73, 172, 170, 84, 72, 201,
        79, 215, 3, 0,
    ];
    const DYNAMIC_REPEAT_COMP: &[u8] = &[75, 76, 74, 78, 28, 73, 8, 0];
    const DYNAMIC_TEXT_COMP: &[u8] = &[
        237, 203, 209, 9, 192, 48, 8, 5, 192, 85, 222, 0, 165, 147, 100, 137, 96, 164, 60, 136, 49,
        168, 217, 191, 107, 244, 163, 247, 127, 205, 67, 13, 220, 121, 12, 195, 167, 7, 146, 133,
        110, 90, 23, 196, 87, 170, 148, 214, 9, 244, 193, 205, 20, 174, 7, 58, 89, 55, 218, 31, 243,
        67, 241, 5,
    ];
    const DYNAMIC_CLASSLIKE_COMP: &[u8] = &[
        43, 40, 77, 202, 201, 76, 86, 40, 46, 73, 44, 1, 82, 101, 249, 153, 41, 10, 185, 137, 153,
        121, 26, 193, 37, 69, 153, 121, 233, 209, 177, 10, 137, 69, 233, 197, 154, 10, 213, 10, 193,
        149, 197, 37, 169, 185, 122, 249, 165, 37, 122, 5, 64, 169, 146, 156, 60, 141, 10, 77, 107,
        133, 90, 174, 130, 81, 19, 70, 77, 24, 118, 38, 0, 0,
    ];

    fn check(comp: &[u8], expected: &[u8]) {
        let got = inflate(comp).unwrap_or_else(|e| panic!("inflate 失败:{e:?}"));
        assert_eq!(got.as_slice(), expected, "解压输出不符");
    }

    #[test]
    fn inflate_stored_empty() {
        check(STORED_EMPTY_COMP, b"");
    }

    #[test]
    fn inflate_stored_short() {
        check(STORED_SHORT_COMP, b"Hi");
    }

    #[test]
    fn inflate_fixed_hello() {
        check(FIXED_HELLO_COMP, b"Hello, World!");
    }

    #[test]
    fn inflate_fixed_fox() {
        check(FIXED_FOX_COMP, b"The quick brown fox jumps over the lazy dog.");
    }

    #[test]
    fn inflate_dynamic_repeat() {
        // BTYPE=1(fixed Huffman)但重度 LZ77 回溯(distance=3 重复)。
        let inp: Vec<u8> = b"abc".repeat(80);
        check(DYNAMIC_REPEAT_COMP, &inp);
    }

    #[test]
    fn inflate_dynamic_text() {
        // BTYPE=2(真 dynamic Huffman)+ LZ77 回溯。
        let inp: Vec<u8> = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(8);
        check(DYNAMIC_TEXT_COMP, &inp);
    }

    #[test]
    fn inflate_dynamic_classlike() {
        // 类源码风格输入(BTYPE=1 fixed),多回溯。
        let inp: Vec<u8> = b"public static void main(String[] args) { System.out.println(x); }\n"
            .repeat(12);
        check(DYNAMIC_CLASSLIKE_COMP, &inp);
    }

    #[test]
    fn inflate_rejects_bad_block_type() {
        // BFINAL=1, BTYPE=3(非法)。
        assert_eq!(
            inflate(&[0b0000_0111]).unwrap_err(),
            InflateError::InvalidData("非法块类型 3")
        );
    }
}
