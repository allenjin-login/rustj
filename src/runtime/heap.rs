//! 对象堆(id-arena)。对应 HotSpot `gc/shared/collectedHeap.hpp`(仅分配,无 GC)。
//!
//! 引用 [`crate::runtime::Reference`] 为 `u32` 句柄,等价于 HotSpot 对象地址(用 id
//! 代替裸指针,安全)。分配追加 [`Oop`],返回递增 id;无回收(GC 留待后续层)。

use crate::oops::Oop;
use crate::runtime::Reference;

/// 对象堆:按 `u32` id 索引的对象数组。
pub struct Heap {
    objects: Vec<Oop>,
}

impl Heap {
    pub fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    /// 分配一个对象,返回其引用(id = 分配前长度)。
    pub fn alloc(&mut self, oop: Oop) -> Reference {
        let id = self.objects.len() as u32;
        self.objects.push(oop);
        Reference::from_id(id)
    }

    /// 按引用取对象的不可变引用;null 或越界返回 `None`。
    pub fn get(&self, r: Reference) -> Option<&Oop> {
        r.id().and_then(|id| self.objects.get(id as usize))
    }

    /// 按引用取对象的可变引用;null 或越界返回 `None`。
    pub fn get_mut(&mut self, r: Reference) -> Option<&mut Oop> {
        r.id().and_then(|id| self.objects.get_mut(id as usize))
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oops::InstanceOop;
    use crate::runtime::{Reference, Slot};

    fn inst(name: &str, slots: Vec<Slot>) -> Oop {
        Oop::Instance(InstanceOop::new(name.into(), slots))
    }

    #[test]
    fn alloc_returns_increasing_ids() {
        let mut heap = Heap::new();
        let r0 = heap.alloc(inst("A", vec![]));
        let r1 = heap.alloc(inst("B", vec![]));
        assert_eq!(r0.id(), Some(0));
        assert_eq!(r1.id(), Some(1));
    }

    #[test]
    fn get_returns_allocated_object() {
        let mut heap = Heap::new();
        let r = heap.alloc(inst("P", vec![Slot::Int(7)]));
        match heap.get(r).unwrap() {
            Oop::Instance(i) => {
                assert_eq!(i.class_name(), "P");
                assert_eq!(i.field(0), Slot::Int(7));
            }
            Oop::Array(_) | Oop::Class(_) | Oop::Lambda(_) => panic!("期望实例"),
        }
    }

    #[test]
    fn get_mut_allows_field_update() {
        let mut heap = Heap::new();
        let r = heap.alloc(inst("P", vec![Slot::Int(0)]));
        match heap.get_mut(r).unwrap() {
            Oop::Instance(i) => i.set_field(0, Slot::Int(5)),
            Oop::Array(_) | Oop::Class(_) | Oop::Lambda(_) => panic!("期望实例"),
        }
        match heap.get(r).unwrap() {
            Oop::Instance(i) => assert_eq!(i.field(0), Slot::Int(5)),
            Oop::Array(_) | Oop::Class(_) | Oop::Lambda(_) => panic!("期望实例"),
        }
    }

    #[test]
    fn get_on_null_reference_is_none() {
        let heap = Heap::new();
        assert_eq!(heap.get(Reference::null()), None);
    }
}
