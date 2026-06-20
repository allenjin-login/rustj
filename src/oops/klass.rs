//! 已解析类(对应 HotSpot `instanceKlass`)+ 类注册表。
//!
//! 从 [`ClassFile`] 计算**实例字段布局**(声明序,每字段一槽)与**静态字段存储**
//! (默认初始化)。4.1 仅本类字段(`java/lang/Object` 无实例字段);多层继承字段
//! 叠加留待 4.2(随类层次与虚分派)。

use std::collections::HashMap;

use crate::classfile::ClassFileError;
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::{parse_field_descriptor, FieldType};
use crate::metadata::{ClassFile, FieldInfo};
use crate::runtime::Slot;

use super::instance::InstanceOop;

/// 一个已解析字段:名 + 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedField {
    pub name: String,
    pub descriptor: FieldType,
}

/// 已加载的类:`ClassFile` + 实例/静态字段布局 + 静态字段存储。
pub struct LoadedClass {
    pub cf: ClassFile,
    instance_fields: Vec<ResolvedField>,
    static_fields: Vec<ResolvedField>,
    pub static_storage: Vec<Slot>,
}

impl LoadedClass {
    /// 类内部名。
    pub fn name(&self) -> &str {
        self.cf.this_class_name().unwrap_or("")
    }

    /// 实例字段(声明序)。
    pub fn instance_fields(&self) -> &[ResolvedField] {
        &self.instance_fields
    }

    /// 静态字段(声明序)。
    pub fn static_fields(&self) -> &[ResolvedField] {
        &self.static_fields
    }

    /// 按名 + 类型定位实例字段 → 序号(实例槽位序)。
    pub fn instance_field(&self, name: &str, ft: &FieldType) -> Option<usize> {
        self.instance_fields
            .iter()
            .position(|f| f.name == name && f.descriptor == *ft)
    }

    /// 按名 + 类型定位静态字段 → 序号(`static_storage` 索引)。
    pub fn static_field(&self, name: &str, ft: &FieldType) -> Option<usize> {
        self.static_fields
            .iter()
            .position(|f| f.name == name && f.descriptor == *ft)
    }

    /// 构造一个默认初始化的实例(所有字段置零/null)。
    pub fn new_instance(&self) -> InstanceOop {
        let fields = default_fields(&self.instance_fields);
        InstanceOop::new(self.name().to_string(), fields)
    }

    /// 从 `ClassFile` 解析字段布局(本类字段;静态字段默认初始化)。
    fn from_cf(cf: ClassFile) -> Result<Self, ClassFileError> {
        let layout = resolve_fields(&cf.constant_pool, &cf.fields)?;
        Ok(Self {
            cf,
            instance_fields: layout.instance_fields,
            static_fields: layout.static_fields,
            static_storage: layout.static_storage,
        })
    }
}

/// 解析后的字段布局:实例字段、静态字段及其默认初始化的存储。
struct FieldLayout {
    instance_fields: Vec<ResolvedField>,
    static_fields: Vec<ResolvedField>,
    static_storage: Vec<Slot>,
}

/// 类注册表:按内部名索引的已加载类。
pub struct ClassRegistry {
    classes: HashMap<String, LoadedClass>,
}

impl ClassRegistry {
    pub fn new() -> Self {
        Self {
            classes: HashMap::new(),
        }
    }

    /// 加载(解析字段布局)并按 `this_class_name` 注册;返回已注册类的引用。
    pub fn load(&mut self, cf: ClassFile) -> Result<&LoadedClass, ClassFileError> {
        let name = cf
            .this_class_name()
            .ok_or(ClassFileError::Unsupported("类缺少 this_class 名"))?
            .to_string();
        let lc = LoadedClass::from_cf(cf)?;
        Ok(self.classes.entry(name).or_insert(lc))
    }

    /// 按内部名取已加载类。
    pub fn get(&self, name: &str) -> Option<&LoadedClass> {
        self.classes.get(name)
    }
}

impl Default for ClassRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 取常量池中 `Utf8` 条目的字符串。
fn utf8(cp: &ConstantPool, index: u16) -> Result<String, ClassFileError> {
    match cp.get(index)? {
        ConstantPoolEntry::Utf8(s) => Ok(s.clone()),
        _ => Err(ClassFileError::Unsupported("字段名/描述符须为 Utf8 条目")),
    }
}

/// 字段类型的默认槽(零值/null)。
fn default_slot(ft: &FieldType) -> Slot {
    match ft {
        FieldType::Long => Slot::Long(0),
        FieldType::Float => Slot::Float(0.0),
        FieldType::Double => Slot::Double(0.0),
        // byte/char/short/boolean 在运行时均以 int 承载,故默认 Int(0)。
        FieldType::Int | FieldType::Byte | FieldType::Char | FieldType::Short
        | FieldType::Boolean => Slot::Int(0),
        FieldType::Class(_) | FieldType::Array(_) => Slot::Reference(crate::runtime::Reference::null()),
    }
}

/// 一组字段的默认槽。
fn default_fields(fields: &[ResolvedField]) -> Vec<Slot> {
    fields.iter().map(|f| default_slot(&f.descriptor)).collect()
}

/// 从字段表解析实例/静态字段布局,并默认初始化静态存储。
///
/// 非静态入实例,静态入静态并附默认值。返回 [`FieldLayout`]。
fn resolve_fields(cp: &ConstantPool, fields: &[FieldInfo]) -> Result<FieldLayout, ClassFileError> {
    let mut instance_fields = Vec::new();
    let mut static_fields = Vec::new();
    for f in fields {
        let name = utf8(cp, f.name_index)?;
        let desc_str = utf8(cp, f.descriptor_index)?;
        let descriptor = parse_field_descriptor(&desc_str)?;
        let resolved = ResolvedField { name, descriptor };
        if f.access_flags.is_static() {
            static_fields.push(resolved);
        } else {
            instance_fields.push(resolved);
        }
    }
    // 静态字段默认初始化(零/null);与 static_fields 同序。
    let static_storage = default_fields(&static_fields);
    Ok(FieldLayout {
        instance_fields,
        static_fields,
        static_storage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classfile::Reader;
    use crate::metadata::access_flags::ACC_STATIC;
    use crate::metadata::AccessFlags;
    use crate::runtime::Reference;

    /// CP:`[1]="x"` `[2]="I"` `[3]="y"` `[4]="J"` `[5]="count"` `[6]="I"`。
    fn cp_with_names() -> ConstantPool {
        let bytes = [
            0x00, 0x07, // count=7
            0x01, 0x00, 0x01, b'x', // [1] "x"
            0x01, 0x00, 0x01, b'I', // [2] "I"
            0x01, 0x00, 0x01, b'y', // [3] "y"
            0x01, 0x00, 0x01, b'J', // [4] "J"
            0x01, 0x00, 0x05, b'c', b'o', b'u', b'n', b't', // [5] "count"
            0x01, 0x00, 0x01, b'I', // [6] "I"
        ];
        ConstantPool::parse(&mut Reader::new(&bytes)).unwrap()
    }

    fn field(flags: u16, name: u16, desc: u16) -> FieldInfo {
        FieldInfo {
            access_flags: AccessFlags::from_bits(flags),
            name_index: name,
            descriptor_index: desc,
            attributes: Vec::new(),
        }
    }

    #[test]
    fn default_slot_matches_type() {
        assert_eq!(default_slot(&FieldType::Int), Slot::Int(0));
        assert_eq!(default_slot(&FieldType::Byte), Slot::Int(0));
        assert_eq!(default_slot(&FieldType::Boolean), Slot::Int(0));
        assert_eq!(default_slot(&FieldType::Long), Slot::Long(0));
        assert_eq!(default_slot(&FieldType::Float), Slot::Float(0.0));
        assert_eq!(default_slot(&FieldType::Double), Slot::Double(0.0));
        assert_eq!(
            default_slot(&FieldType::Class("java/lang/String".into())),
            Slot::Reference(Reference::null())
        );
        assert_eq!(
            default_slot(&FieldType::Array(Box::new(FieldType::Int))),
            Slot::Reference(Reference::null())
        );
    }

    #[test]
    fn resolve_fields_splits_instance_and_static() {
        let cp = cp_with_names();
        let fields = [
            field(0, 1, 2), // x:I 实例
            field(0, 3, 4), // y:J 实例
            field(ACC_STATIC, 5, 6), // count:I 静态
        ];
        let (inst, stat, storage) = {
            let l = resolve_fields(&cp, &fields).unwrap();
            (l.instance_fields, l.static_fields, l.static_storage)
        };
        assert_eq!(inst.len(), 2);
        assert_eq!(inst[0].name, "x");
        assert_eq!(inst[0].descriptor, FieldType::Int);
        assert_eq!(inst[1].name, "y");
        assert_eq!(inst[1].descriptor, FieldType::Long);
        assert_eq!(stat.len(), 1);
        assert_eq!(stat[0].name, "count");
        assert_eq!(stat[0].descriptor, FieldType::Int);
        assert_eq!(storage, vec![Slot::Int(0)]); // 静态默认值
    }
}
