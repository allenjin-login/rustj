//! 已解析类(对应 HotSpot `instanceKlass`)+ 类注册表。
//!
//! 从 [`ClassFile`] 计算**实例字段布局**(声明序,每字段一槽)与**静态字段存储**
//! (默认初始化)。4.1 仅本类字段(`java/lang/Object` 无实例字段);多层继承字段
//! 叠加留待 4.2(随类层次与虚分派)。

use std::collections::{HashMap, HashSet, VecDeque};

use std::cell::RefCell;

use crate::classfile::ClassFileError;
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::{parse_field_descriptor, FieldType};
use crate::metadata::{ClassFile, FieldInfo, MethodInfo};
use crate::runtime::Slot;

use super::instance::InstanceOop;

/// 一个已解析字段:名 + 类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedField {
    pub name: String,
    pub descriptor: FieldType,
}

/// 已加载的类:`ClassFile` + 本类实例/静态字段布局 + 静态字段存储 + 超类关系。
///
/// `instance_fields` 为**本类声明**的实例字段;**继承字段**经
/// [`ClassRegistry::flattened_instance_fields`] 惰性扁平化(超类链 ++ 本类)并缓存于 `flat_cache`。
///
/// `static_storage` 用 [`RefCell`] 承载:静态字段是**类级可变状态**(putstatic 写入),
/// 对应 HotSpot `InstanceKlass` 中就地持有的静态字段区;注册表以不可变引用暴露
/// `LoadedClass`,静态字段经 `RefCell` 内部可变性写入。
pub struct LoadedClass {
    pub cf: ClassFile,
    instance_fields: Vec<ResolvedField>,
    static_fields: Vec<ResolvedField>,
    pub static_storage: RefCell<Vec<Slot>>,
    super_class_name: Option<String>,
    flat_cache: RefCell<Option<Vec<ResolvedField>>>,
}

impl LoadedClass {
    /// 类内部名。
    pub fn name(&self) -> &str {
        self.cf.this_class_name().unwrap_or("")
    }

    /// 实例字段(本类声明序)。
    pub fn instance_fields(&self) -> &[ResolvedField] {
        &self.instance_fields
    }

    /// 静态字段(声明序)。
    pub fn static_fields(&self) -> &[ResolvedField] {
        &self.static_fields
    }

    /// 按名 + 类型定位**静态**字段 → 序号(`static_storage` 索引)。静态字段不扁平化
    /// (归属声明类;getstatic/putstatic 的 Fieldref 已指向声明类)。
    pub fn static_field(&self, name: &str, ft: &FieldType) -> Option<usize> {
        self.static_fields
            .iter()
            .position(|f| f.name == name && f.descriptor == *ft)
    }

    /// 超类内部名;`None` 表示 `java/lang/Object` 或无超类。
    pub fn super_class_name(&self) -> Option<&str> {
        self.super_class_name.as_deref()
    }

    /// 直接实现的接口内部名(解析 `cf.interfaces` 的 `Class` 条目)。
    pub fn interface_names(&self) -> Vec<String> {
        let cp = &self.cf.constant_pool;
        self.cf
            .interfaces
            .iter()
            .filter_map(|&idx| match cp.get(idx).ok()? {
                ConstantPoolEntry::Class { name_index } => match cp.get(*name_index).ok()? {
                    ConstantPoolEntry::Utf8(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// 从 `ClassFile` 解析字段布局(本类字段;静态字段默认初始化)+ 超类名。
    fn from_cf(cf: ClassFile) -> Result<Self, ClassFileError> {
        let layout = resolve_fields(&cf.constant_pool, &cf.fields)?;
        let super_class_name = cf.super_class_name().map(String::from);
        Ok(Self {
            cf,
            instance_fields: layout.instance_fields,
            static_fields: layout.static_fields,
            static_storage: RefCell::new(layout.static_storage),
            super_class_name,
            flat_cache: RefCell::new(None),
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

    /// 扁平化实例字段(超类链 ++ 本类),惰性缓存于 `flat_cache`。
    ///
    /// 超类链置前、本类在后;`java/lang/Object`(未加载)作根终止。解耦加载顺序。
    pub fn flattened_instance_fields(&self, lc: &LoadedClass) -> Vec<ResolvedField> {
        if let Some(cached) = lc.flat_cache.borrow().clone() {
            return cached;
        }
        let mut fields = Vec::new();
        if let Some(super_name) = &lc.super_class_name
            && super_name.as_str() != "java/lang/Object"
            && let Some(super_lc) = self.get(super_name)
        {
            fields.extend(self.flattened_instance_fields(super_lc));
        }
        fields.extend(lc.instance_fields.iter().cloned());
        *lc.flat_cache.borrow_mut() = Some(fields.clone());
        fields
    }

    /// 按名 + 类型在 lc 的**扁平布局**中定位实例字段 → 全局序号(与实际对象对齐)。
    pub fn instance_field(
        &self,
        lc: &LoadedClass,
        name: &str,
        ft: &FieldType,
    ) -> Option<usize> {
        self.flattened_instance_fields(lc)
            .iter()
            .position(|f| f.name == name && f.descriptor == *ft)
    }

    /// 创建默认初始化实例(扁平布局全零/null)。对应 `new`。
    pub fn new_instance(&self, lc: &LoadedClass) -> InstanceOop {
        let fields = default_fields(&self.flattened_instance_fields(lc));
        InstanceOop::new(lc.name().to_string(), fields)
    }

    /// 虚分派:从 `class_name` 沿超类链找首个 (name, desc) 方法 → (声明类, 方法)。
    ///
    /// 对应 HotSpot `InstanceKlass::find_method` 上行查找(我们用线性查找,不用 vtable)。
    pub fn find_virtual_method<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        let mut current = self.get(class_name);
        while let Some(lc) = current {
            if let Some(m) = lc
                .cf
                .methods
                .iter()
                .find(|m| method_matches(&lc.cf, m, name, desc))
            {
                return Some((lc, m));
            }
            current = lc
                .super_class_name
                .as_deref()
                .filter(|s| *s != "java/lang/Object")
                .and_then(|s| self.get(s));
        }
        None
    }

    /// 在 `class_name`(单类,非链)内精确查找 (name, desc) 方法 → (类, 方法)。
    /// 用于 invokespecial 的私有精确判定与 super 虚查起点。
    pub fn find_exact_method<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        let lc = self.get(class_name)?;
        lc.cf
            .methods
            .iter()
            .find(|m| method_matches(&lc.cf, m, name, desc))
            .map(|m| (lc, m))
    }

    /// 接口 default 方法查找:沿 `class_name` 类层次所有传递实现接口 BFS,
    /// 找首个**带 Code** 的 (name, desc) → (声明接口类, 方法)。
    /// 类链已由调用方查过;此仅兜底 default(抽象方法跳过,继续搜索)。
    pub fn find_default_method<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut visited: HashSet<String> = HashSet::new();
        // 种子:`class_name` 及其超类链上每类的直接接口。
        let mut cur = self.get(class_name);
        while let Some(lc) = cur {
            for iface in lc.interface_names() {
                if visited.insert(iface.clone()) {
                    queue.push_back(iface);
                }
            }
            cur = lc
                .super_class_name()
                .filter(|s| *s != "java/lang/Object")
                .and_then(|s| self.get(s));
        }
        // BFS 接口闭包,跳过抽象,命中带 Code 的 default。
        while let Some(iface_name) = queue.pop_front() {
            if let Some(iface_lc) = self.get(&iface_name) {
                if let Some(m) = iface_lc
                    .cf
                    .methods
                    .iter()
                    .find(|m| method_matches(&iface_lc.cf, m, name, desc) && m.code.is_some())
                {
                    return Some((iface_lc, m));
                }
                for super_iface in iface_lc.interface_names() {
                    if visited.insert(super_iface.clone()) {
                        queue.push_back(super_iface);
                    }
                }
            }
        }
        None
    }

    /// 虚/接口分派解析:类链先行(`find_virtual_method`),落空走接口 default
    /// (`find_default_method`)。命中抽象类方法(无 Code)时仍返回(由调用方判
    /// `AbstractMethodError`);default 路径必带 Code。
    pub fn resolve_dispatch<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        if let Some(hit) = self.find_virtual_method(class_name, name, desc) {
            return Some(hit);
        }
        self.find_default_method(class_name, name, desc)
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

/// 方法名与描述符是否同时匹配(查常量池 Utf8)。
fn method_matches(cf: &ClassFile, m: &MethodInfo, name: &str, desc: &str) -> bool {
    let name_ok = matches!(
        cf.constant_pool.get(m.name_index),
        Ok(ConstantPoolEntry::Utf8(n)) if n == name
    );
    let desc_ok = matches!(
        cf.constant_pool.get(m.descriptor_index),
        Ok(ConstantPoolEntry::Utf8(d)) if d == desc
    );
    name_ok && desc_ok
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
    use crate::metadata::access_flags::{ACC_PUBLIC, ACC_STATIC};
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

    /// 构建常量池:先放 utf8s(索引从 1 起),再放 classes(每个 = 指向某 utf8 索引的 Class 条目)。
    fn mk_cp(utf8s: &[&str], classes: &[u16]) -> ConstantPool {
        let count = (utf8s.len() + classes.len() + 1) as u16;
        let mut b = count.to_be_bytes().to_vec();
        for s in utf8s {
            b.push(0x01); // Utf8
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        for &name_idx in classes {
            b.push(0x07); // Class
            b.extend_from_slice(&name_idx.to_be_bytes());
        }
        ConstantPool::parse(&mut Reader::new(&b)).unwrap()
    }

    /// 构造 ClassFile(空字段表;方法表可填)。
    fn mk_cf(
        cp: ConstantPool,
        this: u16,
        super_c: u16,
        interfaces: Vec<u16>,
        methods: Vec<MethodInfo>,
    ) -> ClassFile {
        ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp,
            access_flags: AccessFlags::from_bits(0),
            this_class: this,
            super_class: super_c,
            interfaces,
            fields: Vec::new(),
            methods,
            attributes: Vec::new(),
        }
    }

    #[test]
    fn interface_names_resolves_cp_class_entries() {
        // utf8: [1]="C",[2]="java/lang/Object",[3]="I1",[4]="I2"
        // classes[1,2,3,4] → [5]=Class{1}="C",[6]=Class{2}=Object,[7]=Class{3}="I1",[8]=Class{4}="I2"
        let pool = mk_cp(&["C", "java/lang/Object", "I1", "I2"], &[1, 2, 3, 4]);
        let cf = mk_cf(pool, 5, 6, vec![7, 8], vec![]);
        let lc = LoadedClass::from_cf(cf).unwrap();
        assert_eq!(
            lc.interface_names(),
            vec!["I1".to_string(), "I2".to_string()]
        );
    }

    use crate::classfile::attributes::CodeAttribute;

    fn default_code() -> CodeAttribute {
        CodeAttribute {
            max_stack: 0,
            max_locals: 0,
            code: Vec::new(),
            exception_table: Vec::new(),
            attributes: Vec::new(),
        }
    }

    fn mk_method(name_idx: u16, desc_idx: u16, code: Option<CodeAttribute>) -> MethodInfo {
        MethodInfo {
            access_flags: AccessFlags::from_bits(ACC_PUBLIC),
            name_index: name_idx,
            descriptor_index: desc_idx,
            attributes: Vec::new(),
            code,
        }
    }

    #[test]
    fn find_exact_method_locates_in_named_class() {
        // utf8: [1]="C",[2]="m",[3]="()I"; classes[1] → [4]=Class{1}="C"
        let pool = mk_cp(&["C", "m", "()I"], &[1]);
        let cf = mk_cf(pool, 4, 0, vec![], vec![mk_method(2, 3, Some(default_code()))]);
        let mut reg = ClassRegistry::new();
        reg.load(cf).unwrap();
        let (lc, _m) = reg.find_exact_method("C", "m", "()I").expect("应命中");
        assert_eq!(lc.name(), "C");
        assert!(reg.find_exact_method("C", "nope", "()I").is_none());
    }

    #[test]
    fn find_default_method_finds_interface_default() {
        // 接口 I:[1]="I",[2]=Object,[3]="m",[4]="()I"; classes[1,3] → [5]=Class"I",[6]=Class Object
        let i_pool = mk_cp(&["I", "java/lang/Object", "m", "()I"], &[1, 3]);
        let i_cf = mk_cf(i_pool, 5, 6, vec![], vec![mk_method(3, 4, Some(default_code()))]);
        // 类 C 实现 I,不声明 m:[1]="C",[2]=Object,[3]="I"; classes[1,3,3] → [4]=Class"C",[5]=Class Object,[6]=Class"I"
        let c_pool = mk_cp(&["C", "java/lang/Object", "I"], &[1, 3, 3]);
        let c_cf = mk_cf(c_pool, 4, 5, vec![6], vec![]);
        let mut reg = ClassRegistry::new();
        reg.load(i_cf).unwrap();
        reg.load(c_cf).unwrap();
        let (lc, _m) = reg.find_default_method("C", "m", "()I").expect("应命中接口 default");
        assert_eq!(lc.name(), "I");
        assert!(reg.find_exact_method("C", "m", "()I").is_none()); // C 自身未声明
    }

    #[test]
    fn find_default_method_skips_abstract_finds_superinterface() {
        // I2:default m。[1]="I2",[2]=Object,[3]="m",[4]="()I"; classes[1,3] → [5]=Class"I2",[6]=Class Object
        let i2_pool = mk_cp(&["I2", "java/lang/Object", "m", "()I"], &[1, 3]);
        let i2_cf = mk_cf(i2_pool, 5, 6, vec![], vec![mk_method(3, 4, Some(default_code()))]);
        // I1:抽象 m + 超接口 I2。[1]="I1",[2]=Object,[3]="I2",[4]="m",[5]="()I";
        //    classes[1,2,3] → [6]=Class"I1",[7]=Class Object,[8]=Class"I2"
        let i1_pool = mk_cp(&["I1", "java/lang/Object", "I2", "m", "()I"], &[1, 2, 3]);
        let i1_cf = mk_cf(
            i1_pool,
            6,
            7,
            vec![8],
            vec![mk_method(4, 5, None)], // 抽象 m,无 Code
        );
        // C 实现 I1。[1]="C",[2]=Object,[3]="I1"; classes[1,2,3] → [4]=Class"C",[5]=Class Object,[6]=Class"I1"
        let c_pool = mk_cp(&["C", "java/lang/Object", "I1"], &[1, 2, 3]);
        let c_cf = mk_cf(c_pool, 4, 5, vec![6], vec![]);
        let mut reg = ClassRegistry::new();
        reg.load(i2_cf).unwrap();
        reg.load(i1_cf).unwrap();
        reg.load(c_cf).unwrap();
        let (lc, _m) = reg.find_default_method("C", "m", "()I").expect("应跳过抽象,命中 I2");
        assert_eq!(lc.name(), "I2");
    }

    #[test]
    fn find_default_method_none_when_all_abstract() {
        // I 抽象 m(无超接口)。C 实现 I。
        let i_pool = mk_cp(&["I", "java/lang/Object", "m", "()I"], &[1, 3]);
        let i_cf = mk_cf(i_pool, 5, 6, vec![], vec![mk_method(3, 4, None)]);
        let c_pool = mk_cp(&["C", "java/lang/Object", "I"], &[1, 3, 3]);
        let c_cf = mk_cf(c_pool, 4, 5, vec![6], vec![]);
        let mut reg = ClassRegistry::new();
        reg.load(i_cf).unwrap();
        reg.load(c_cf).unwrap();
        assert!(reg.find_default_method("C", "m", "()I").is_none());
    }
}
