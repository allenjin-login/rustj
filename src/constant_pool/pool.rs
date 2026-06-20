//! 常量池容器与解析。对应 HotSpot `oops/constantPool.cpp` 中 class 文件常量池的解析部分。

use crate::classfile::{ClassFileError, Reader};

use super::entry::ConstantPoolEntry;
use super::tag::ConstantTag;

/// 已解析的常量池。条目按 class 文件中的 1-based 索引存放(`entries[0]` 为占位)。
#[derive(Debug, Clone, PartialEq)]
pub struct ConstantPool {
    /// `entries[0]` 恒为 `Unusable` 占位,使 1-based 索引可直接映射。
    entries: Vec<ConstantPoolEntry>,
}

impl ConstantPool {
    /// 从 class 文件常量池区段解析。读取 `constant_pool_count` 及其后所有条目。
    pub fn parse(reader: &mut Reader) -> Result<Self, ClassFileError> {
        let count = reader.u2()?;
        // entries[0] 占位,使 1-based 索引可直接映射。
        let mut entries: Vec<ConstantPoolEntry> = Vec::with_capacity(usize::from(count));
        entries.push(ConstantPoolEntry::Unusable);

        let mut index: u16 = 1;
        while index < count {
            let tag = ConstantTag::from_u8(reader.u1()?)?;
            let entry = Self::parse_entry(reader, tag)?;
            let slots = entry.slot_count();
            entries.push(entry);
            index = index
                .checked_add(slots as u16)
                .ok_or(ClassFileError::InvalidConstantPoolTag(tag as u8))?;
            // Long/Double 多占一个槽位:补一个 Unusable 占位。
            if slots == 2 {
                entries.push(ConstantPoolEntry::Unusable);
            }
        }
        Ok(Self { entries })
    }

    /// 按 1-based 索引取条目的引用。索引 0 及越界均返回错误。
    pub fn get(&self, index: u16) -> Result<&ConstantPoolEntry, ClassFileError> {
        let i = usize::from(index);
        if i == 0 || i >= self.entries.len() {
            return Err(ClassFileError::BadConstantPoolIndex {
                index,
                length: (self.entries.len() - 1) as u16,
            });
        }
        Ok(&self.entries[i])
    }

    /// 解析单个常量池条目(不含标签字节,标签已由调用方读取)。
    fn parse_entry(reader: &mut Reader, tag: ConstantTag) -> Result<ConstantPoolEntry, ClassFileError> {
        Ok(match tag {
            ConstantTag::Utf8 => {
                let len = usize::from(reader.u2()?);
                ConstantPoolEntry::Utf8(reader.modified_utf8(len)?)
            }
            ConstantTag::Integer => ConstantPoolEntry::Integer(reader.u4()? as i32),
            ConstantTag::Float => ConstantPoolEntry::Float(f32::from_bits(reader.u4()?)),
            ConstantTag::Long => {
                let hi = u64::from(reader.u4()?);
                let lo = u64::from(reader.u4()?);
                ConstantPoolEntry::Long(((hi << 32) | lo) as i64)
            }
            ConstantTag::Double => {
                let hi = u64::from(reader.u4()?);
                let lo = u64::from(reader.u4()?);
                ConstantPoolEntry::Double(f64::from_bits((hi << 32) | lo))
            }
            ConstantTag::Class => ConstantPoolEntry::Class {
                name_index: reader.u2()?,
            },
            ConstantTag::String => ConstantPoolEntry::String {
                string_index: reader.u2()?,
            },
            ConstantTag::Fieldref => ConstantPoolEntry::Fieldref {
                class_index: reader.u2()?,
                name_and_type_index: reader.u2()?,
            },
            ConstantTag::Methodref => ConstantPoolEntry::Methodref {
                class_index: reader.u2()?,
                name_and_type_index: reader.u2()?,
            },
            ConstantTag::InterfaceMethodref => ConstantPoolEntry::InterfaceMethodref {
                class_index: reader.u2()?,
                name_and_type_index: reader.u2()?,
            },
            ConstantTag::NameAndType => ConstantPoolEntry::NameAndType {
                name_index: reader.u2()?,
                descriptor_index: reader.u2()?,
            },
            ConstantTag::MethodHandle => ConstantPoolEntry::MethodHandle {
                reference_kind: reader.u1()?,
                reference_index: reader.u2()?,
            },
            ConstantTag::MethodType => ConstantPoolEntry::MethodType {
                descriptor_index: reader.u2()?,
            },
            ConstantTag::Dynamic => ConstantPoolEntry::Dynamic {
                bootstrap_method_attr_index: reader.u2()?,
                name_and_type_index: reader.u2()?,
            },
            ConstantTag::InvokeDynamic => ConstantPoolEntry::InvokeDynamic {
                bootstrap_method_attr_index: reader.u2()?,
                name_and_type_index: reader.u2()?,
            },
            ConstantTag::Module => ConstantPoolEntry::Module {
                name_index: reader.u2()?,
            },
            ConstantTag::Package => ConstantPoolEntry::Package {
                name_index: reader.u2()?,
            },
        })
    }

    /// 常量池槽位数(= `constant_pool_count - 1`)。
    pub fn len(&self) -> usize {
        self.entries.len() - 1
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 迭代所有条目(1-based)。
    pub fn iter(&self) -> impl Iterator<Item = (u16, &ConstantPoolEntry)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, e)| (i as u16, e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be_u2(v: u16) -> [u8; 2] {
        v.to_be_bytes()
    }
    fn be_u4(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    #[test]
    fn parses_single_utf8_entry() {
        // constant_pool_count = 2 -> 只有索引 1
        let bytes = [
            0x00, 0x02, // count
            0x01, // Utf8
            0x00, 0x02, b'H', b'i',
        ];
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(
            pool.get(1).unwrap(),
            &ConstantPoolEntry::Utf8("Hi".to_string())
        );
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn long_takes_two_slots_and_next_is_unusable() {
        // count = 3 -> 索引1=Long, 索引2=占位
        let mut bytes = vec![0x00, 0x03, 0x05]; // count=3, tag Long
        bytes.extend_from_slice(&be_u4(1)); // high
        bytes.extend_from_slice(&be_u4(2)); // low
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(pool.get(1).unwrap(), &ConstantPoolEntry::Long((1i64 << 32) | 2));
        assert_eq!(pool.get(2).unwrap(), &ConstantPoolEntry::Unusable);
        assert!(pool.get(3).is_err());
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn index_zero_is_invalid() {
        let bytes = [0x00, 0x02, 0x01, 0x00, 0x01, b'x']; // one Utf8("x")
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(
            pool.get(0).unwrap_err(),
            ClassFileError::BadConstantPoolIndex { index: 0, length: 1 }
        );
    }

    #[test]
    fn parses_integer_and_float() {
        // count=3: [1]=Integer(0x12345678), [2]=Float
        let mut bytes = vec![0x00, 0x03, 0x03]; // count=3, Integer
        bytes.extend_from_slice(&be_u4(0x1234_5678));
        bytes.push(0x04); // Float
        bytes.extend_from_slice(&be_u4(0x4020_0000)); // 2.5f
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(pool.get(1).unwrap(), &ConstantPoolEntry::Integer(0x1234_5678_i32));
        match pool.get(2).unwrap() {
            ConstantPoolEntry::Float(f) => assert!((f - 2.5_f32).abs() < 0.01),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn parses_class_nameandtype_methodref_chain() {
        // 构造:
        //  count=5
        //  [1] Utf8("java/lang/Object")
        //  [2] Utf8("toString")
        //  [3] Utf8("()Ljava/lang/String;")
        //  [4] Methodref{class=..., nat=...} 这里用占位索引 1, 4 本身不合法但仅测解析
        // 为简化,直接测 Methodref 字段解析:
        let mut bytes = vec![0x00, 0x02, 0x0A]; // count=2, Methodref
        bytes.extend_from_slice(&be_u2(5)); // class_index
        bytes.extend_from_slice(&be_u2(6)); // name_and_type_index
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(
            pool.get(1).unwrap(),
            &ConstantPoolEntry::Methodref {
                class_index: 5,
                name_and_type_index: 6,
            }
        );
    }

    #[test]
    fn parses_name_and_type() {
        let mut bytes = vec![0x00, 0x02, 0x0C]; // count=2, NameAndType
        bytes.extend_from_slice(&be_u2(3));
        bytes.extend_from_slice(&be_u2(4));
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(
            pool.get(1).unwrap(),
            &ConstantPoolEntry::NameAndType {
                name_index: 3,
                descriptor_index: 4,
            }
        );
    }

    #[test]
    fn parses_method_handle() {
        let mut bytes = vec![0x00, 0x02, 0x0F]; // count=2, MethodHandle
        bytes.push(6); // REF_invokeStatic
        bytes.extend_from_slice(&be_u2(7));
        let mut r = Reader::new(&bytes);
        let pool = ConstantPool::parse(&mut r).unwrap();
        assert_eq!(
            pool.get(1).unwrap(),
            &ConstantPoolEntry::MethodHandle {
                reference_kind: 6,
                reference_index: 7,
            }
        );
    }

    #[test]
    fn rejects_truncated_pool() {
        // count=2 但后面没有任何数据
        let bytes = [0x00, 0x02];
        let mut r = Reader::new(&bytes);
        assert!(matches!(
            ConstantPool::parse(&mut r),
            Err(ClassFileError::Truncated { .. })
        ));
    }
}
