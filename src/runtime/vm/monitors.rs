//! 对象管程(`monitorenter`/`monitorexit`/`holdsLock`;Layer 4.41 / Phase B.1,
//! Phase B.2.3b T7 从 [`super::vm`] 分解)。移植自 HotSpot `ObjectSynchronizer::enter/exit`。
//! 共享态 `VmShared.monitors`(`HashMap<Reference, MonitorState>`);owner 经 [`Vm::main_thread`]
//!(`super::threads`)解析。

use std::collections::hash_map::Entry;

use crate::runtime::{Reference, Vm, VmError};

use super::MonitorState;

impl<'a> Vm<'a> {
    /// `monitorenter`(JVMS §6.5):进入 `obj` 管程。null → NPE;owner = 当前线程(`main_thread`);
    /// 未锁 → 记 owner/count=1;已持 → count+1(重入)。单线程下 owner 恒为当前线程、无争用
    /// (Phase B.3 真并发:被他人持有时阻塞至释放;rustj 当前单线程直接重入)。
    pub(crate) fn monitor_enter(&mut self, obj: Reference) -> Result<(), VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        let owner = self.main_thread();
        // 锁 monitors 仅覆盖本 match 块:body 不调 &mut self,guard 出块即释(B.2.3b)。
        let mut monitors = self.shared.monitors.lock().unwrap();
        match monitors.entry(obj) {
            Entry::Occupied(mut e) => e.get_mut().count += 1,
            Entry::Vacant(e) => {
                e.insert(MonitorState { owner, count: 1 });
            }
        }
        Ok(())
    }

    /// `monitorexit`(JVMS §6.5):退出 `obj` 管程。null → NPE;当前线程持有(count>0)→ count-1
    /// (归零释放);未持有 / owner 不符 → `IllegalMonitorStateException`。
    pub(crate) fn monitor_exit(&mut self, obj: Reference) -> Result<(), VmError> {
        if obj.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        let owner = self.main_thread();
        // held 判定:锁 monitors 取 bool,出块释 guard 后再 throw(避免持 guard 调 &mut self)。
        let held = {
            let monitors = self.shared.monitors.lock().unwrap();
            monitors
                .get(&obj)
                .is_some_and(|m| m.owner == owner && m.count > 0)
        };
        if !held {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        }
        // 再锁执行 count-1/释放(与 held 判定不重叠,无死锁)。
        let mut monitors = self.shared.monitors.lock().unwrap();
        let count = monitors.get(&obj).unwrap().count;
        if count == 1 {
            monitors.remove(&obj);
        } else {
            monitors.get_mut(&obj).unwrap().count -= 1;
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
        let monitors = self.shared.monitors.lock().unwrap();
        Ok(monitors
            .get(&obj)
            .is_some_and(|m| m.owner == owner && m.count > 0))
    }
}
