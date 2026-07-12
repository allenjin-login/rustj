//! 类初始化(`<clinit>`):首次 **active use** 时执行超类→本类的静态初始化器。
//!
//! 对应 HotSpot `InstanceKlass::initialize_impl()`(JVMS §5.5 子集)。当前仅单线程,
//! 故无需 HotSpot 的锁 / "其它线程进行中则等待" / notify;状态机收敛为
//! `NotStarted → InProgress → Done`(重入跳过),失败 → `Failed`。
//!
//! active use 触发点(`new` / `invokestatic` / `getstatic` / `putstatic` 的目标类)
//! 在 `field` / `invoke` 子模块首步调用 [`ensure_class_initialized`];其余 invoke 形式对
//! 声明类的触发由 `new` 在实例化时先行覆盖(完整"解析类即触发"留待类链接层)。

use crate::classfile::attributes::CodeAttribute;
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};
use crate::metadata::descriptor::parse_field_descriptor;
use crate::metadata::ClassFile;
use crate::oops::{InitState, LoadedClass, Oop};
use crate::runtime::{Frame, Reference, Slot, Vm};

use super::invoke::run_with_depth;
use super::{set_throwable_field, throw_exception, Interpreter, VmError};

/// 在 `cf` 的方法表中找 `<clinit>()V`,取其 `Code`(无则 `None`)。
fn find_clinit(cf: &ClassFile) -> Option<&CodeAttribute> {
    for m in &cf.methods {
        let name_ok = matches!(
            cf.constant_pool.get(m.name_index),
            Ok(ConstantPoolEntry::Utf8(n)) if n == "<clinit>"
        );
        let desc_ok = matches!(
            cf.constant_pool.get(m.descriptor_index),
            Ok(ConstantPoolEntry::Utf8(d)) if d == "()V"
        );
        if name_ok && desc_ok {
            return m.code.as_ref();
        }
    }
    None
}

/// 运行 `lc` 的 `<clinit>`(经既有 `interpret_with` + `run_with_depth`)。无 `<clinit>`
/// 则仅默认初始化(加载期已完成)→ `Ok`。`<clinit>` 为 static void 无参,局部变量
/// 全默认,无需传参。其内 `putstatic` 经既有 `static_storage` 机制写静态字段。
fn run_clinit(lc: &LoadedClass, vm: &mut Vm) -> Result<(), VmError> {
    let Some(code) = find_clinit(&lc.cf) else {
        return Ok(());
    };
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), "<clinit>");
    run_with_depth(vm, |vm| interp.interpret_with(&mut frame, vm))?;
    Ok(())
}

/// 异常对象是否已是初始化失败类(`ExceptionInInitializerError` /
/// `NoClassDefFoundError`)——超类初始化失败已上传此类异常时,本类不再重复包装。
fn is_init_failure_class(vm: &Vm, exc: Reference) -> bool {
    let heap = vm.heap();
    let Some(Oop::Instance(i)) = heap.get(exc) else {
        return false;
    };
    matches!(
        i.class_name(),
        "java/lang/ExceptionInInitializerError" | "java/lang/NoClassDefFoundError"
    )
}

/// 应用 `ConstantValue` 属性(JVMS §4.7.2):`static` 字段若带该属性,在类准备阶段(本实现置于
/// init 前;单线程下与 prep 等价)把常量值写入 `static_storage`。primitive
/// (Integer/Long/Float/Double CP 条目)直接写槽;String 条目经 `string::intern` 成 String 引用。
///
/// 对应 HotSpot 链接阶段 `InstanceKlass::transfer_static_fields` 对 ConstantValue 的处理
/// (`javaClasses.cpp` 读 `fieldinfo` 的 initval 索引)。常量字段若另有 `<clinit>` putstatic
/// 则后续覆盖;否则此常量即终值。无 `<clinit>` 的纯常量类(如 `Integer`)依赖此步生效。
fn apply_constant_values(vm: &mut Vm, lc: &LoadedClass) {
    let cp = &lc.cf.constant_pool;
    // 先收 (ord, Slot) 对,再统一写:String 常量需 `string::intern(vm, …)`(持 `&mut vm`),
    // 须在锁 `static_storage` 之外完成,避免持 guard 时 `&mut vm`(锁序隐患)。
    let mut writes: Vec<(usize, Slot)> = Vec::new();
    for field in &lc.cf.fields {
        if !field.access_flags.is_static() {
            continue;
        }
        // 识别 ConstantValue 属性(属性名 = CP Utf8 "ConstantValue")。
        let Some(cv_attr) = field.attributes.iter().find(|a| {
            matches!(cp.get(a.name_index), Ok(ConstantPoolEntry::Utf8(n)) if n == "ConstantValue")
        }) else {
            continue;
        };
        // 属性体:u2 constantvalue_index(JVMS §4.7.2)。
        if cv_attr.info.len() != 2 {
            continue;
        }
        let cv_index = u16::from_be_bytes([cv_attr.info[0], cv_attr.info[1]]);
        let field_name = match cp.get(field.name_index) {
            Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
            _ => continue,
        };
        let field_desc = match cp.get(field.descriptor_index) {
            Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
            _ => continue,
        };
        let Ok(ft) = parse_field_descriptor(&field_desc) else {
            continue;
        };
        let Some(ord) = lc.static_field(&field_name, &ft) else {
            continue;
        };
        let Some(slot) = constant_value_slot(vm, cp, cv_index) else {
            continue;
        };
        writes.push((ord, slot));
    }
    if writes.is_empty() {
        return;
    }
    let mut storage = lc.static_storage.lock().unwrap();
    for (ord, slot) in writes {
        if ord < storage.len() {
            storage[ord] = slot;
        }
    }
}

/// ConstantValue 属性的常量池条目 → 槽。Integer/Long/Float/Double 直接转;String 经
/// `string::intern`。非 primitive/String 条目 → `None`(跳过,交 `<clinit>` 兜底)。
fn constant_value_slot(vm: &mut Vm, cp: &ConstantPool, cv_index: u16) -> Option<Slot> {
    let entry = cp.get(cv_index).ok()?;
    match entry {
        ConstantPoolEntry::Integer(v) => Some(Slot::Int(*v)),
        ConstantPoolEntry::Long(v) => Some(Slot::Long(*v)),
        ConstantPoolEntry::Float(v) => Some(Slot::Float(*v)),
        ConstantPoolEntry::Double(v) => Some(Slot::Double(*v)),
        ConstantPoolEntry::String { string_index } => {
            let text = match cp.get(*string_index).ok()? {
                ConstantPoolEntry::Utf8(s) => s.clone(),
                _ => return None,
            };
            // String 常量需 Vm intern;init 期 Vm 必在,失败则跳过(交 <clinit> 兜底)。
            super::string::intern(vm, &text).ok().map(Slot::Reference)
        }
        _ => None,
    }
}

/// 确保类已初始化(首次 active use 触发)。幂等:`Done` / `InProgress`(重入)直接返回;
/// `Failed`(曾抛异常)→ `NoClassDefFoundError`。顺序:**超类先于本类**。本类 `<clinit>`
/// 抛异常 → `ExceptionInInitializerError`(状态置 `Failed`);已是初始化失败类异常则原样上传。
///
/// 沿用 `invoke_static` / `throw_exception` 的 `'a` 借用技巧:[`Vm::registry`] 返回
/// `&'a ClassRegistry`(寿命不绑 `&self`),故取出 `&'a LoadedClass` 后仍可 `&mut vm`
/// 执行 `<clinit>`,并在重入/超类递归中反复再借。
pub(crate) fn ensure_class_initialized(
    vm: &mut Vm,
    class_name: &str,
) -> Result<(), VmError> {
    let Some(registry) = vm.registry() else {
        return Ok(()); // 无注册表(纯数值 Vm)→ 无类可初始化
    };
    let Some(lc) = registry.get(class_name) else {
        return Ok(()); // 未加载类 → 跳过(让既有"未加载"错误照常上报)
    };
    match lc.init_state() {
        InitState::Done | InitState::InProgress => return Ok(()),
        InitState::Failed => {
            return Err(throw_exception(
                vm,
                "java/lang/NoClassDefFoundError",
            ));
        }
        InitState::NotStarted => {}
    }
    // 置进行中(先于超类 / 本类),防 <clinit> 执行中重入本类造成无限递归。
    lc.set_init_state(InitState::InProgress);
    // 先初始化超类:super_class_name() 在 super_class==0(Object)时为 None → 自然终止。
    if let Some(super_name) = lc.super_class_name() {
        ensure_class_initialized(vm, super_name)?;
    }
    // 准备阶段:应用 `ConstantValue` 属性(JVMS §4.7.2 / §5.4.2)——`static` 字段的常量初值。
    // 须在 `<clinit>` 前:常量字段若有 putstatic 会再覆盖;String 常量需 `Vm` intern(仅 init 期可得)。
    apply_constant_values(vm, lc);
    match run_clinit(lc, vm) {
        Ok(()) => {
            lc.set_init_state(InitState::Done);
            Ok(())
        }
        Err(VmError::ThrownException(cause)) => {
            lc.set_init_state(InitState::Failed);
            // 超类初始化失败已上传 EIIE/NCDFO → 原样;本类 <clinit> 直接抛的业务异常 → 包 EIIE,
            // 并登记 cause(EIIE.cause = 原异常),对应 `new ExceptionInInitializerError(cause)`。
            // cause 自身轨迹含 clinit 内部位置 → format_trace 渲染 "Caused by:" 不丢根因。
            if is_init_failure_class(vm, cause) {
                Err(VmError::ThrownException(cause))
            } else {
                // throw_exception 恒返 ThrownException(reference);取引用记 cause 后再包 Err。
                let eiie = throw_exception(vm, "java/lang/ExceptionInInitializerError");
                let VmError::ThrownException(eiie) = eiie else {
                    unreachable!("throw_exception 恒返 ThrownException")
                };
                vm.record_cause(eiie, cause);
                // 同步到真 Throwable 的 cause 字段(镜像 capture_backtrace),使真 getCause()
                // 字节码(Throwable.java:448 `return (cause==this ? null : cause);`)读回根因引用。
                set_throwable_field(
                    vm,
                    eiie,
                    "cause",
                    crate::metadata::descriptor::FieldType::Class("java/lang/Throwable".into()),
                    crate::runtime::Slot::Reference(cause),
                );
                Err(VmError::ThrownException(eiie))
            }
        }
        Err(e) => {
            lc.set_init_state(InitState::Failed);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::opcode::Opcode;
    use crate::classfile::Reader;
    use crate::classfile::attributes::Attribute;
    use crate::constant_pool::ConstantPool;
    use crate::metadata::access_flags::{ACC_FINAL, ACC_STATIC};
    use crate::metadata::{AccessFlags, ClassFile, FieldInfo, MethodInfo};
    use crate::oops::ClassRegistry;
    use crate::runtime::{Slot, Vm};

    /// 常量池:[1]Utf8"Cls" [2]Utf8"java/lang/Object" [3]Utf8"v" [4]Utf8"I"
    ///        [5]Utf8"<clinit>" [6]Utf8"()V" [7]Class{1} [8]Class{2}
    ///        [9]NameAndType{3,4} [10]Fieldref{class=7, nat=9}。
    fn cp_with_static_field() -> ConstantPool {
        let mut b = vec![0x00, 0x0b]; // count=11
        b.push(0x01);
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(b"Cls");
        b.push(0x01);
        b.extend_from_slice(&16u16.to_be_bytes());
        b.extend_from_slice(b"java/lang/Object");
        b.push(0x01);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"v");
        b.push(0x01);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"I");
        b.push(0x01);
        b.extend_from_slice(&8u16.to_be_bytes());
        b.extend_from_slice(b"<clinit>");
        b.push(0x01);
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(b"()V");
        b.push(0x07);
        b.extend_from_slice(&1u16.to_be_bytes()); // [7] Class{1}
        b.push(0x07);
        b.extend_from_slice(&2u16.to_be_bytes()); // [8] Class{2}
        b.push(0x0c);
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(&4u16.to_be_bytes()); // [9] NameAndType{3,4}
        b.push(0x09);
        b.extend_from_slice(&7u16.to_be_bytes());
        b.extend_from_slice(&9u16.to_be_bytes()); // [10] Fieldref{7,9}
        ConstantPool::parse(&mut Reader::new(&b)).unwrap()
    }

    /// 合成 `Cls`:静态字段 `v:I` + `<clinit>`(`iconst_5; putstatic #10; return`)。
    fn cls_with_clinit() -> ClassFile {
        let static_v = FieldInfo {
            access_flags: AccessFlags::from_bits(ACC_STATIC),
            name_index: 3,
            descriptor_index: 4,
            attributes: Vec::new(),
        };
        let clinit = MethodInfo {
            access_flags: AccessFlags::from_bits(ACC_STATIC),
            name_index: 5, // "<clinit>"
            descriptor_index: 6, // "()V"
            attributes: Vec::new(),
            code: Some(CodeAttribute {
                max_stack: 1,
                max_locals: 0,
                code: vec![
                    Opcode::Iconst5 as u8,
                    Opcode::Putstatic as u8,
                    0x00,
                    0x0a, // Fieldref #10 = Cls.v
                    Opcode::Return as u8,
                ],
                exception_table: Vec::new(),
                attributes: Vec::new(),
                line_number_table: Vec::new(),
            }),
        };
        ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp_with_static_field(),
            access_flags: AccessFlags::from_bits(0),
            this_class: 7,
            super_class: 8,
            interfaces: Vec::new(),
            fields: vec![static_v],
            methods: vec![clinit],
            attributes: Vec::new(),
        }
    }

    #[test]
    fn clinit_runs_and_sets_static() {
        let mut reg = ClassRegistry::new();
        reg.load(cls_with_clinit()).unwrap();
        let reg = std::sync::Arc::new(reg);
        let mut vm = Vm::new(std::sync::Arc::clone(&reg));
        // <clinit> 执行前:静态字段为默认 0。
        assert_eq!(
            *reg.get("Cls").unwrap().static_storage.lock().unwrap(),
            vec![Slot::Int(0)]
        );
        ensure_class_initialized(&mut vm, "Cls").unwrap();
        // <clinit> 的 putstatic 写入 5,状态推进到 Done。
        assert_eq!(
            *reg.get("Cls").unwrap().static_storage.lock().unwrap(),
            vec![Slot::Int(5)]
        );
        assert_eq!(reg.get("Cls").unwrap().init_state(), InitState::Done);
    }

    #[test]
    fn ensure_class_initialized_is_idempotent() {
        let mut reg = ClassRegistry::new();
        reg.load(cls_with_clinit()).unwrap();
        let reg = std::sync::Arc::new(reg);
        let mut vm = Vm::new(std::sync::Arc::clone(&reg));
        ensure_class_initialized(&mut vm, "Cls").unwrap();
        // 再次触发:Done → 直接返回,不重跑 <clinit>(静态值仍为 5,非 10)。
        ensure_class_initialized(&mut vm, "Cls").unwrap();
        assert_eq!(
            *reg.get("Cls").unwrap().static_storage.lock().unwrap(),
            vec![Slot::Int(5)]
        );
    }

    /// 合成 `Cls` 的 `<clinit>` 执行 `1/0` → ArithmeticException(供 EIIE cause 链测试)。
    /// 复用 [`cp_with_static_field`](含 "<clinit>"/"()V" 于 #5/#6、this/super 于 #7/#8)。
    fn cls_with_throwing_clinit() -> ClassFile {
        let clinit = MethodInfo {
            access_flags: AccessFlags::from_bits(ACC_STATIC),
            name_index: 5, // "<clinit>"
            descriptor_index: 6, // "()V"
            attributes: Vec::new(),
            code: Some(CodeAttribute {
                max_stack: 2,
                max_locals: 0,
                code: vec![
                    Opcode::Iconst1 as u8,
                    Opcode::Iconst0 as u8,
                    Opcode::Idiv as u8, // 除零 → ArithmeticException
                    Opcode::Return as u8, // 不可达(idiv 已抛)
                ],
                exception_table: Vec::new(),
                attributes: Vec::new(),
                line_number_table: Vec::new(),
            }),
        };
        ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp_with_static_field(),
            access_flags: AccessFlags::from_bits(0),
            this_class: 7,
            super_class: 8,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: vec![clinit],
            attributes: Vec::new(),
        }
    }

    #[test]
    fn clinit_failure_eiie_carries_cause_and_clinit_frame() {
        // Cls.<clinit> 1/0 → ArithmeticException;ensure_class_initialized 包 EIIE。
        // 真实 JVM:new ExceptionInInitializerError(cause) 保留 cause;cause 自身轨迹含 clinit 帧。
        // ClassRegistry::new() 预装 ArithmeticException / ExceptionInInitializerError 引导桩。
        let mut reg = ClassRegistry::new();
        reg.load(cls_with_throwing_clinit()).unwrap();
        let mut vm = Vm::new(reg);
        let err = ensure_class_initialized(&mut vm, "Cls").unwrap_err();
        let VmError::ThrownException(eiie) = err else {
            panic!("须包为 EIIE(ThrownException),得 {err:?}");
        };
        let trace = vm.format_trace(eiie);
        assert!(
            trace.starts_with("java/lang/ExceptionInInitializerError"),
            "头部须为 EIIE,得:\n{trace}"
        );
        assert!(
            trace.contains("Caused by: java/lang/ArithmeticException"),
            "须渲染 cause 链,得:\n{trace}"
        );
        assert!(
            trace.contains("Cls.<clinit>"),
            "cause 轨迹须含 clinit 帧,得:\n{trace}"
        );
    }

    /// 常量池:[1]Utf8"Cls" [2]Utf8"java/lang/Object" [3]Utf8"iv" [4]Utf8"I"
    ///        [5]Utf8"lv" [6]Utf8"J" [7]Utf8"fv" [8]Utf8"F" [9]Utf8"dv" [10]Utf8"D"
    ///        [11]Utf8"ConstantValue" [12]Class{1} [13]Class{2}
    ///        [14]Integer(MIN) [15]Long(MIN) [16]Unusable [17]Float(1.0) [18]Double(1.0) [19]Unusable。
    fn cp_with_constant_value_field() -> ConstantPool {
        let mut b = vec![0x00, 0x14]; // count=20
        // [1] Utf8 "Cls"
        b.push(0x01);
        b.extend_from_slice(&3u16.to_be_bytes());
        b.extend_from_slice(b"Cls");
        // [2] Utf8 "java/lang/Object"
        b.push(0x01);
        b.extend_from_slice(&16u16.to_be_bytes());
        b.extend_from_slice(b"java/lang/Object");
        // [3] Utf8 "iv"
        b.push(0x01);
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"iv");
        // [4] Utf8 "I"
        b.push(0x01);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"I");
        // [5] Utf8 "lv"
        b.push(0x01);
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"lv");
        // [6] Utf8 "J"
        b.push(0x01);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"J");
        // [7] Utf8 "fv"
        b.push(0x01);
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"fv");
        // [8] Utf8 "F"
        b.push(0x01);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"F");
        // [9] Utf8 "dv"
        b.push(0x01);
        b.extend_from_slice(&2u16.to_be_bytes());
        b.extend_from_slice(b"dv");
        // [10] Utf8 "D"
        b.push(0x01);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(b"D");
        // [11] Utf8 "ConstantValue"
        b.push(0x01);
        b.extend_from_slice(&13u16.to_be_bytes());
        b.extend_from_slice(b"ConstantValue");
        // [12] Class{1}
        b.push(0x07);
        b.extend_from_slice(&1u16.to_be_bytes());
        // [13] Class{2}
        b.push(0x07);
        b.extend_from_slice(&2u16.to_be_bytes());
        // [14] Integer(-2147483648)
        b.push(0x03);
        b.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        // [15] Long(i64::MIN)  ([16] Unusable 隐式占位)
        b.push(0x05);
        b.extend_from_slice(&(i64::MIN as u64).to_be_bytes());
        // [17] Float(1.0) = 0x3f800000
        b.push(0x04);
        b.extend_from_slice(&0x3f80_0000u32.to_be_bytes());
        // [18] Double(1.0) = 0x3ff0000000000000  ([19] Unusable 隐式占位)
        b.push(0x06);
        b.extend_from_slice(&0x3ff0_0000_0000_0000u64.to_be_bytes());
        ConstantPool::parse(&mut Reader::new(&b)).unwrap()
    }

    /// 合成 `Cls`:4 个 `static final` 字段(iv:I/lv:J/fv:F/dv:D)各带 `ConstantValue` 属性,
    /// 无 `<clinit>`。验证准备阶段 ConstantValue 写入(IVMS §4.7.2)。
    fn cls_with_constant_value() -> ClassFile {
        let mk = |name_idx: u16, desc_idx: u16, cv_idx: u16| FieldInfo {
            access_flags: AccessFlags::from_bits(ACC_STATIC | ACC_FINAL),
            name_index: name_idx,
            descriptor_index: desc_idx,
            attributes: vec![Attribute {
                name_index: 11, // "ConstantValue"
                info: cv_idx.to_be_bytes().to_vec(),
            }],
        };
        ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp_with_constant_value_field(),
            access_flags: AccessFlags::from_bits(0),
            this_class: 12,
            super_class: 13,
            interfaces: Vec::new(),
            fields: vec![
                mk(3, 4, 14), // iv:I  → Integer(MIN)
                mk(5, 6, 15), // lv:J  → Long(MIN)
                mk(7, 8, 17), // fv:F  → Float(1.0)
                mk(9, 10, 18), // dv:D  → Double(1.0)
            ],
            methods: Vec::new(),
            attributes: Vec::new(),
        }
    }

    /// **RED**(ConstantValue 属性 JVMS §4.7.2):`static final` 常量字段须在类准备阶段(init 前)
    /// 由 ConstantValue 属性写入常量值,而非默认 0。Integer.MIN_VALUE / Long.MIN_VALUE 是典型用例。
    #[test]
    fn constant_value_attribute_set_before_clinit() {
        let mut reg = ClassRegistry::new();
        reg.load(cls_with_constant_value()).unwrap();
        let reg = std::sync::Arc::new(reg);
        let mut vm = Vm::new(std::sync::Arc::clone(&reg));
        let lc = reg.get("Cls").unwrap();
        // 初始化前:全默认。
        assert_eq!(
            *lc.static_storage.lock().unwrap(),
            vec![
                Slot::Int(0),
                Slot::Long(0),
                Slot::Float(0.0),
                Slot::Double(0.0),
            ]
        );
        ensure_class_initialized(&mut vm, "Cls").unwrap();
        // 初始化后:ConstantValue 属性写入常量值。
        let storage = lc.static_storage.lock().unwrap().clone();
        assert_eq!(storage[0], Slot::Int(-2147483648), "Integer.MIN_VALUE");
        assert_eq!(storage[1], Slot::Long(i64::MIN), "Long.MIN_VALUE");
        assert_eq!(storage[2], Slot::Float(1.0), "Float(1.0)");
        assert_eq!(storage[3], Slot::Double(1.0), "Double(1.0)");
    }
}
