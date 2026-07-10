//! 对象管程(`monitorenter`/`monitorexit`/`holdsLock`)。移植自 HotSpot
//! `ObjectSynchronizer::enter/exit`(`ObjectMonitor` 的 rustj 阻塞子集)。
//!
//! Phase B.1:重入 owner/count;Phase B.3a:**真阻塞**——`monitor_enter` 被他人持有时经
//! `entry: Condvar` 阻塞至 owner 空闲再获取,`monitor_exit` 归零时 `notify_one` 唤醒等待者。
//! 共享态 `VmShared.monitors`(`HashMap<Reference, Arc<JavaMonitor>>`,per-object 惰性分配);
//! owner = 当前线程 Thread 镜像句柄(`main_thread`,经 [`super::threads`])。

use std::sync::Arc;

use crate::runtime::{Reference, Vm, VmError};

use super::{JavaMonitor, MonitorInner};

impl Vm {
    /// `monitorenter`(JVMS §6.5):进入 `obj` 管程。null → NPE;owner = 当前线程(`main_thread`)。
    /// 取/建该对象的 [`JavaMonitor`](锁表→取 `Arc` clone→**释表**),再锁 `inner`:owner==本线程→重入
    /// `count+1`;owner==None→占位 `owner+count=1`;owner==他人→`entry.wait` 循环至 owner 空闲/本线程。
    /// 释表锁后再锁 inner:持 inner 等待时不持表锁 → 不同对象不同 JavaMonitor → 无锁序死锁。
    pub(crate) fn monitor_enter(&mut self, obj: Reference) -> Result<(), VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        let owner = self.main_thread();
        // 锁表取/建 JavaMonitor,克隆 Arc 后即释表 guard(drop-before-recurse;B.2.3b)。
        let mon = {
            let mut table = self.shared.monitors.lock().unwrap();
            Arc::clone(
                table
                    .entry(obj)
                    .or_insert_with(|| Arc::new(JavaMonitor::new())),
            )
        };
        // 锁 inner:被他人持有时阻塞等待至空闲或本线程持有(Condvar 标准用法:wait 释锁、唤醒重获)。
        let mut guard = mon.inner.lock().unwrap();
        while guard.owner.is_some() && guard.owner != Some(owner) {
            guard = mon.entry.wait(guard).unwrap();
        }
        acquire_or_reenter(&mut guard, owner);
        Ok(())
    }

    /// `monitorexit`(JVMS §6.5):退出 `obj` 管程。null → NPE;当前线程持有(count>0)→ count-1
    ///(归零 owner=None + `notify_one` 唤醒等待者);未持有 / owner 不符 / 表中无该对象 → IMSE。
    pub(crate) fn monitor_exit(&mut self, obj: Reference) -> Result<(), VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        let owner = self.main_thread();
        // 锁表取 Arc clone(无该对象 → 未持有 → IMSE)。先提取 owned Option<Arc>、释表 guard,
        // 再 IMSE(throw_exception 须 &mut self,不能持表 guard)。
        let mon = self.shared.monitors.lock().unwrap().get(&obj).cloned();
        let Some(mon) = mon else {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        };
        let mut guard = mon.inner.lock().unwrap();
        if guard.owner != Some(owner) || guard.count == 0 {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        }
        guard.count -= 1;
        if guard.count == 0 {
            guard.owner = None;
            // 释 inner guard 后再 notify(标准做法:持锁 notify 非错误但释后更简,wait 方唤醒即重获)。
            drop(guard);
            mon.entry.notify_one();
        }
        Ok(())
    }

    /// `Thread.holdsLock(Object)`(Thread.java:2178):当前线程是否持有 `obj` 管程。
    /// null → NPE(JDK:`holdsLock(null)` 抛 NPE)。
    pub(crate) fn holds_lock(&mut self, obj: Reference) -> Result<bool, VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        let owner = self.main_thread();
        // 锁表取 Arc(无 → false),释表后锁 inner 读 owner==本线程 && count>0。
        let mon = {
            let table = self.shared.monitors.lock().unwrap();
            table.get(&obj).cloned()
        };
        let Some(mon) = mon else { return Ok(false) };
        let guard = mon.inner.lock().unwrap();
        Ok(guard.owner == Some(owner) && guard.count > 0)
    }
}

/// 占位空闲管程(owner==None)或重入本线程(owner==self);调用前须持 `inner` 锁且已过等待循环。
fn acquire_or_reenter(inner: &mut std::sync::MutexGuard<'_, MonitorInner>, owner: Reference) {
    if inner.owner == Some(owner) {
        inner.count += 1;
    } else {
        // 等待循环保证到达此处时 owner==None。
        inner.owner = Some(owner);
        inner.count = 1;
    }
}
