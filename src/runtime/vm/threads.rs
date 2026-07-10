//! 线程管理器 + main 线程单例(Phase B.2.3b T7 从 [`super::vm`] 分解)。
//!
//! T7 把 `next_tid`(原 `VmShared` 顶层字段)收编进 [`ThreadManager`]——给 B.3 真多线程
//! 的线程表/调度预留结构归宿(对应 HotSpot `Threads` 表的 rustj 侧子集)。main 线程单例、
//! tid 分配、`Thread` 镜像字段填充归此。

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, Vm};

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
}

impl ThreadManager {
    /// 构造(tid 起始 1,故 main 线程 tid=1;空 JoinHandle 表)。
    pub(crate) fn new() -> Self {
        Self {
            next_tid: std::sync::Mutex::new(1),
            handles: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl Vm {
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
            let Some(reg) = self.shared.registry.as_ref() else {
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

    /// `Thread.start0` 真起线程(Phase B.3b;移植 `JVM_StartThread`,jvm.cpp)。`this` = Thread
    /// 实例。null → NPE。取 `Arc::clone(&self.shared)` → `std::thread::spawn` 子 OS 线程:子
    /// `Vm::from_shared` 派生(共享堆/注册表/管程;独立调用栈)→ 置 `thread_ref = this`(子线程
    /// 身份)→ 置 `eetop` 非 0(标记 alive)→ 跑虚分派 `run()V` → 终止置 `eetop=0`。
    /// `JoinHandle` 存 `ThreadManager.handles`(键=this);[`Vm::join_thread`] 阻塞 join。
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
        let shared = self.shared_arc();
        let handle = std::thread::spawn(move || {
            let mut child = Self::from_shared(shared);
            // 子线程身份 = 此 Thread 实例(`currentThread()` 返它;管程 owner 据此区分线程)。
            child.thread.thread_ref = Some(this);
            // 标记 alive:eetop 非 0(VM 管理字段;Thread.java:265,`alive()` = eetop!=0)。
            child.set_eetop(this, 1);
            // 跑 Thread.run()V(虚分派;override 优先)。异常丢弃(B.4 顺延)。
            let res = child.run_thread_body(this);
            // 终止:eetop=0(`isAlive()`=false)。B.3c 增 `notifyAll`(Thread.exit → join 唤醒)。
            child.set_eetop(this, 0);
            let _ = res;
        });
        self.shared
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
        if let Some(h) = self.shared.threads.handles.lock().unwrap().remove(&this) {
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
        let (target_lc, target_method) = match reg.resolve_dispatch(&runtime_class, "run", "()V") {
            Some(x) => x,
            None => {
                return Err(crate::runtime::interpreter::throw_exception(
                    self,
                    "java/lang/AbstractMethodError",
                ))
            }
        };
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
}
