//! 顶层 `ClassFile` 结构:把常量池、字段、方法、属性汇总。
//!
//! 对应 HotSpot `ClassFileParser` 产出的 `InstanceKlass`(本层只保留元数据,
//! 不含运行时状态)。

use crate::classfile::attributes::Attribute;
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};

use super::access_flags::AccessFlags;
use super::field::FieldInfo;
use super::method::MethodInfo;

/// 已解析的 class 文件。
#[derive(Debug, Clone)]
pub struct ClassFile {
    pub minor_version: u16,
    pub major_version: u16,
    pub constant_pool: ConstantPool,
    pub access_flags: AccessFlags,
    /// 本类的常量池索引(指向 `Class` 条目)。
    pub this_class: u16,
    /// 父类的常量池索引;`java/lang/Object` 为 0。
    pub super_class: u16,
    /// 直接实现的接口的常量池索引列表。
    pub interfaces: Vec<u16>,
    pub fields: Vec<FieldInfo>,
    pub methods: Vec<MethodInfo>,
    pub attributes: Vec<Attribute>,
}

impl ClassFile {
    /// 本类的内部名(如 `"java/lang/String"`)。结构异常时返回 `None`。
    pub fn this_class_name(&self) -> Option<&str> {
        self.class_name_at(self.this_class)
    }

    /// 父类的内部名;`java/lang/Object` 或结构异常时返回 `None`。
    pub fn super_class_name(&self) -> Option<&str> {
        if self.super_class == 0 {
            return None;
        }
        self.class_name_at(self.super_class)
    }

    fn class_name_at(&self, class_index: u16) -> Option<&str> {
        let entry = self.constant_pool.get(class_index).ok()?;
        let ConstantPoolEntry::Class { name_index } = entry else {
            return None;
        };
        match self.constant_pool.get(*name_index).ok()? {
            ConstantPoolEntry::Utf8(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// 类级 `SourceFile` 属性(JVMS §4.7.10)指向的源文件名(如 `"Math.java"`)。
    /// 懒扫 `self.attributes` 按名 "SourceFile"(经 cp 识名),取体 `u2 sourcefile_index` → Utf8。
    /// 无该属性 / 结构异常 → `None`。供栈轨迹行号渲染 `at Class.method(File.java:LINE)`。
    pub fn source_file_name(&self) -> Option<&str> {
        for attr in &self.attributes {
            let Ok(ConstantPoolEntry::Utf8(name)) = self.constant_pool.get(attr.name_index)
            else {
                continue;
            };
            if name != "SourceFile" {
                continue;
            }
            if attr.info.len() < 2 {
                return None;
            }
            let idx = u16::from_be_bytes([attr.info[0], attr.info[1]]);
            match self.constant_pool.get(idx).ok()? {
                ConstantPoolEntry::Utf8(file) => return Some(file.as_str()),
                _ => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造常量池:[1]=Utf8("Foo"), [2]=Class{1}, [3]=Utf8("Bar"), [4]=Class{3}
    fn pool() -> ConstantPool {
        let bytes = [
            0x00, 0x05, // count
            0x01, 0x00, 0x03, b'F', b'o', b'o', // [1] "Foo"
            0x07, 0x00, 0x01, // [2] Class{1}
            0x01, 0x00, 0x03, b'B', b'a', b'r', // [3] "Bar"
            0x07, 0x00, 0x03, // [4] Class{3}
        ];
        let mut r = crate::classfile::Reader::new(&bytes);
        ConstantPool::parse(&mut r).unwrap()
    }

    #[test]
    fn resolves_this_and_super_class_names() {
        let cp = pool();
        let cf = ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp,
            access_flags: AccessFlags::from_bits(0x0021),
            this_class: 2,
            super_class: 4,
            interfaces: vec![],
            fields: vec![],
            methods: vec![],
            attributes: vec![],
        };
        assert_eq!(cf.this_class_name(), Some("Foo"));
        assert_eq!(cf.super_class_name(), Some("Bar"));
    }

    #[test]
    fn super_zero_is_object() {
        let cp = pool();
        let cf = ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp,
            access_flags: AccessFlags::from_bits(0),
            this_class: 2,
            super_class: 0,
            interfaces: vec![],
            fields: vec![],
            methods: vec![],
            attributes: vec![],
        };
        assert_eq!(cf.this_class_name(), Some("Foo"));
        assert_eq!(cf.super_class_name(), None);
    }
}
