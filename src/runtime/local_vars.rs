//! 局部变量表(JVMS §2.6.1)。long/double 占两个连续索引。

use super::frame::FrameError;
use super::slot::{Reference, Slot};

/// 定容局部变量表。索引单位为槽位(`max_locals`)。
#[derive(Debug, Clone)]
pub struct LocalVars {
    slots: Vec<Slot>,
}

impl LocalVars {
    /// 按 `max_locals`(槽位数)构造,所有槽初始化为 [`Slot::Top`]。
    pub fn new(max_locals: u16) -> Self {
        Self {
            slots: vec![Slot::Top; usize::from(max_locals)],
        }
    }

    /// 槽位数。
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    // ---- category-1 ----

    pub fn get_int(&self, index: u16) -> Result<i32, FrameError> {
        match self.slot(index)? {
            Slot::Int(v) => Ok(*v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn set_int(&mut self, index: u16, v: i32) -> Result<(), FrameError> {
        self.set1(index, Slot::Int(v))
    }
    pub fn get_float(&self, index: u16) -> Result<f32, FrameError> {
        match self.slot(index)? {
            Slot::Float(v) => Ok(*v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn set_float(&mut self, index: u16, v: f32) -> Result<(), FrameError> {
        self.set1(index, Slot::Float(v))
    }
    pub fn get_reference(&self, index: u16) -> Result<Reference, FrameError> {
        match self.slot(index)? {
            Slot::Reference(v) => Ok(*v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn set_reference(&mut self, index: u16, v: Reference) -> Result<(), FrameError> {
        self.set1(index, Slot::Reference(v))
    }

    // ---- category-2(占 index 与 index+1)----

    pub fn get_long(&self, index: u16) -> Result<i64, FrameError> {
        match self.slot(index)? {
            Slot::Long(v) => Ok(*v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn set_long(&mut self, index: u16, v: i64) -> Result<(), FrameError> {
        let next = index.checked_add(1).ok_or(FrameError::BadLocalIndex(u16::MAX))?;
        self.set1(next, Slot::Top)?;
        self.set1(index, Slot::Long(v))
    }
    pub fn get_double(&self, index: u16) -> Result<f64, FrameError> {
        match self.slot(index)? {
            Slot::Double(v) => Ok(*v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn set_double(&mut self, index: u16, v: f64) -> Result<(), FrameError> {
        let next = index.checked_add(1).ok_or(FrameError::BadLocalIndex(u16::MAX))?;
        self.set1(next, Slot::Top)?;
        self.set1(index, Slot::Double(v))
    }

    /// 取某槽的只读引用(带越界检查)。
    pub fn slot(&self, index: u16) -> Result<&Slot, FrameError> {
        self.slots
            .get(usize::from(index))
            .ok_or(FrameError::BadLocalIndex(index))
    }

    fn set1(&mut self, index: u16, slot: Slot) -> Result<(), FrameError> {
        let i = usize::from(index);
        if i >= self.slots.len() {
            return Err(FrameError::BadLocalIndex(index));
        }
        self.slots[i] = slot;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_initializes_to_top() {
        let lv = LocalVars::new(3);
        assert_eq!(lv.len(), 3);
        assert_eq!(lv.slot(0).unwrap(), &Slot::Top);
    }

    #[test]
    fn int_round_trip() {
        let mut lv = LocalVars::new(2);
        lv.set_int(0, 99).unwrap();
        assert_eq!(lv.get_int(0).unwrap(), 99);
    }

    #[test]
    fn long_spans_two_indices() {
        let mut lv = LocalVars::new(4);
        lv.set_long(1, 1_000_000_000_000).unwrap();
        assert_eq!(lv.get_long(1).unwrap(), 1_000_000_000_000);
        // 第二个槽为占位
        assert_eq!(lv.slot(2).unwrap(), &Slot::Top);
    }

    #[test]
    fn references_round_trip() {
        let mut lv = LocalVars::new(2);
        lv.set_reference(0, Reference::from_id(5)).unwrap();
        assert_eq!(lv.get_reference(0).unwrap().id(), Some(5));
        lv.set_reference(1, Reference::null()).unwrap();
        assert!(lv.get_reference(1).unwrap().is_null());
    }

    #[test]
    fn bad_index_is_rejected() {
        let mut lv = LocalVars::new(2);
        assert_eq!(lv.set_int(5, 1).unwrap_err(), FrameError::BadLocalIndex(5));
        assert_eq!(lv.get_int(5).unwrap_err(), FrameError::BadLocalIndex(5));
    }

    #[test]
    fn long_at_last_index_overflows() {
        let mut lv = LocalVars::new(2);
        // index=1 的 long 需要 index 2,越界
        assert_eq!(lv.set_long(1, 0).unwrap_err(), FrameError::BadLocalIndex(2));
    }

    #[test]
    fn type_mismatch_is_rejected() {
        let mut lv = LocalVars::new(2);
        lv.set_int(0, 1).unwrap();
        assert_eq!(lv.get_long(0).unwrap_err(), FrameError::TypeMismatch);
    }
}
