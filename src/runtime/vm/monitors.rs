//! 对象管程(`monitorenter`/`monitorexit`/`holdsLock` + `Object.wait/notify/notifyAll`)。移植自
//! HotSpot `ObjectSynchronizer::enter/exit/wait/notify/notifyall` 与 `ObjectMonitor::*`(rustj 阻塞子集)。
//!
//! Phase B.1:重入 owner/count;Phase B.3a:**真阻塞**——`monitor_enter` 被他人持有时经
//! `entry: Condvar` 阻塞至 owner 空闲再获取,`monitor_exit` 归零时 `notify_one` 唤醒等待者。
//! Phase B.3c:`object_wait` 释管程后 `wait_cvar` 阻塞,`object_notify[_all]` 推 `wake_seq` 唤醒。
//! 共享态 `VmShared.monitors`(`HashMap<Reference, Arc<JavaMonitor>>`,per-object 惰性分配);
//! owner = 当前线程 Thread 镜像句柄(`main_thread`,经 [`super::threads`])。

use std::sync::Arc;
use std::time::Duration;

use crate::runtime::{Reference, VmThread, VmError};

use super::{JavaMonitor, MonitorInner};

impl VmThread {
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
            let mut table = self.runtime.monitors.lock().unwrap();
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
        let mon = self.runtime.monitors.lock().unwrap().get(&obj).cloned();
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
            let table = self.runtime.monitors.lock().unwrap();
            table.get(&obj).cloned()
        };
        let Some(mon) = mon else { return Ok(false) };
        let guard = mon.inner.lock().unwrap();
        Ok(guard.owner == Some(owner) && guard.count > 0)
    }

    /// `Object.wait(long)`(JLS §17.2.1;移植 `ObjectSynchronizer::wait` synchronizer.cpp:514 +
    /// `ObjectMonitor::wait` objectMonitor.cpp:1732)。null → NPE;`millis<0` → IllegalArgumentException
    ///(synchronizer.cpp:516);未持有管程(无条目或 owner≠本线程)→ IMSE(`CHECK_OWNER`)。
    /// 语义:保存重入计数 → 释管程(`owner=None`/`count=0`/`waiters+1`/`entry.notify_one` 唤醒入口等待者)
    /// → 记 `wake_seq` → `wait_cvar.wait[_timeout]_while(guard, |i| i.wake_seq==my_seq)` 阻塞
    ///(`millis==0` 永久;`>0` 超时;谓词抗 spurious wakeup)→ 唤醒后 `waiters-1` → 重获管程
    ///(可能被他人抢占 → `entry.wait` 循环)→ 恢复原重入计数。释表锁后再锁 inner(drop-before-recurse)。
    pub(crate) fn object_wait(&mut self, obj: Reference, millis: i64) -> Result<(), VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        if millis < 0 {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalArgumentException",
            ));
        }
        let owner = self.main_thread();
        // B.4c:入口中断检查——已中断则清标志 + 抛 IEE(`JVM_Object_wait` 入口检 is_interrupted)。
        if self
            .interrupt_flag(owner)
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.clear_interrupt_status(owner);
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/InterruptedException",
            ));
        }
        // 锁表取 Arc clone(无该对象 → 未持有 → IMSE)。先提取 owned Option<Arc>、释表 guard,
        // 再 IMSE(throw_exception 须 &mut self,不能持表 guard)。
        let mon = self.runtime.monitors.lock().unwrap().get(&obj).cloned();
        let Some(mon) = mon else {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        };
        let mut guard = mon.inner.lock().unwrap();
        // CHECK_OWNER(objectMonitor.cpp:1741):owner 须为当前线程。
        if guard.owner != Some(owner) {
            drop(guard);
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        }
        let saved_count = guard.count;
        // 释管程:owner/count 归零、waiters+1、唤醒 entry 等待者(使其能进入管程)。
        guard.owner = None;
        guard.count = 0;
        guard.waiters += 1;
        let my_seq = guard.wake_seq;
        // 持 inner guard 调 entry.notify_one(std 允许;waiter 须重获 inner,blocked 至 wait_cvar.wait 释)。
        mon.entry.notify_one();
        // B.4c:登记 wait_targets[owner]=obj(供 interrupt0 找本线程阻塞的 monitor → wait_cvar.notify_all)。
        self.runtime
            .threads
            .wait_targets
            .lock()
            .unwrap()
            .insert(owner, obj);
        // 中断标志 Arc clone:谓词内无法读 Java 字段(须锁堆),用镜像标志廉价轮询。
        let irq = self.interrupt_flag(owner);
        // wait_cvar 阻塞:释 inner 锁、唤醒重获。谓词真(wake_seq 未变 **且** 未被中断)→ 继续等
        //(抗 spurious wakeup);notify/notifyAll 推 wake_seq 或 interrupt 置标志 → 谓词假 → 返回(或超时到)。
        let mut guard = if millis == 0 {
            mon.wait_cvar
                .wait_while(guard, |inner| {
                    inner.wake_seq == my_seq && !irq.load(std::sync::atomic::Ordering::Relaxed)
                })
                .unwrap()
        } else {
            mon.wait_cvar
                .wait_timeout_while(guard, Duration::from_millis(millis as u64), |inner| {
                    inner.wake_seq == my_seq && !irq.load(std::sync::atomic::Ordering::Relaxed)
                })
                .unwrap()
                .0
        };
        // B.4c:注销 wait_targets(已唤醒,不再阻塞于 wait)。
        self.runtime.threads.wait_targets.lock().unwrap().remove(&owner);
        // 唤醒后:waiters-1、重获管程(可能被他人抢占 → entry 等待循环)、恢复重入计数。
        guard.waiters -= 1;
        let notified = guard.wake_seq != my_seq;
        while guard.owner.is_some() && guard.owner != Some(owner) {
            guard = mon.entry.wait(guard).unwrap();
        }
        guard.owner = Some(owner);
        guard.count = saved_count;
        // B.4c:唤醒后检中断——非 notify 唤醒(超时 / 中断)且被中断 → 清标志 + 抛 IEE
        //(JLS §17.2:被 notify 唤醒则正常返回,即使随后被中断;故仅非 notify 时抛)。
        if !notified && irq.load(std::sync::atomic::Ordering::Relaxed) {
            self.clear_interrupt_status(owner);
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/InterruptedException",
            ));
        }
        Ok(())
    }

    /// `Object.notify()`(JLS §17.2.2;移植 `ObjectSynchronizer::notify` synchronizer.cpp:543 +
    /// `ObjectMonitor::notify` objectMonitor.cpp:2108)。null → NPE;未持有 → IMSE(`CHECK_OWNER`);
    /// 无等待者(`_wait_set==nullptr`)→ no-op(objectMonitor.cpp:2111);否则推 `wake_seq` +
    /// `wait_cvar.notify_one`(唤醒一个等待者)。
    pub(crate) fn object_notify(&mut self, obj: Reference) -> Result<(), VmError> {
        self.object_notify_common(obj, false)
    }

    /// `Object.notifyAll()`(JLS §17.2.3;移植 `ObjectSynchronizer::notifyall` synchronizer.cpp:556 +
    /// `ObjectMonitor::notifyAll` objectMonitor.cpp:2136)。同 notify,但 `wait_cvar.notify_all`(唤醒全部)。
    pub(crate) fn object_notify_all(&mut self, obj: Reference) -> Result<(), VmError> {
        self.object_notify_common(obj, true)
    }

    /// notify/notifyAll 共用核(null→NPE、未持有→IMSE、waiters>0 推 wake_seq 并唤醒)。`all` 控全部。
    /// 推 wake_seq 后**先释 inner guard 再 notify**(标准做法:让被唤醒 waiter 即刻重获 inner)。
    fn object_notify_common(&mut self, obj: Reference, all: bool) -> Result<(), VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        let owner = self.main_thread();
        let mon = self.runtime.monitors.lock().unwrap().get(&obj).cloned();
        let Some(mon) = mon else {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        };
        let mut guard = mon.inner.lock().unwrap();
        // CHECK_OWNER(objectMonitor.cpp:2109/2137):owner 须为当前线程。
        if guard.owner != Some(owner) {
            drop(guard);
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        }
        // wait_set 空 → no-op(objectMonitor.cpp:2111/2139);否则推 wake_seq、释 guard、唤醒。
        if guard.waiters > 0 {
            guard.wake_seq += 1;
            drop(guard);
            if all {
                mon.wait_cvar.notify_all();
            } else {
                mon.wait_cvar.notify_one();
            }
        }
        Ok(())
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
