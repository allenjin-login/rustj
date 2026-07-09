//! 线程管理器 + main 线程单例(Phase B.2.3b T7 从 [`super::vm`] 分解)。
//!
//! T7 把 `next_tid`(原 `VmShared` 顶层字段)收编进 [`ThreadManager`]——给 B.3 真多线程
//! 的线程表/调度预留结构归宿(对应 HotSpot `Threads` 表的 rustj 侧子集)。main 线程单例、
//! tid 分配、`Thread` 镜像字段填充归此。

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, Vm};

/// 线程管理器(B.2.3b T7)。当前仅持 tid 分配器;B.3b 增线程表(句柄→Thread 上下文)、
/// 调度顺延。对应 HotSpot `Threads` 表的 rustj 侧子集。字段 `pub(crate)`:`VmShared`
///(`super::vm`)按值持有 + `next_thread_tid` 跨模块(本模块)访问。
pub(crate) struct ThreadManager {
    /// 下一线程 tid(`Thread.tid` 递增;main 线程取首值 1,后续递增)。
    pub(crate) next_tid: std::sync::Mutex<u64>,
}

impl ThreadManager {
    /// 构造(tid 起始 1,故 main 线程 tid=1)。
    pub(crate) fn new() -> Self {
        Self {
            next_tid: std::sync::Mutex::new(1),
        }
    }
}

impl<'a> Vm<'a> {
    /// main 线程单例(惰性,4.40):`Thread.currentThread()` 返此实例。rustj 单线程 → 唯一 "main"
    /// 线程。`new_instance`(**不跑 `<init>`**)构造——默认字段(tid=0/name=null/threadLocals=null/
    /// …),`Thread.<clinit>` 仅 `registerNatives()` 空操作故无重初始化负担。无注册表/Thread 未预载
    /// → 返 null(防御,`currentThread` native 据 null 抛 InternalError)。
    pub(crate) fn main_thread(&mut self) -> Reference {
        if let Some(r) = self.thread.thread_ref {
            return r;
        }
        let r = self.alloc_main_thread();
        if !r.is_null() {
            self.thread.thread_ref = Some(r);
        }
        r
    }

    /// 分配 main 线程 Thread Instance(`new_instance`,不跑 `<init>`),并置核心字段
    /// `name="main"`、`tid`=递增首值(main=1)。对应 HotSpot `Threads::create_vm` 置 main 线程名
    /// "main"、`Thread` 构造器赋 `tid`(递增计数,首=1)。`getName()`/`threadId()` 真字节码读字段
    /// 即得。无注册表 / Thread 未预载 → 返 null。借用:先借注册表取 `&'a LoadedClass` + `new_instance`
    /// (§6 `'a` 不绑 `&self`),出块后 `heap_mut` 分配,再 `set_instance_field_by_name` 置字段
    ///(`set_instance_field_by_name` 在 [`super::mirrors`]。
    fn alloc_main_thread(&mut self) -> Reference {
        let inst = {
            let Some(reg) = self.shared.registry else {
                return Reference::null();
            };
            let Some(lc) = reg.get("java/lang/Thread") else {
                return Reference::null();
            };
            reg.new_instance(lc)
        };
        let r = self.heap_mut().alloc(Oop::Instance(inst));
        if r.is_null() {
            return r;
        }
        // main 线程 tid(递增首值=1);name="main"(真 String 实例,供 getName/equals)。
        // 对应 Thread.java:268 `private final long tid`、:271 `private volatile String name`。
        let tid = self.next_thread_tid();
        if let Ok(name_ref) = crate::runtime::interpreter::string::intern(self, "main") {
            self.set_instance_field_by_name(r, "java/lang/Thread", "name", Slot::Reference(name_ref));
        }
        self.set_instance_field_by_name(r, "java/lang/Thread", "tid", Slot::Long(tid as i64));
        r
    }

    /// 取并递增下一线程 tid(供 Thread 镜像 tid 字段;main 线程取首值 1,后续递增)。
    /// T7:`next_tid` 从 `VmShared` 顶层迁入 [`ThreadManager`](`self.shared.threads`)。
    pub(crate) fn next_thread_tid(&mut self) -> u64 {
        let mut tid = self.shared.threads.next_tid.lock().unwrap();
        let v = *tid;
        *tid += 1;
        v
    }
}
