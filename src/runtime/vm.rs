//! 执行上下文:对象堆 + 类注册表。对应 HotSpot `JavaThread` 执行所需的共享状态。
//!
//! 4.1:纯数值路径可不带注册表([`Vm::default`] 空堆 + 无注册表);对象/字段/
//! `invokestatic` 路径需注册表([`Vm::new`])。

use crate::oops::ClassRegistry;
use crate::runtime::heap::Heap;

/// 执行上下文:拥有对象堆,借用类注册表。
#[derive(Default)]
pub struct Vm<'a> {
    heap: Heap,
    registry: Option<&'a ClassRegistry>,
}

impl<'a> Vm<'a> {
    /// 构造带类注册表的 Vm(空堆)。
    pub fn new(registry: &'a ClassRegistry) -> Self {
        Self {
            heap: Heap::new(),
            registry: Some(registry),
        }
    }

    /// 对象堆。
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// 对象堆(可变)。
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// 类注册表(若启用)。
    ///
    /// 返回的引用与注册表本身同寿命(`'a`),不依赖本次对 `self` 的借用——
    /// 这样取出 `&LoadedClass` 后仍可再借 `&mut self`(如递归 `interpret_with`)。
    pub fn registry(&self) -> Option<&'a ClassRegistry> {
        self.registry
    }
}
