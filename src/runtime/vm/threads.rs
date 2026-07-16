//! 线程管理器 + main 线程单例(Phase B.2.3b T7 从 [`super::vm`] 分解)。
//!
//! T7 把 `next_tid`(原 `VmShared` 顶层字段)收编进 [`ThreadManager`]——给 B.3 真多线程
//! 的线程表/调度预留结构归宿(对应 HotSpot `Threads` 表的 rustj 侧子集)。main 线程单例、
//! tid 分配、`Thread` 镜像字段填充归此。

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, VmThread};

/// 线程管理器(B.2.3b T7)。当前持 tid 分配器;B.3b 增 JoinHandle 表(已启动线程的 OS
/// 句柄),给 `join_thread` 阻塞至子完。对应 HotSpot `Threads` 表(持各 `JavaThread` 的
/// join 句柄)的 rustj 侧子集。main 线程单例、tid 分配、`Thread` 镜像字段填充归此。
pub(crate) struct ThreadManager {
    /// 下一线程 tid(`Thread.tid` 递增;main 线程取首值 1,后续递增)。
    pub(crate) next_tid: std::sync::Mutex<u64>,
    /// 已启动线程的 JoinHandle 表(B.3b;键 = Thread 实例句柄)。`start_thread` spawn 后插入;
    /// `join_thread` 取出 join(阻塞至子完)。线程结束/未在表 → 空。子线程 `run()` 完即终止,
    /// handle 经此表显式 join(防 detach;进程退出时未 join 的 Arc<VmShared> 由末存者释)。
    pub(crate) handles:
        std::sync::Mutex<std::collections::HashMap<Reference, std::thread::JoinHandle<()>>>,
    /// 系统 ThreadGroup 单例(B.4a;main 线程 holder.group,`new Thread(r)` 构造器复用之)。
    /// 惰性分配;`system_thread_group` 首调置位。对应 HotSpot `Threads::create_vm` 创建的顶层组。
    pub(crate) system_group: std::sync::Mutex<Option<Reference>>,
    /// 每线程**中断标志镜像**(B.4c):`Arc<AtomicBool>`。Java 字段 `Thread.interrupted` 由字节码
    /// 置位(`interrupt()`),但 `Object.wait` 的 Condvar 谓词无法读 Java 字段(须锁堆)→ 此 Rust 侧
    /// 镜像供谓词廉价轮询。`interrupt0` 置 true,`clearInterruptEvent` 置 false——与字段同步(每次
    /// 字段写后即调对应 native)。惰性建条目。
    pub(crate) interrupt_flags:
        std::sync::Mutex<std::collections::HashMap<Reference, std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    /// 每线程**正在 Object.wait 的锁对象**(B.4c):`interrupt0` 据此找目标线程阻塞的 monitor →
    /// `wait_cvar.notify_all` 唤醒(谓词查中断标志 → 抛 InterruptedException)。无条目 = 未在 wait 中。
    pub(crate) wait_targets: std::sync::Mutex<std::collections::HashMap<Reference, Reference>>,
}

impl ThreadManager {
    /// 构造(tid 起始 1,故 main 线程 tid=1;空 JoinHandle 表;未分配系统组)。
    pub(crate) fn new() -> Self {
        Self {
            next_tid: std::sync::Mutex::new(1),
            handles: std::sync::Mutex::new(std::collections::HashMap::new()),
            system_group: std::sync::Mutex::new(None),
            interrupt_flags: std::sync::Mutex::new(std::collections::HashMap::new()),
            wait_targets: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl VmThread {
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
    ///
    /// **B.4a**:额外置 `holder` 字段([`Self::bootstrap_main_holder`])——main 线程作 `parent` 时,
    /// `new Thread(r)` 构造器读 `parent.getThreadGroup()`/`getPriority()`/`isDaemon()`(均经 holder)。
    fn alloc_main_thread(&mut self) -> Reference {
        let inst = {
            let Some(reg) = self.vm.registry.as_ref() else {
                return Reference::null();
            };
            let Some(lc) = reg.get("java/lang/Thread") else {
                return Reference::null();
            };
            reg.new_instance(&lc)
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
        // B.4a:holder 引导(FieldHolder + 系统 ThreadGroup;真运行场景类已加载)。
        self.bootstrap_main_holder(r);
        r
    }

    /// 系统 ThreadGroup 单例(惰性;B.4a)。对应 HotSpot 顶层 "system"/"main" 组(`Threads::create_vm`)。
    /// 命中缓存即返;否则 [`Self::alloc_system_thread_group`] 分配并缓存。未加载 ThreadGroup → null。
    pub(crate) fn system_thread_group(&mut self) -> Reference {
        if let Some(r) = *self.vm.threads.system_group.lock().unwrap() {
            return r;
        }
        let r = self.alloc_system_thread_group();
        if !r.is_null() {
            *self.vm.threads.system_group.lock().unwrap() = Some(r);
        }
        r
    }

    /// 分配系统 ThreadGroup(`new_instance` 不跑 `<init>`),按名置 `name="main"`、
    /// `maxPriority=Thread.MAX_PRIORITY(10)`(ThreadGroup.java:102 私有 `<init>` 的 VM 引导等价)。
    /// `new Thread(r)` 构造器经 `parent.getThreadGroup()` 复用此组。未加载 → null。
    fn alloc_system_thread_group(&mut self) -> Reference {
        let inst = {
            let Some(reg) = self.vm.registry.as_ref() else {
                return Reference::null();
            };
            let Some(lc) = reg.get("java/lang/ThreadGroup") else {
                return Reference::null();
            };
            reg.new_instance(&lc)
        };
        let r = self.heap_mut().alloc(Oop::Instance(inst));
        if r.is_null() {
            return r;
        }
        if let Ok(name_ref) = crate::runtime::interpreter::string::intern(self, "main") {
            self.set_instance_field_by_name(r, "java/lang/ThreadGroup", "name", Slot::Reference(name_ref));
        }
        // maxPriority=10(Thread.MAX_PRIORITY;ThreadGroup.java:105)。getMaxPriority 真字节码读此字段。
        self.set_instance_field_by_name(r, "java/lang/ThreadGroup", "maxPriority", Slot::Int(10));
        r
    }

    /// 置 main 线程 `holder` 字段(VM 引导,非跑 `<init>`;B.4a 对应 D2)。分配 `Thread$FieldHolder` 实例,
    /// 按名置 `holder.{group=系统组, priority=NORM_PRIORITY(5), daemon=false, threadStatus=0(NEW),
    /// stackSize=0}`(task 默认 null)。使 `new Thread(r)` 构造器的 `parent.getThreadGroup()`/
    /// `getPriority()`/`isDaemon()`/`isTerminated()`(均经 holder)可用。FieldHolder/ThreadGroup 未加载
    ///(单测最小设置)→ 静默跳过(保既有 main_thread 无 holder 行为,不破坏既有测试)。
    fn bootstrap_main_holder(&mut self, main: Reference) {
        let group = self.system_thread_group();
        if group.is_null() {
            return;
        }
        let holder = {
            let Some(reg) = self.vm.registry.as_ref() else {
                return;
            };
            let Some(lc) = reg.get("java/lang/Thread$FieldHolder") else {
                return;
            };
            let inst = reg.new_instance(&lc);
            self.heap_mut().alloc(Oop::Instance(inst))
        };
        if holder.is_null() {
            return;
        }
        self.set_instance_field_by_name(
            holder,
            "java/lang/Thread$FieldHolder",
            "group",
            Slot::Reference(group),
        );
        self.set_instance_field_by_name(
            holder,
            "java/lang/Thread$FieldHolder",
            "priority",
            Slot::Int(5),
        );
        self.set_instance_field_by_name(
            holder,
            "java/lang/Thread$FieldHolder",
            "daemon",
            Slot::Int(0),
        );
        self.set_instance_field_by_name(
            holder,
            "java/lang/Thread$FieldHolder",
            "threadStatus",
            Slot::Int(0),
        );
        self.set_instance_field_by_name(
            holder,
            "java/lang/Thread$FieldHolder",
            "stackSize",
            Slot::Long(0),
        );
        // task 默认 null(无需显式置)。holder 入 main 线程。
        self.set_instance_field_by_name(main, "java/lang/Thread", "holder", Slot::Reference(holder));
    }

    /// 取并递增下一线程 tid(供 Thread 镜像 tid 字段;main 线程取首值 1,后续递增)。
    /// T7:`next_tid` 从 `VmShared` 顶层迁入 [`ThreadManager`](`self.shared.threads`)。
    pub(crate) fn next_thread_tid(&mut self) -> u64 {
        let mut tid = self.vm.threads.next_tid.lock().unwrap();
        let v = *tid;
        *tid += 1;
        v
    }

    /// 读堆外「下一线程 tid」计数器(`Unsafe.getLongVolatile(null, NEXT_THREAD_ID_OFFSET)`,
    /// 经 `ThreadIdentifiers.next()→getAndAddLong`)。返回当前值(自增前)。见 [`Vm::next_thread_tid`]。
    pub(crate) fn read_next_thread_id(&self) -> i64 {
        *self.vm.threads.next_tid.lock().unwrap() as i64
    }

    /// CAS 堆外「下一线程 tid」计数器(`Unsafe.compareAndSetLong(null, NEXT_THREAD_ID_OFFSET, e, x)`)。
    /// 当前 == expected → 写 new 返 true,否则 false。单线程构造期首 CAS 必中。供 `getAndAddLong` 循环。
    pub(crate) fn cas_next_thread_id(&self, expected: i64, new: i64) -> bool {
        let mut g = self.vm.threads.next_tid.lock().unwrap();
        if *g as i64 == expected {
            *g = new as u64;
            true
        } else {
            false
        }
    }

    /// `Thread.start0` 真起线程(Phase B.3b;移植 `JVM_StartThread`,jvm.cpp)。`this` = Thread
    /// 实例。null → NPE。取 `Arc::clone(&self.shared)` → `std::thread::spawn` 子 OS 线程:子
    /// `Vm::from_shared` 派生(共享堆/注册表/管程;独立调用栈)→ 置 `thread_ref = this`(子线程
    /// 身份)→ 跑虚分派 `run()V` → 终止序列。
    /// `JoinHandle` 存 `ThreadManager.handles`(键=this);[`Vm::join_thread`] 阻塞 join。
    ///
    /// **B.4b 关键:eetop + threadStatus 须在父线程(start_thread)同步置位,spawn 之前**——
    /// 对应 HotSpot `java_lang_Thread::set_thread`(JVM_StartThread 内,子线程 run 前)。否则
    /// `start0` 返后 `isAlive()`(`eetop!=0`)可能读到旧值 0 → `join()` 不 wait 即返 → 子线程副作用
    /// 丢失的竞态。父线程置位后,无论子线程何时调度,start0 返时 isAlive() 恒 true。
    ///
    /// spawn 闭包 move 捕获 `shared`(`Arc<VmShared>: 'static`,B.3.0)+ `this`(`Reference: Copy`)。
    /// 子线程 `run_thread_body` 的异常顺延(B.4:子线程未捕获异常处理 / `Thread.dispatchUncaughtException`)。
    pub(crate) fn start_thread(&mut self, this: Reference) -> Result<(), crate::runtime::VmError> {
        if this.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        // 父线程同步置 alive 标志:eetop=1(`isAlive()`=eetop!=0)+ threadStatus=RUNNABLE。
        // 必在 spawn 前——start0 返后 join() 的 isAlive() 须恒 true(无子线程调度竞态)。
        self.set_eetop(this, 1);
        self.set_thread_status(this, crate::runtime::vm::THREAD_STATUS_RUNNABLE);
        let shared = self.vm_arc();
        let handle = std::thread::spawn(move || {
            let mut child = Self::from_vm(shared);
            // 子线程身份 = 此 Thread 实例(`currentThread()` 返它;管程 owner 据此区分线程)。
            child.thread.thread_ref = Some(this);
            // eetop 已由父线程置位。跑 Thread.run()V(虚分派;override 优先)。异常丢弃(B.4 顺延)。
            let res = child.run_thread_body(this);
            // B.4 收尾:未捕获异常分派(Java 语义)。`run()` 抛出 → VM 调
            // `Thread.dispatchUncaughtException(e)`(Thread.java:2561 包私有字节码)→
            // `getUncaughtExceptionHandler().uncaughtException(this, e)`。须在 terminate 前
            //(getUncaughtExceptionHandler 检 isTerminated → 已终止返 null)。
            if let Err(crate::runtime::VmError::ThrownException(throwable)) = res {
                child.dispatch_uncaught_exception(this, throwable);
            }
            // B.4b 终止:ensure_join——set TERMINATED + eetop=0 + notifyAll 唤醒 join() 等待者。
            child.terminate_thread(this);
        });
        self.vm
            .threads
            .handles
            .lock()
            .unwrap()
            .insert(this, handle);
        Ok(())
    }

    /// 阻塞至 `this` 线程结束(Phase B.3b;取 `ThreadManager.handles` 的 JoinHandle → `join`)。
    /// 供 Rust 侧测试 / 未来 native 桥确认子线程完成。未在表(未 start / 已 join)→ 空操作。
    /// **注意**:真 Java `Thread.join()` 走 `synchronized + wait(0)` 循环(B.3c Object.wait/notify),
    /// 非本 Rust 直 join;本方法为 VM 内部确定性等待(测试用)。
    #[allow(dead_code)] // 仅 #[cfg(test)] 闸门引用 → 非 test lib 构建视为 dead。
    pub(crate) fn join_thread(&self, this: Reference) {
        if let Some(h) = self.vm.threads.handles.lock().unwrap().remove(&this) {
            let _ = h.join();
        }
    }

    /// 子线程体(B.3b):虚分派解析 `run()V`(按 `this` 运行时类——子类 override 优先),建帧
    ///(`locals[0] = this`),经解释器跑真字节码。移植 HotSpot `JavaThread::run` → `thread_entry`
    /// → `call_virtual(run)`。不经 `run_with_depth`(其在私有 `interpreter::invoke` 模块,vm 不可
    /// 达);顶层入口帧 frame_depth 不 +1(无害 off-by-one;嵌套 invoke 自带 depth)。
    fn run_thread_body(&mut self, this: Reference) -> Result<(), crate::runtime::VmError> {
        use crate::runtime::{Frame, Interpreter, Value, VmError};
        // this 运行时类(owned;后续 &mut self 须先释 heap guard)。
        let runtime_class = {
            let heap = self.heap();
            match heap.get(this) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                _ => return Err(VmError::BadConstant("start0:this 须为 Instance")),
            }
        };
        let reg = self
            .registry()
            .ok_or(VmError::BadConstant("start0 须类注册表"))?;
        let (target_lc, target_method_idx) = match reg.resolve_dispatch(&runtime_class, "run", "()V") {
            Some(x) => x,
            None => {
                return Err(crate::runtime::interpreter::throw_exception(
                    self,
                    "java/lang/AbstractMethodError",
                ))
            }
        };
        let target_method = &target_lc.cf.methods[target_method_idx];
        let Some(code) = target_method.code.as_ref() else {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/AbstractMethodError",
            ));
        };
        let interp = Interpreter::new(&code.code, &target_lc.cf.constant_pool)
            .with_exception_table(&code.exception_table);
        let mut frame = Frame::new(code.max_locals, code.max_stack);
        frame.locals.set_reference(0, this)?;
        match interp.interpret_with(&mut frame, self)? {
            Value::Void => Ok(()),
            _ => Err(VmError::BadConstant("Thread.run 须 void")),
        }
    }

    /// 置 `Thread.eetop` 字段(VM 管理字段;Thread.java:265)。`set_instance_field_by_name` 在
    /// 真实 Thread(已加载)上写;桩(bootstrap)无此字段则静默跳过(同 [`Vm::alloc_main_thread`]。
    pub(crate) fn set_eetop(&mut self, this: Reference, val: i64) {
        self.set_instance_field_by_name(this, "java/lang/Thread", "eetop", Slot::Long(val));
    }

    /// 置 `Thread$FieldHolder.threadStatus` 字段(B.4b;JVMTI 状态位掩码 javaThreadStatus.hpp:33-60)。
    /// `Thread.start()`/`Thread.join()` 经 `holder.threadStatus` 判活/判状态——此字段为它们的关键状态源。
    /// 真实 Thread 无 holder(bootstrap 桩 / 未加载 FieldHolder)→ 静默跳过(保既有行为)。
    fn set_thread_status(&mut self, this: Reference, status: i32) {
        let Some(holder) = self.instance_reference_field(this, "java/lang/Thread", "holder") else {
            return;
        };
        if holder.is_null() {
            return;
        }
        self.set_instance_field_by_name(
            holder,
            "java/lang/Thread$FieldHolder",
            "threadStatus",
            Slot::Int(status),
        );
    }

    /// 子线程终止序列(B.4b;移植 HotSpot `JavaThread::ensure_join`,javaThread.cpp:668-683):
    /// `synchronized(threadObj)` [ObjectLocker] → `set_thread_status(TERMINATED)` →
    /// `release_set_thread(nullptr)` [eetop=0] → `notify_all`。
    ///
    /// 顺序关键:status=TERMINATED + eetop=0 **在** notifyAll **之前**,使 joiner 的 `isAlive()`
    ///(`eetop!=0`)重检返 false(否则 joiner 被唤醒后 isAlive() 仍 true → 死循环 wait)。
    /// `synchronized` 块持管程,notifyAll 唤醒的 joiner 在本块退出后(释管程)才获锁继续——
    /// 故 joiner 醒后重检 isAlive() 时,eetop 与 status 均已就位。
    fn terminate_thread(&mut self, this: Reference) {
        // 持 threadObj 管程(对应 ObjectLocker;确保 set_status/eetop 与 notifyAll 原子可见)。
        let _ = self.monitor_enter(this);
        self.set_thread_status(this, crate::runtime::vm::THREAD_STATUS_TERMINATED);
        self.set_eetop(this, 0);
        let _ = self.object_notify_all(this);
        let _ = self.monitor_exit(this);
    }

    /// 取/建线程的中断标志镜像(B.4c)。惰性建条目(首次访问,默认 false)。返 `Arc` clone
    /// 供 `Object.wait` 的 Condvar 谓词廉价轮询(无法在谓词内读 Java 字段——须锁堆)。
    pub(crate) fn interrupt_flag(
        &self,
        thread: Reference,
    ) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        let mut flags = self.vm.threads.interrupt_flags.lock().unwrap();
        flags
            .entry(thread)
            .or_insert_with(|| std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)))
            .clone()
    }

    /// `Thread.interrupt0()` 实例 native(B.4c;移植 `JVM_Interrupt` jvm.cpp → `JavaThread::interrupt`)。
    /// null → NPE。置目标线程中断标志镜像 true + 若目标正阻塞于 `Object.wait` 则唤醒其 monitor 的
    /// `wait_cvar`(谓词查中断标志 → 抛 InterruptedException)。Java 字段 `interrupted` 已由 `interrupt()`
    /// 字节码在调本 native 前置 true;本 native 仅负责唤醒。sleep 中断由 `sleepNanos0` 轮询标志捕获。
    pub(crate) fn interrupt_thread(&mut self, target: Reference) -> Result<(), crate::runtime::VmError> {
        if target.is_null() {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/NullPointerException",
            ));
        }
        self.interrupt_flag(target)
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // 若目标在 Object.wait 中,取其锁对象 → monitor → wait_cvar.notify_all 唤醒(谓词查中断)。
        let lock_obj = self
            .vm
            .threads
            .wait_targets
            .lock()
            .unwrap()
            .get(&target)
            .copied();
        if let Some(lock) = lock_obj
            && let Some(mon) = self.vm.monitors.lock().unwrap().get(&lock).cloned()
        {
            mon.wait_cvar.notify_all();
        }
        Ok(())
    }

    /// `Thread.clearInterruptEvent()` 静态 native(B.4c):清**当前线程**中断标志镜像。Java 字段
    /// `interrupted` 已由 `clearInterrupt()`/`getAndClearInterrupt()` 字节码在调本 native 前置 false。
    /// 对应 HotSpot `JVM_ClearInterruptEvent`(VM 记账清除)。与 [`Self::interrupt_thread`] 配对,
    /// 保持 Java 字段 ↔ 镜像标志同步(每次字段写后即调对应 native)。
    pub(crate) fn clear_interrupt_event(&mut self) {
        let cur = self.main_thread();
        if cur.is_null() {
            return;
        }
        if let Some(flag) = self.vm.threads.interrupt_flags.lock().unwrap().get(&cur) {
            flag.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 清线程中断状态(Java 字段 `interrupted` + 镜像标志;B.4c)。供 `Object.wait`/`Thread.sleep`
    /// 抛 `InterruptedException` 前调用(JLS §17.2.3:抛 IEE 前先清中断状态)。对应 HotSpot
    /// `ObjectMonitor::wait` / sleep 检测中断后清标志再抛。
    pub(crate) fn clear_interrupt_status(&mut self, thread: Reference) {
        // Java 字段 interrupted = false(真 Thread 写;桩无此字段静默跳过)。
        self.set_instance_field_by_name(thread, "java/lang/Thread", "interrupted", Slot::Int(0));
        if let Some(flag) = self.vm.threads.interrupt_flags.lock().unwrap().get(&thread) {
            flag.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// 子线程未捕获异常分派(B.4 收尾;移植 HotSpot `JavaThread::invoke_uncaught_exception_handler`
    /// → `Thread.dispatchUncaughtException(Throwable)`,Thread.java:2561)。`run()` 抛出后,VM 在
    /// [`Self::terminate_thread`] 前调此。
    ///
    /// **自定义 handler**(`Thread.uncaughtExceptionHandler` 字段非 null)→ 走 Java 字节码
    /// `dispatchUncaughtException`(`getUncaughtExceptionHandler().uncaughtException(this, e)`):
    /// 虚分派 + `interpret_with`(帧 `locals[0]=this`/`locals[1]=throwable`)。
    /// **默认路径**(字段 null;JVM `ThreadGroup.uncaughtException` 顶层分支)→ stderr 打印
    /// `Exception in thread "<name>" <轨迹>`——**附线程名**(用户要求,方便 debug;JVM 本也如此)。
    /// 自定义 handler 字节码分派失败 → 回退默认打印路径(防异常失控;JVM 亦不二次传播)。
    fn dispatch_uncaught_exception(&mut self, this: Reference, throwable: Reference) {
        let has_custom = self
            .instance_reference_field(this, "java/lang/Thread", "uncaughtExceptionHandler")
            .is_some_and(|h| !h.is_null());
        if has_custom && self.invoke_dispatch_uncaught_bytecode(this, throwable).is_ok() {
            return;
        }
        // 默认路径(无自定义 handler,或字节码分派失败回退):stderr + 线程名前缀。
        eprintln!("{}", self.format_uncaught_default(this, throwable));
    }

    /// 默认未捕获异常的格式化文本(B.4 收尾):`Exception in thread "<name>" <轨迹>`——**附线程名**
    ///(用户要求,方便 debug;JVM `ThreadGroup.uncaughtException` 顶层分支等价)。无自定义 handler 时
    /// [`Self::dispatch_uncaught_exception`] 用此打印 stderr。抽离为独立法便于单测钉死格式。
    /// 无 name 字段 → 回退 `"Thread-N"`;无轨迹元数据 → 退化为仅运行时类名(再回退 `java/lang/Throwable`)。
    pub(crate) fn format_uncaught_default(&self, this: Reference, throwable: Reference) -> String {
        let name_ref = self
            .instance_reference_field(this, "java/lang/Thread", "name")
            .filter(|r| !r.is_null());
        let name = match name_ref {
            Some(nr) => crate::runtime::interpreter::string::read_text(self, nr)
                .ok()
                .flatten()
                .unwrap_or_else(|| "Thread-N".to_string()),
            None => "Thread-N".to_string(),
        };
        let trace = self.format_trace(throwable);
        let body = if trace.is_empty() {
            // 无轨迹元数据:退化为仅运行时类名(防御;正常 throw_exception 路径必有元数据)。
            match self.vm.heap.lock().unwrap().get(throwable) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                _ => "java/lang/Throwable".to_string(),
            }
        } else {
            trace
        };
        format!("Exception in thread \"{name}\" {body}")
    }

    /// 走 Java 字节码 `Thread.dispatchUncaughtException(Throwable)`(B.4 收尾)。虚分派解析
    /// `dispatchUncaughtException(Ljava/lang/Throwable;)V`(按 `this` 运行时类),建帧
    /// `locals[0]=this`/`locals[1]=throwable`,经解释器跑真字节码。Err(类未加载 / 方法缺失 /
    /// 运行时抛出)由 [`Self::dispatch_uncaught_exception`] 据以回退默认打印路径。
    fn invoke_dispatch_uncaught_bytecode(
        &mut self,
        this: Reference,
        throwable: Reference,
    ) -> Result<(), crate::runtime::VmError> {
        use crate::runtime::{Frame, Interpreter, Value, VmError};
        let runtime_class = {
            let heap = self.heap();
            match heap.get(this) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                _ => return Err(VmError::BadConstant("dispatchUncaught:this 须为 Instance")),
            }
        };
        let reg = self
            .registry()
            .ok_or(VmError::BadConstant("dispatchUncaught 须类注册表"))?;
        let Some((target_lc, target_method_idx)) =
            reg.resolve_dispatch(&runtime_class, "dispatchUncaughtException", "(Ljava/lang/Throwable;)V")
        else {
            return Err(VmError::BadConstant("未解析 dispatchUncaughtException"));
        };
        let target_method = &target_lc.cf.methods[target_method_idx];
        let Some(code) = target_method.code.as_ref() else {
            return Err(VmError::BadConstant("dispatchUncaughtException 无 Code"));
        };
        let interp = Interpreter::new(&code.code, &target_lc.cf.constant_pool)
            .with_exception_table(&code.exception_table);
        let mut frame = Frame::new(code.max_locals, code.max_stack);
        frame.locals.set_reference(0, this)?;
        frame.locals.set_reference(1, throwable)?;
        match interp.interpret_with(&mut frame, self)? {
            Value::Void => Ok(()),
            _ => Err(VmError::BadConstant("dispatchUncaughtException 须 void")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锁定堆外「下一线程 tid」计数器的 read/CAS 往返语义(B.4a)。
    /// `ThreadIdentifiers.next()` = 字节码 `getAndAddLong(null, NEXT_TID_OFFSET, 1)` 循环 =
    /// `read_next_thread_id` + `cas_next_thread_id` —— 构造器闸门只经构造器间接走一次 CAS,
    /// 本测直接钉住 round-trip(成功 CAS 写新值;失配 CAS 不改值返 false),防回归。
    #[test]
    fn next_thread_id_cas_round_trip() {
        let mut vm = VmThread::default();
        let initial = vm.read_next_thread_id();
        // 当前 == initial → 写 initial+1,返 true。
        assert!(vm.cas_next_thread_id(initial, initial + 1));
        assert_eq!(vm.read_next_thread_id(), initial + 1);
        // 当前(initial+1)!= initial → 失配,不改值,返 false。
        assert!(!vm.cas_next_thread_id(initial, initial + 5));
        assert_eq!(vm.read_next_thread_id(), initial + 1);
        // next_thread_tid 走同一计数器(递增取值):此后取值 == initial+1,计数器 → initial+2。
        assert_eq!(vm.next_thread_tid(), (initial + 1) as u64);
        assert_eq!(vm.read_next_thread_id(), initial + 2);
    }

    /// 默认未捕获异常文本**附线程名前缀**(B.4 收尾;用户要求"栈轨迹要附带线程信息")。
    /// 空 Vm(无注册表)→ name 字段读返 None → 回退 `"Thread-N"`;throwable 无元数据 → 回退
    /// `java/lang/Throwable`。钉死 `Exception in thread "<name>" <body>` 格式,防回归。
    #[test]
    fn uncaught_default_format_has_thread_prefix() {
        let vm = VmThread::default();
        let s = vm.format_uncaught_default(Reference::null(), Reference::null());
        assert!(
            s.starts_with("Exception in thread \""),
            "须附线程名前缀: {s}"
        );
        assert!(s.contains("Thread-N"), "无名线程回退 Thread-N: {s}");
    }
}
