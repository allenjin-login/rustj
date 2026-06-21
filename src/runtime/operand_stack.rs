//! 操作数栈(JVMS §2.6.2)。

use super::frame::FrameError;
use super::slot::{Reference, Slot};

/// 定容操作数栈。容量单位为**槽位**(`max_stack`)。
#[derive(Debug, Clone)]
pub struct OperandStack {
    slots: Vec<Slot>,
    max_depth: usize,
}

impl OperandStack {
    /// 按 `max_stack`(槽位数)构造空栈。
    pub fn new(max_stack: u16) -> Self {
        Self {
            slots: Vec::with_capacity(usize::from(max_stack)),
            max_depth: usize::from(max_stack),
        }
    }

    /// 当前栈深度(槽位)。
    pub fn depth(&self) -> usize {
        self.slots.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// 清空操作数栈(异常处理者进入前调用;容量不变)。
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    // ---- category-1 ----

    pub fn push_int(&mut self, v: i32) -> Result<(), FrameError> {
        self.push1(Slot::Int(v))
    }
    pub fn push_float(&mut self, v: f32) -> Result<(), FrameError> {
        self.push1(Slot::Float(v))
    }
    pub fn push_reference(&mut self, v: Reference) -> Result<(), FrameError> {
        self.push1(Slot::Reference(v))
    }

    pub fn pop_int(&mut self) -> Result<i32, FrameError> {
        match self.pop1()? {
            Slot::Int(v) => Ok(v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn pop_float(&mut self) -> Result<f32, FrameError> {
        match self.pop1()? {
            Slot::Float(v) => Ok(v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn pop_reference(&mut self) -> Result<Reference, FrameError> {
        match self.pop1()? {
            Slot::Reference(v) => Ok(v),
            _ => Err(FrameError::TypeMismatch),
        }
    }

    // ---- category-2(long/double 占两槽)----

    pub fn push_long(&mut self, v: i64) -> Result<(), FrameError> {
        self.push2(Slot::Long(v))
    }
    pub fn push_double(&mut self, v: f64) -> Result<(), FrameError> {
        self.push2(Slot::Double(v))
    }
    pub fn pop_long(&mut self) -> Result<i64, FrameError> {
        match self.pop2()? {
            Slot::Long(v) => Ok(v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn pop_double(&mut self) -> Result<f64, FrameError> {
        match self.pop2()? {
            Slot::Double(v) => Ok(v),
            _ => Err(FrameError::TypeMismatch),
        }
    }

    // ---- 原始槽位访问(供 dup/swap 等更复杂的栈操作)----

    /// 压入单个槽位(category-1 或占位)。溢出返回错误。
    pub fn push_slot(&mut self, slot: Slot) -> Result<(), FrameError> {
        self.push1(slot)
    }

    /// 弹出单个槽位。下溢返回错误。
    pub fn pop_slot(&mut self) -> Result<Slot, FrameError> {
        self.pop1()
    }

    // ---- 内部 ----

    fn push1(&mut self, slot: Slot) -> Result<(), FrameError> {
        if self.depth() + 1 > self.max_depth {
            return Err(FrameError::Overflow);
        }
        self.slots.push(slot);
        Ok(())
    }

    fn push2(&mut self, value: Slot) -> Result<(), FrameError> {
        if self.depth() + 2 > self.max_depth {
            return Err(FrameError::Overflow);
        }
        self.slots.push(value);
        self.slots.push(Slot::Top);
        Ok(())
    }

    fn pop1(&mut self) -> Result<Slot, FrameError> {
        self.slots.pop().ok_or(FrameError::Underflow)
    }

    fn pop2(&mut self) -> Result<Slot, FrameError> {
        // 先弹出占位 Top,再弹出 category-2 值。
        let _top = self.slots.pop().ok_or(FrameError::Underflow)?;
        self.slots.pop().ok_or(FrameError::Underflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_pop_int_round_trip() {
        let mut s = OperandStack::new(4);
        s.push_int(42).unwrap();
        assert_eq!(s.depth(), 1);
        assert_eq!(s.pop_int().unwrap(), 42);
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn long_occupies_two_slots() {
        let mut s = OperandStack::new(4);
        s.push_long(123_456_789_012).unwrap();
        assert_eq!(s.depth(), 2);
        assert_eq!(s.pop_long().unwrap(), 123_456_789_012);
        assert_eq!(s.depth(), 0);
    }

    #[test]
    fn double_occupies_two_slots() {
        let mut s = OperandStack::new(4);
        s.push_double(2.5).unwrap();
        assert_eq!(s.depth(), 2);
        assert!((s.pop_double().unwrap() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn overflow_is_rejected() {
        let mut s = OperandStack::new(1);
        assert!(s.push_int(1).is_ok());
        assert_eq!(s.push_int(2).unwrap_err(), FrameError::Overflow);
        // long 需要两槽,容量 1 时必失败
        let mut s2 = OperandStack::new(1);
        assert_eq!(s2.push_long(0).unwrap_err(), FrameError::Overflow);
    }

    #[test]
    fn underflow_is_rejected() {
        let mut s = OperandStack::new(4);
        assert_eq!(s.pop_int().unwrap_err(), FrameError::Underflow);
        // 只有一个槽时弹 long 不够
        let mut s2 = OperandStack::new(4);
        s2.push_int(1).unwrap();
        assert_eq!(s2.pop_long().unwrap_err(), FrameError::Underflow);
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let mut s = OperandStack::new(4);
        // 两个 int 占两槽,但不是 long → 类型不符
        s.push_int(1).unwrap();
        s.push_int(2).unwrap();
        assert_eq!(s.pop_long().unwrap_err(), FrameError::TypeMismatch);
        s.push_long(1).unwrap();
        assert_eq!(s.pop_int().unwrap_err(), FrameError::TypeMismatch);
    }

    #[test]
    fn references_round_trip() {
        let mut s = OperandStack::new(4);
        s.push_reference(Reference::from_id(7)).unwrap();
        s.push_reference(Reference::null()).unwrap();
        assert!(s.pop_reference().unwrap().is_null());
        assert_eq!(s.pop_reference().unwrap().id(), Some(7));
    }

    #[test]
    fn interleaved_values_keep_order() {
        let mut s = OperandStack::new(8);
        s.push_int(1).unwrap();
        s.push_long(2).unwrap();
        s.push_int(3).unwrap();
        assert_eq!(s.depth(), 4); // 1 + 2 + 1
        assert_eq!(s.pop_int().unwrap(), 3);
        assert_eq!(s.pop_long().unwrap(), 2);
        assert_eq!(s.pop_int().unwrap(), 1);
    }

    #[test]
    fn clear_empties_a_populated_stack() {
        let mut s = OperandStack::new(4);
        s.push_int(1).unwrap();
        s.push_int(2).unwrap();
        assert_eq!(s.depth(), 2);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.depth(), 0);
        // clear 后仍可正常压栈(容量不变)
        s.push_int(9).unwrap();
        assert_eq!(s.depth(), 1);
    }
}
