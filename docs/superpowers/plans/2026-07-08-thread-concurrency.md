# Phase B 实施计划:Thread 与并发(目标导向 4-commit)

**日期**:2026-07-08
**前提**:用户"并发刻不容缓,之后代价更大"——放弃过渡形态,直奔目标架构,每 commit 闸门绿。

## 目标架构

- `ThreadContext`(每线程隔离):`call_stack/frame_depth/stack_limit/thread_ref`
- `VmShared`(跨线程共享):`heap/registry/string_pool/exception_meta/class_mirrors/mirror_class/module_mirrors/unnamed_module/monitors/next_tid`
- `Vm`(单线程入口)= `VmShared` + 当前 `ThreadContext`
- C3 起:`start0 → std::thread::spawn(Arc<Mutex<VmShared>> + 新 ThreadContext)`

## Commit 序列

### C1 / Layer 4.41 — ThreadContext 物理分离 + monitor 真实化(本 commit)
- `ThreadContext` 独立类型;`Vm.thread: ThreadContext` 字段;call_stack/frame_depth/stack_limit/main_thread 全下沉
- `Vm.monitors: HashMap<Reference, MonitorState{owner,count}>`、`next_tid: u64`
- `monitorenter`:null→NPE;未锁→owner=cur,count=1;已持→count+1
- `monitorexit`:null→NPE;owner=cur→count-1(归零释放);否则→`IllegalMonitorStateException`
- `holdsLock(Object)Z`:owner==当前线程→true;null→NPE
- Thread 镜像:name="main"、tid=1(main)、daemon=false、priority=5、contextClassLoader=null
- `sleep0`→`std::thread::sleep`;`yield0`→`yield_now`;`start0`→桩(置 threadStatus + 同步跑 target.run())
- 闸门:`monitor_reentry.rs`/`synchronized_block.rs`/`thread_context.rs`;315 lib 全绿

### C2 / Layer 4.442 — 签名分离 + 共享态 Sync
- `interpret_with(&mut Frame, &Arc<Mutex<VmShared>>, &mut ThreadContext)`
- `Heap→Mutex<Heap>`;`RefCell→Mutex`(static_storage/init_state/flat_cache/class_modules)
- Vm 拆 VmShared;单线程下 Mutex 开销可接受

### C3 / Layer 4.443 — start0 真 spawn + join
- `Thread.start0 → std::thread::spawn` 跑 target.run();`JoinHandle` 表
- `Thread.join()` 阻塞;threadStatus NEW→RUNNABLE→TERMINATED

### C4 / Layer 4.444 — wait/notify/interrupt
- `Object.wait/notify/notifyAll`(per-对象 Condvar + IMSE 检查、虚假唤醒重试)
- `Thread.interrupt/isInterrupted`;`LockSupport.park/unpark`

## C1 TDD 步骤
- **S1 保绿重构**:ThreadContext 类型 + Vm 字段重组 + 方法转发;调用点 `vm.frame_depth`→`vm.thread.frame_depth`;315 全绿(无行为变化)
- **S2 红绿**:monitor 真重入 + IMSE(`tests/monitor_reentry.rs`)
- **S3 红绿**:holdsLock native
- **S4 红绿**:Thread 镜像 name/tid(currentThread.getName)
- **S5 红绿**:sleep0/yield0/start0 桩
- **S6 闸门**:`tests/synchronized_block.rs`(javac 编译 synchronized 块真字节码)
- **S7**:commit + memory;转 C2
