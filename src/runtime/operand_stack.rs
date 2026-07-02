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
        match self.pop_cat2()? {
            Slot::Long(v) => Ok(v),
            _ => Err(FrameError::TypeMismatch),
        }
    }
    pub fn pop_double(&mut self) -> Result<f64, FrameError> {
        match self.pop_cat2()? {
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

    // ---- 栈操作指令族(JVMS §6.5 dup/swap/pop2)----
    // cat-2(long/double)占两槽:**下槽持值、上槽为 `Slot::Top`**;cat-1 占单槽。
    // 故"顶值是否 cat-2" ⟺ 顶槽 == `Slot::Top`。所有形式均按值(view)操作,经
    // [`top_is_category2`] 判类别 + [`pop_n`]/[`push_n`] 槽级搬运,统一覆盖 JVMS 各形式。

    /// `dup_x1`:复制顶值(cat-1)并插到其下两个槽处。`..., v2, v1 → ..., v1, v2, v1`。
    pub fn dup_x1(&mut self) -> Result<(), FrameError> {
        let v1 = self.pop_n(1)?; // 顶值(cat-1)
        let v2 = self.pop_n(1)?; // 紧邻其下的 cat-1
        self.push_n(v1.clone())?;
        self.push_n(v2)?;
        self.push_n(v1)
    }

    /// `dup_x2`:复制顶值(cat-1)并插到其下两/三个槽处。
    /// 形式1(下为 2 cat-1):`..., v3, v2, v1 → ..., v1, v3, v2, v1`;
    /// 形式2(下为 1 cat-2):`..., v2, v1 → ..., v1, v2, v1`。
    pub fn dup_x2(&mut self) -> Result<(), FrameError> {
        let v1 = self.pop_n(1)?; // 顶值(cat-1)
        if self.top_is_category2() {
            let v2 = self.pop_n(2)?; // 紧邻其下的 cat-2
            self.push_n(v1.clone())?;
            self.push_n(v2)?;
            self.push_n(v1)
        } else {
            let v2 = self.pop_n(1)?;
            let v3 = self.pop_n(1)?;
            self.push_n(v1.clone())?;
            self.push_n(v3)?;
            self.push_n(v2)?;
            self.push_n(v1)
        }
    }

    /// `dup2`:复制顶部 2 槽(1 cat-2 或 2 cat-1)到栈顶。
    /// 形式1(2 cat-1):`..., v2, v1 → ..., v2, v1, v2, v1`;形式2(1 cat-2):`..., v1 → ..., v1, v1`。
    pub fn dup2(&mut self) -> Result<(), FrameError> {
        if self.top_is_category2() {
            let v1 = self.pop_n(2)?; // 顶值 cat-2
            self.push_n(v1.clone())?;
            self.push_n(v1)
        } else {
            let v1 = self.pop_n(1)?; // 顶 cat-1
            let v2 = self.pop_n(1)?; // 次 cat-1
            self.push_n(v2.clone())?;
            self.push_n(v1.clone())?;
            self.push_n(v2)?;
            self.push_n(v1)
        }
    }

    /// `dup2_x1`:复制顶部 2 槽(组 G)并插到其下两个槽处(其下须为 1 cat-1)。
    /// 形式1(3 cat-1):`..., v3, v2, v1 → ..., v2, v1, v3, v2, v1`;
    /// 形式2(顶 cat-2,下 cat-1):`..., v2, v1 → ..., v1, v2, v1`。
    /// 组 G 自底向顶取出,原样回插,故两形式同一代码路径。
    pub fn dup2_x1(&mut self) -> Result<(), FrameError> {
        let g = self.pop_n(2)?; // 组 G(2 槽)
        let w = self.pop_n(1)?; // 其下的 cat-1
        self.push_n(g.clone())?;
        self.push_n(w)?;
        self.push_n(g)
    }

    /// `dup2_x2`:复制顶部 2 槽(组 G)并插到其下两/三/四个槽处。
    /// 形式1(4 cat-1)/形式2(顶 cat-2,下 2 cat-1):其下为 2 cat-1;
    /// 形式3(顶 2 cat-1,下 cat-2)/形式4(顶 cat-2,下 cat-2):其下为 1 cat-2。
    pub fn dup2_x2(&mut self) -> Result<(), FrameError> {
        let g = self.pop_n(2)?; // 组 G(2 槽)
        if self.top_is_category2() {
            let w = self.pop_n(2)?; // 其下的 cat-2
            self.push_n(g.clone())?;
            self.push_n(w)?;
            self.push_n(g)
        } else {
            let w1 = self.pop_n(1)?; // 紧邻 G 的 cat-1
            let w2 = self.pop_n(1)?; // 更下的 cat-1
            self.push_n(g.clone())?;
            self.push_n(w2)?;
            self.push_n(w1)?;
            self.push_n(g)
        }
    }

    /// `pop2`:弹出顶部 2 槽(1 cat-2 或 2 cat-1)。
    pub fn pop2(&mut self) -> Result<(), FrameError> {
        self.pop_n(2)?;
        Ok(())
    }

    /// `swap`:交换顶部两个 cat-1 值。`..., v2, v1 → ..., v1, v2`。
    pub fn swap(&mut self) -> Result<(), FrameError> {
        let mut top = self.pop_n(2)?; // [v2, v1](自底向顶)
        top.reverse(); // → [v1, v2]
        self.push_n(top)
    }

    // ---- 内部 ----

    /// 顶值是否为 category-2(顶槽 == `Slot::Top`)。空栈视为非 cat-2(调用方保证深度)。
    fn top_is_category2(&self) -> bool {
        matches!(self.slots.last(), Some(Slot::Top))
    }

    /// 弹出顶部 `n` 槽,**自底向顶**顺序返回([`push_n`] 同序可还原)。下溢 → Underflow。
    fn pop_n(&mut self, n: usize) -> Result<Vec<Slot>, FrameError> {
        if n > self.slots.len() {
            return Err(FrameError::Underflow);
        }
        let split = self.slots.len() - n;
        Ok(self.slots.split_off(split))
    }

    /// 按**自底向顶**顺序压入多个槽;逐槽溢出检查。
    fn push_n(&mut self, slots: Vec<Slot>) -> Result<(), FrameError> {
        for s in slots {
            self.push1(s)?;
        }
        Ok(())
    }

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

    /// 弹出一个 category-2 值的两槽(先弹占位 `Top`,再弹值),返回值槽。
    fn pop_cat2(&mut self) -> Result<Slot, FrameError> {
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

    // ---- dup/swap 族(JVMS §6.5)----
    // 约定:cat-2(long/double)占两槽,下槽持值、上槽为 `Slot::Top`;cat-1 占单槽。
    // 以下断言均自栈顶向栈底逐弹核验 JVMS 规定的结果布局。

    #[test]
    fn dup_x1_inserts_copy_two_down() {
        // JVMS dup_x1: ..., v2, v1 → ..., v1, v2, v1(v1/v2 均 cat-1)
        let mut s = OperandStack::new(5);
        s.push_int(1).unwrap(); // v2
        s.push_int(2).unwrap(); // v1(顶)
        s.dup_x1().unwrap();
        assert_eq!(s.depth(), 3);
        assert_eq!(s.pop_int().unwrap(), 2); // v1 副本
        assert_eq!(s.pop_int().unwrap(), 1); // v2
        assert_eq!(s.pop_int().unwrap(), 2); // v1
    }

    #[test]
    fn dup_x2_below_two_cat1() {
        // 形式1:..., v3, v2, v1 → ..., v1, v3, v2, v1
        let mut s = OperandStack::new(6);
        s.push_int(3).unwrap();
        s.push_int(2).unwrap();
        s.push_int(1).unwrap();
        s.dup_x2().unwrap();
        assert_eq!(s.depth(), 4);
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_int().unwrap(), 2);
        assert_eq!(s.pop_int().unwrap(), 3);
        assert_eq!(s.pop_int().unwrap(), 1);
    }

    #[test]
    fn dup_x2_below_one_cat2() {
        // 形式2:..., v2(cat2), v1(cat1) → ..., v1, v2, v1
        let mut s = OperandStack::new(6);
        s.push_long(10).unwrap(); // v2(cat-2)
        s.push_int(1).unwrap(); // v1(cat-1)
        s.dup_x2().unwrap();
        assert_eq!(s.depth(), 4); // v1(1) + v2(2) + v1(1)
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_long().unwrap(), 10);
        assert_eq!(s.pop_int().unwrap(), 1);
    }

    #[test]
    fn dup2_two_cat1() {
        // 形式1:..., v2, v1 → ..., v2, v1, v2, v1
        let mut s = OperandStack::new(6);
        s.push_int(1).unwrap(); // v2
        s.push_int(2).unwrap(); // v1(顶)
        s.dup2().unwrap();
        assert_eq!(s.depth(), 4);
        assert_eq!(s.pop_int().unwrap(), 2);
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_int().unwrap(), 2);
        assert_eq!(s.pop_int().unwrap(), 1);
    }

    #[test]
    fn dup2_one_cat2() {
        // 形式2:..., v1(cat2) → ..., v1, v1
        let mut s = OperandStack::new(6);
        s.push_double(1.5).unwrap(); // v1(cat-2)
        s.dup2().unwrap();
        assert_eq!(s.depth(), 4);
        assert!((s.pop_double().unwrap() - 1.5).abs() < 1e-12);
        assert!((s.pop_double().unwrap() - 1.5).abs() < 1e-12);
    }

    #[test]
    fn dup2_x1_form1_three_cat1() {
        // 形式1:..., v3, v2, v1 → ..., v2, v1, v3, v2, v1
        let mut s = OperandStack::new(7);
        s.push_int(3).unwrap();
        s.push_int(2).unwrap();
        s.push_int(1).unwrap();
        s.dup2_x1().unwrap();
        assert_eq!(s.depth(), 5);
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_int().unwrap(), 2);
        assert_eq!(s.pop_int().unwrap(), 3);
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_int().unwrap(), 2);
    }

    #[test]
    fn dup2_x1_form2_cat2_on_top() {
        // 形式2:..., v2(cat1), v1(cat2) → ..., v1, v2, v1
        let mut s = OperandStack::new(6);
        s.push_int(7).unwrap(); // v2(cat-1)
        s.push_long(9).unwrap(); // v1(cat-2,顶)
        s.dup2_x1().unwrap();
        assert_eq!(s.depth(), 5); // v1(2) + v2(1) + v1(2)
        assert_eq!(s.pop_long().unwrap(), 9);
        assert_eq!(s.pop_int().unwrap(), 7);
        assert_eq!(s.pop_long().unwrap(), 9);
    }

    #[test]
    fn dup2_x2_form1_four_cat1() {
        // 形式1:..., v4, v3, v2, v1 → ..., v2, v1, v4, v3, v2, v1
        let mut s = OperandStack::new(8);
        s.push_int(4).unwrap();
        s.push_int(3).unwrap();
        s.push_int(2).unwrap();
        s.push_int(1).unwrap();
        s.dup2_x2().unwrap();
        assert_eq!(s.depth(), 6);
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_int().unwrap(), 2);
        assert_eq!(s.pop_int().unwrap(), 3);
        assert_eq!(s.pop_int().unwrap(), 4);
        assert_eq!(s.pop_int().unwrap(), 1);
        assert_eq!(s.pop_int().unwrap(), 2);
    }

    #[test]
    fn dup2_x2_form4_two_cat2() {
        // 形式4:..., v2(cat2), v1(cat2) → ..., v1, v2, v1
        let mut s = OperandStack::new(8);
        s.push_long(20).unwrap(); // v2(cat-2)
        s.push_long(10).unwrap(); // v1(cat-2,顶)
        s.dup2_x2().unwrap();
        assert_eq!(s.depth(), 6); // v1(2) + v2(2) + v1(2)
        assert_eq!(s.pop_long().unwrap(), 10);
        assert_eq!(s.pop_long().unwrap(), 20);
        assert_eq!(s.pop_long().unwrap(), 10);
    }

    #[test]
    fn pop2_removes_one_cat2_or_two_cat1() {
        // 两个 cat-1
        let mut s = OperandStack::new(4);
        s.push_int(1).unwrap();
        s.push_int(2).unwrap();
        s.pop2().unwrap();
        assert!(s.is_empty());
        // 一个 cat-2
        s.push_long(5).unwrap();
        s.pop2().unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn swap_top_two_cat1() {
        // JVMS swap:..., v2, v1 → ..., v1, v2(v1/v2 均 cat-1)
        let mut s = OperandStack::new(4);
        s.push_int(1).unwrap(); // v2
        s.push_int(2).unwrap(); // v1(顶)
        s.swap().unwrap();
        assert_eq!(s.depth(), 2);
        assert_eq!(s.pop_int().unwrap(), 1); // 原 v2 现在顶
        assert_eq!(s.pop_int().unwrap(), 2); // 原 v1 现在底
    }
}
