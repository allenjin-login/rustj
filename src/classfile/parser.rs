//! 顶层 class 文件解析器。对应 HotSpot `ClassFileParser::parse_stream`。
//!
//! 按 JVMS §4 的顺序读取所有区段,产出 [`ClassFile`]。
//! 解析完常量池后,顺带深解析每个方法的 `Code` 属性。

use crate::classfile::attributes::parse_attributes;
use crate::classfile::{ClassFileError, Reader};
use crate::constant_pool::ConstantPool;
use crate::metadata::access_flags::AccessFlags;
use crate::metadata::class_file::ClassFile;
use crate::metadata::field::FieldInfo;
use crate::metadata::method::MethodInfo;

/// class 文件魔数。
pub const MAGIC: u32 = 0xCAFEBABE;

/// 解析整份 class 文件字节。
pub fn parse(bytes: &[u8]) -> Result<ClassFile, ClassFileError> {
    let mut reader = Reader::new(bytes);
    parse_from_reader(&mut reader)
}

/// 从读取器解析 class 文件(允许字节流后面还有数据)。
pub fn parse_from_reader(reader: &mut Reader) -> Result<ClassFile, ClassFileError> {
    let magic = reader.u4()?;
    if magic != MAGIC {
        return Err(ClassFileError::BadMagic { actual: magic });
    }
    let minor_version = reader.u2()?;
    let major_version = reader.u2()?;

    let constant_pool = ConstantPool::parse(reader)?;

    let access_flags = AccessFlags::from_bits(reader.u2()?);
    let this_class = reader.u2()?;
    let super_class = reader.u2()?;

    let interfaces_count = usize::from(reader.u2()?);
    let mut interfaces = Vec::with_capacity(interfaces_count);
    for _ in 0..interfaces_count {
        interfaces.push(reader.u2()?);
    }

    let fields_count = usize::from(reader.u2()?);
    let mut fields = Vec::with_capacity(fields_count);
    for _ in 0..fields_count {
        fields.push(FieldInfo::parse(reader)?);
    }

    let methods_count = usize::from(reader.u2()?);
    let mut methods = Vec::with_capacity(methods_count);
    for _ in 0..methods_count {
        methods.push(MethodInfo::parse(reader)?);
    }

    let attributes = parse_attributes(reader)?;

    // 常量池已就绪:深解析每个方法的 Code 属性。
    for method in &mut methods {
        method.resolve_code(&constant_pool)?;
    }

    Ok(ClassFile {
        minor_version,
        major_version,
        constant_pool,
        access_flags,
        this_class,
        super_class,
        interfaces,
        fields,
        methods,
        attributes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小但合法的 class 文件字节:
    /// class Foo extends java/lang/Object { public static void main(); }
    fn minimal_class() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_be_bytes()); // magic
        b.extend_from_slice(&0u16.to_be_bytes()); // minor
        b.extend_from_slice(&52u16.to_be_bytes()); // major (Java 8)
        // 常量池
        b.extend_from_slice(&8u16.to_be_bytes()); // count=8
        b.extend_from_slice(&[0x01, 0x00, 0x03, b'F', b'o', b'o']); // [1] Utf8 "Foo"
        b.extend_from_slice(&[0x07, 0x00, 0x01]); // [2] Class{1}
        b.extend_from_slice(&[0x01, 0x00, 0x10]); // [3] Utf8 len=16
        b.extend_from_slice(b"java/lang/Object");
        b.extend_from_slice(&[0x07, 0x00, 0x03]); // [4] Class{3}
        b.extend_from_slice(&[0x01, 0x00, 0x04, b'm', b'a', b'i', b'n']); // [5] Utf8 "main"
        b.extend_from_slice(&[0x01, 0x00, 0x03, b'(', b')', b'V']); // [6] Utf8 "()V"
        b.extend_from_slice(&[0x01, 0x00, 0x04, b'C', b'o', b'd', b'e']); // [7] Utf8 "Code"
        // access_flags = PUBLIC|SUPER = 0x0021
        b.extend_from_slice(&0x0021u16.to_be_bytes());
        b.extend_from_slice(&2u16.to_be_bytes()); // this_class
        b.extend_from_slice(&4u16.to_be_bytes()); // super_class
        b.extend_from_slice(&0u16.to_be_bytes()); // interfaces_count
        b.extend_from_slice(&0u16.to_be_bytes()); // fields_count
        b.extend_from_slice(&1u16.to_be_bytes()); // methods_count
        // method main: access=PUBLIC|STATIC=0x0009, name=5, desc=6, attrs=1
        b.extend_from_slice(&0x0009u16.to_be_bytes());
        b.extend_from_slice(&5u16.to_be_bytes());
        b.extend_from_slice(&6u16.to_be_bytes());
        b.extend_from_slice(&1u16.to_be_bytes()); // attributes_count
        // Code attr: name=7, length=12, info=max_stack0 max_locals0 code_len0 exc0 attrs0
        b.extend_from_slice(&7u16.to_be_bytes());
        b.extend_from_slice(&12u32.to_be_bytes());
        b.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);
        b.extend_from_slice(&0u16.to_be_bytes()); // class attributes_count
        b
    }

    #[test]
    fn parses_minimal_class_structure() {
        let bytes = minimal_class();
        let cf = parse(&bytes).unwrap();
        assert_eq!(cf.major_version, 52);
        assert_eq!(cf.minor_version, 0);
        assert_eq!(cf.this_class_name(), Some("Foo"));
        assert_eq!(cf.super_class_name(), Some("java/lang/Object"));
        assert!(cf.fields.is_empty());
        assert_eq!(cf.methods.len(), 1);
        assert_eq!(cf.methods[0].name_index, 5);
        // Code 属性已被深解析
        let code = cf.methods[0].code.as_ref().expect("code resolved");
        assert_eq!(code.max_stack, 0);
        assert_eq!(code.max_locals, 0);
        assert!(code.code.is_empty());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = minimal_class();
        bytes[0..4].copy_from_slice(&0xDEADBEEFu32.to_be_bytes());
        let err = parse(&bytes).unwrap_err();
        assert_eq!(err, ClassFileError::BadMagic { actual: 0xDEADBEEF });
    }

    #[test]
    fn rejects_truncated() {
        // 只有 magic + 半个 version
        let bytes = [0xCA, 0xFE, 0xBA, 0xBE, 0x00];
        assert!(matches!(
            parse(&bytes),
            Err(ClassFileError::Truncated { .. })
        ));
    }
}
