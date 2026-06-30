//! 异常分派:`athrow` 抛出对象的异常表扫描(`find_handler`)。
//!
//! 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_athrow)` 异常表查找。
//! `[start_pc, end_pc)` 覆盖抛出点 pc 且 `catch_type`(0 = catch-all,否则
//! `is_instance`)匹配 → 返回 `handler_pc`;首条匹配胜出(JVMS 要求表内顺序即优先级)。

use super::field::resolve_class_name;
use super::{Interpreter, VmError};
use crate::classfile::attributes::ExceptionTableEntry;
use crate::runtime::{Reference, Vm};

/// 取异常对象(非 null)的运行时类名(own String 避免借用纠缠)。
/// athrow 对象必为实例(数组不能 throw);悬空 / 数组 → `BadConstant`。
fn runtime_class_name(vm: &Vm<'_>, exc: Reference) -> Result<String, VmError> {
    use crate::oops::Oop;
    let obj = vm
        .heap()
        .get(exc)
        .ok_or(VmError::BadConstant("athrow/异常分派 引用悬空"))?;
    match obj {
        Oop::Instance(i) => Ok(i.class_name().to_string()),
        Oop::Array(_) | Oop::Class(_) => {
            Err(VmError::BadConstant("athrow 对象须为异常实例(数组/Class 非法)"))
        }
    }
}

/// 在 `table` 里找覆盖 `pc` 且匹配 `exc` 运行时类的处理者;返回 `handler_pc`。
/// `catch_type == 0` → catch-all;否则解析目标类名,`is_instance(运行时类, 目标)`。
pub(super) fn find_handler(
    interp: &Interpreter<'_>,
    vm: &Vm<'_>,
    table: &[ExceptionTableEntry],
    pc: usize,
    exc: Reference,
) -> Result<Option<usize>, VmError> {
    for e in table {
        if (e.start_pc as usize) <= pc && pc < (e.end_pc as usize) {
            let hit = if e.catch_type == 0 {
                true
            } else {
                let target = resolve_class_name(interp.cp(), e.catch_type)?;
                let exc_class = runtime_class_name(vm, exc)?;
                let reg = vm
                    .registry()
                    .ok_or(VmError::BadConstant("异常分派需类注册表"))?;
                reg.is_instance(&exc_class, &target)
            };
            if hit {
                return Ok(Some(e.handler_pc as usize));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classfile::Reader;
    use crate::classfile::attributes::ExceptionTableEntry;
    use crate::constant_pool::ConstantPool;
    use crate::metadata::{AccessFlags, ClassFile};
    use crate::oops::{ClassRegistry, Oop};
    use crate::runtime::{Reference, Vm};

    /// utf8: 1=Throwable 2=BaseExc 3=SubExc 4=OtherExc ; class: 5=Throwable 6=BaseExc 7=SubExc 8=OtherExc。
    fn cp_bytes() -> Vec<u8> {
        let mut b = vec![0x00, 0x09]; // count = 8 entries + 1
        for s in ["java/lang/Throwable", "BaseExc", "SubExc", "OtherExc"] {
            b.push(0x01);
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        for idx in [1u16, 2, 3, 4] {
            b.push(0x07);
            b.extend_from_slice(&idx.to_be_bytes());
        }
        b
    }

    /// 注册表:BaseExc←SubExc,OtherExc(均 extends Throwable,Throwable 不加载)。
    fn build() -> (ClassRegistry, ConstantPool) {
        let bytes = cp_bytes();
        let mk_cp = || ConstantPool::parse(&mut Reader::new(&bytes)).unwrap();
        let mk_cf = |this: u16, super_c: u16| ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: mk_cp(),
            access_flags: AccessFlags::from_bits(0),
            this_class: this,
            super_class: super_c,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(),
        };
        let mut reg = ClassRegistry::new();
        reg.load(mk_cf(6, 5)).unwrap(); // BaseExc extends Throwable(#5)
        reg.load(mk_cf(7, 6)).unwrap(); // SubExc extends BaseExc
        reg.load(mk_cf(8, 5)).unwrap(); // OtherExc extends Throwable
        (reg, mk_cp())
    }

    fn sub_instance(reg: &ClassRegistry, vm: &mut Vm<'_>) -> Reference {
        let lc = reg.get("SubExc").unwrap();
        vm.heap_mut().alloc(Oop::Instance(reg.new_instance(lc)))
    }

    fn entry(start: u16, end: u16, handler: u16, catch_type: u16) -> ExceptionTableEntry {
        ExceptionTableEntry {
            start_pc: start,
            end_pc: end,
            handler_pc: handler,
            catch_type,
        }
    }

    fn empty_interp(cp: &ConstantPool) -> Interpreter<'_> {
        Interpreter::new(&[], cp)
    }

    #[test]
    fn find_exact_type_matches() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 7)]; // catch SubExc(#7)
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), Some(2));
    }

    #[test]
    fn find_out_of_range_no_match() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 7)];
        assert_eq!(find_handler(&interp, &vm, &table, 5, exc).unwrap(), None);
    }

    #[test]
    fn find_supertype_matches() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm); // SubExc 实例
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 6)]; // catch BaseExc(#6)
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), Some(2));
    }

    #[test]
    fn find_unrelated_no_match() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 8)]; // catch OtherExc(#8)
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), None);
    }

    #[test]
    fn find_catch_all_matches_anything() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 0)]; // catch_type 0 = catch-all
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), Some(2));
    }
}
