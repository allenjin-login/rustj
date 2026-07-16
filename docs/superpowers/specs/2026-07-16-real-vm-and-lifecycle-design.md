# 真 `Vm` 单例 + 生命周期 / 线程注册表 / Shutdown 设计

**日期**:2026-07-16
**层**:Phase V(承接 Phase G / B.4d 之后)
**前提**:`VmShared`→`Runtime`(2026-07)、`Vm`→`VmThread`(2026-07)两次重命名已完成。

---

## 1. 目标与现状洞察

**用户要求**:创建真正的 `Vm` 类,管理**线程 / 堆 / 虚拟机生命周期**。原则上 `Vm` 只有一个。

**核心洞察**:`Runtime`(`src/runtime/vm.rs:180`)**已经是 Vm 单例的 ~90%**——它拥有堆、注册表、Class/Module 镜像表、管程表、线程管理器,全字段 `Mutex`,以 `Arc<Runtime>` 跨线程共享、`'static`、`Send+Sync`。故本层**不是从零盖,是把 `Runtime` 提升为 `Vm` + 补三块缺失**:

| 缺失 | 现状 | 补 |
|---|---|---|
| 生命周期状态 | 无;Phase1/2/3 由 `launch.rs` 自由函数散调,无统一入口、无幂等保证 | `VmPhase` 状态机 + `VmThread::bootstrap()` 幂等入口 |
| 真线程注册表 | `ThreadManager.handles`(JoinHandle 表,join 后删除)非可遍历活线程集 | `ThreadManager` + `live` 活线程集(register/unregister/iter) |
| Shutdown | 完全没有 | `Runtime.addShutdownHook` 等 native + `Vm::shutdown()`(跑 hooks + join-all) |

**HotSpot 保真映射**:`Vm`↔`Universe`(`universe.hpp`,持 `CollectedHeap` + 主线程 + `is_init_completed`)、`ThreadManager`↔HotSpot `Threads`(`threads.hpp`,活 `JavaThread` 链 + `add/remove/threads_do`)、`VmThread`↔`JavaThread`(`javaThread.hpp`)。

**北极星理由(GC-ready 形状)**:`Vm` 同时持有 `heap` + `threads` 不是凑近——GC 根集 = 所有线程栈(来自 `ThreadManager`)+ 静态字段(来自 registry)+ 管程锁。HotSpot `Universe` 正是 `_collected_heap` + HotSpot `Threads` 同构。堆现在只增不收(CLAUDE.md §9.5),但**形状先摆成 GC-ready**,这是把堆与线程归同一 owner 的真正理由。

---

## 2. 架构

### 2.1 `Vm`(单例,折进 `Runtime`)

```rust
pub(crate) struct Vm {                  // Arc<Vm>, 'static, Send+Sync(原 Runtime 提升 + 扩容)
    // —— 共享可变态(原 Runtime 字段直移,逐字段 Mutex)——
    heap: Mutex<Heap>,
    registry: Option<Arc<ClassRegistry>>,        // 建后固定,无需 Mutex(G.1a runtime_classes 另锁)
    string_pool: Mutex<StringPool>,
    monitors: Mutex<HashMap<Reference, Arc<JavaMonitor>>>,
    exception_meta: Mutex<HashMap<Reference, ExceptionMeta>>,
    class_mirrors: Mutex<HashMap<String, Reference>>,
    mirror_class: Mutex<HashMap<Reference, String>>,
    module_mirrors: Mutex<HashMap<String, Reference>>,
    unnamed_module: Mutex<Option<Reference>>,

    // —— 线程管理(ThreadManager 扩容,保留名)——
    threads: ThreadManager,

    // —— 新增:生命周期 ——
    phase: Mutex<VmPhase>,
    main_thread: Mutex<Option<Reference>>,       // 主线程身份上提为单例(§4.3)
    shutdown_hooks: Mutex<Vec<Reference>>,       // §5
}
```

`Runtime` **退役**(字段全进 `Vm`)。`Arc<Runtime>` → `Arc<Vm>` 全替。`Runtime::new` → `Vm::new`。

### 2.2 `VmThread`(每线程执行句柄,形状不变)

```rust
pub struct VmThread {                   // 原 Vm,形状不变
    vm: Arc<Vm>,                           // 原 shared: Arc<Runtime>
    pub(crate) thread: ThreadContext,      // call_stack / frame_depth / stack_limit / thread_ref
}
```

`VmThread::new`/`from_shared`/`shared_arc` → `new`/`from_vm`/`vm_arc`。accessor(`heap()`/`registry()`/…)经 `self.vm.<field>` 转发(原 `self.shared.<field>`)。

### 2.3 `ThreadManager` 扩容(保留名)

```rust
pub(crate) struct ThreadManager {       // threads.rs:13,保留名(用户决定)
    next_tid: Mutex<u64>,
    handles: Mutex<HashMap<Reference, JoinHandle<()>>>,
    system_group: Mutex<Option<Reference>>,
    interrupt_flags: Mutex<HashMap<Reference, Arc<AtomicBool>>>,
    wait_targets: Mutex<HashMap<Reference, Reference>>,
    live: Mutex<Vec<Reference>>,              // ★ 新增:活线程集
}
```

保留 `ThreadManager` 名(用户决定;HotSpot 对应类为 `Threads`,但 rustj 沿用现名)。新 `live` 在 `start` 时 `register(this)`、`terminate` 时 `unregister(this)`;`iter_live()` 快照 owned `Vec<Reference>`(释 guard 后用)。

### 2.4 `VmPhase`

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum VmPhase { Created, Bootstrapping, Running, ShuttingDown }
```

粗粒度即可(Java 侧 `initLevel` 0..3 由 `launch.rs` 经字节码置,已是 Java 层事实源;Rust 侧 `VmPhase` 只跟踪 Rust 生命周期)。**不**复刻 `VM.initLevel` 到 Rust(避免双源真相)。

---

## 3. 生命周期

### 3.1 `VmThread::bootstrap(&mut self) -> Result<(), VmError>`(幂等)

唯一入口,串行驱动现有 `launch.rs` 三步。**签名抉择(2026-07-16 定 B)**:`launch.rs` 三步须
`&mut VmThread`(跑字节码),而 `Vm` 是 `Arc` 共享(无 `&mut Vm`)。两候选:

- (A) `Vm::bootstrap(&self)` 内部自建一次性主 VmThread —— HotSpot faithful(`Threads::create_vm`
  自建主线程),但调用方 VmThread 与 bootstrap 主 VmThread 分离,main_thread 身份须另存字段。
- (B) `VmThread::bootstrap(&mut self)` 复用调用方 VmThread —— 无一次性 VmThread;三步本就接
  `&mut VmThread`,签名零阻力;契合 rustj 当前「首个 VmThread = main」假设(`alloc_main_thread`)。

**选 B**:调用方的 VmThread 直接跑三步、即为主线程;phase 经 `self.vm.phase`(Mutex)跟踪。

```rust
impl VmThread {
    pub fn bootstrap(&mut self) -> Result<(), VmError> {
        {
            let mut p = self.vm.phase.lock().unwrap();
            match *p {
                VmPhase::Created => *p = VmPhase::Bootstrapping,
                VmPhase::Running | VmPhase::ShuttingDown => return Ok(()),  // 幂等
                VmPhase::Bootstrapping => return Err(InternalError("bootstrap 重入")),
            }
        }
        launch::initialize_system_class(self)?;         // Phase 1
        launch::bootstrap_module_system(self)?;         // Phase 2
        launch::bootstrap_java_lang_invoke(self)?;      // Phase 3 lite
        *self.vm.phase.lock().unwrap() = VmPhase::Running;
        Ok(())
    }
}
```

### 3.2 `Vm::shutdown(&self)`(§5)

`Running` → `ShuttingDown` → 跑 hooks → join-all live 线程 →(未来)VM 退出。

---

## 4. 线程管理

### 4.1 编排分层(Vm 编排 / VmThread 跑体)

| 现位 `impl VmThread`(threads.rs) | 归属 | 理由 |
|---|---|---|
| `start_thread`(spawn + 置 eetop/status + 存 handle) | **`impl Vm`**(经 `ThreadManager`) | VM 级:tid/register/handle/eetop 皆 `Vm.threads` 字段 |
| `run_thread_body`(虚分派 run + interpret_with) | 留 `impl VmThread` | 须 per-thread 栈 |
| `terminate_thread`(set TERMINATED + eetop=0 + notifyAll) | `impl VmThread`(调 `Vm.threads.unregister`) | 跑在子线程,但注销回 Vm |
| `join_thread` / `interrupt_thread` / 中断族 | `impl Vm` | 纯 `ThreadManager` 表操作 |

`Vm::spawn_thread(this)`:`this` Thread 实例 → register live + tid + 置 eetop/status + 存 handle + `thread::spawn(move || { child=VmThread::from_vm(Arc::clone); child.thread.thread_ref=Some(this); child.run_thread_body(this); child.terminate_thread(this); })`。子线程体异常→`dispatch_uncaught_exception`(留 VmThread,须跑字节码)→ terminate(注销 live)。

### 4.2 活线程集语义

- `register(r)`:`live.push(r)`。
- `unregister(r)`:`live.retain(|x| *x != r)`。
- `iter_live() -> Vec<Reference>`:owned 快照(锁内 clone、释锁返)。
- **解锁**:`Thread.enumerate(Thread[])`/`activeCount()`(顺延 native)、未来 GC 根集扫描、`shutdown` join-all。

### 4.3 `main_thread` 上提为单例

现状:`VmThread::main_thread()`(`threads.rs:53`)返 `self.thread.thread_ref`(惰性),实为「本 VmThread 的当前线程身份」(主 VmThread→主线程;子→子线程),**名实不符**且每 VmThread 各自惰性重派。

改:`Vm.main_thread: Mutex<Option<Reference>>`(VM 单例,bootstrap 时分配一次);`VmThread::current_thread()` 取「本线程身份」(= `self.thread.thread_ref`,子线程 spawn 时置);主 VmThread 的 `current_thread()` == `Vm.main_thread`。`Thread.currentThread()` native 据「当前线程」返。

---

## 5. Shutdown Hooks

### 5.1 注册

Java 侧 `Runtime.addShutdownHook(Thread)`/`removeShutdownHook(Thread)` 经 `ApplicationShutdownHooks`(单例,`IdentityHashMap<Thread,Thread>`)。rustj 简化为 **`Vm.shutdown_hooks: Mutex<Vec<Reference>>`** + 绑 native:

- `Runtime.addShutdownHook(Thread)V` → push。
- `Runtime.removeShutdownHook(Thread)Z` → retain 判等返是否移除。

**Step 0 源码核验**:`Runtime.addShutdownHook`(Runtime.java ~950)实际委派 `ApplicationShutdownHooks.add`;`Shutdown.shutdown()`(Shutdown.java)拉起钩子。本层**不**复刻完整 `Shutdown` 序列(halting / goUp 优先级),仅做:注册表 + `Vm::shutdown` 串行跑已注册 hook。

### 5.2 `Vm::shutdown(&self)`

```rust
pub fn shutdown(&self) -> Result<(), VmError> {
    { let mut p = self.phase.lock().unwrap(); *p = VmPhase::ShuttingDown; }
    let hooks = self.shutdown_hooks.lock().unwrap().clone(); drop(self.shutdown_hooks...);
    // 逐 hook:start 其 Thread(若未启动)+ join(确定性);或直接 interpret_with 跑 run()。
    for h in hooks { self.run_hook(h)?; }
    // join-all:对 live 非守护线程 join(经 handles 表);守护线程不阻塞退出。
    self.join_live_non_daemons()?;
    Ok(())
}
```

`run_hook`:hook 是 `Thread` 子类实例,start 之(若 threadStatus==NEW)→ join;或更简:在主 VmThread 上 interpret_with 虚分派 `run()V`(locals[0]=hook)。**抉择偏简**:首版直接 interpret_with 跑 hook.run()(不真起 OS 线程,串行),够覆盖「shutdown 时跑清理逻辑」语义;真并发 hook 顺延。

### 5.3 `VM.shutdown`

`jdk/internal/misc/VM` 的 shutdown 相关(`SYSTEM_SHUTDOWN` initLevel 上限)首版**不**绑(Java 侧 `initLevel` 已由 bootstrap 置 1/2;shutdown 序列的 initLevel 上行顺延)。本层 `shutdown()` 仅 Rust 侧 phase + hooks + join。

---

## 6. 共享拓扑与可测性

- **拓扑不变**:`Arc<Vm>` `'static` `Send+Sync`;`VmThread::from_vm(Arc::clone)`;`thread::spawn(move || Arc::clone(&vt.vm))`。无新 unsafe、无新依赖。
- **实例制,非进程全局**:HotSpot `Universe` 是静态全局单例(一进程一 VM);rustj 保持**每实例一个 `Vm`**(`Arc<Vm>`),测试里每测独立 `Vm`→**隔离**。这是为可测性做的**有意偏离 HotSpot**,记此权衡。
- `Vm: Send+Sync` 编译期断言保留(原 `assert_send::<Arc<Runtime>>` → `Arc<Vm>`)。

---

## 7. 迁移(rename / 改动面)

| 改动 | 量 | 风险 |
|---|---|---|
| `Runtime`→`Vm` 类型 + `Arc<Runtime>`→`Arc<Vm>` | ~439 类型位(vm.rs + 4 子模块 + 全 accessor 调用点) | 低(IDE 符号重命名;行为保持) |
| `Runtime::new`→`Vm::new`,`VmThread.{shared→vm, shared_arc→vm_arc, from_shared→from_vm}` | 单文件 + 调用点 | 低 |
| `self.shared.threads`→`self.vm.threads`(字段 accessor) | threads.rs + 调用点 | 低 |
| 字段 accessor `self.shared.X`→`self.vm.X` | ~全 accessor | 低 |
| **`vm` 局部变量保留** | ~1564 处 | 无(渐进改 `jt`,见 memory 重命名备注;本层不动) |
| `launch.rs` 三步签名不变(`&mut VmThread`) | 0 | — |
| 新增 `phase`/`main_thread`/`shutdown_hooks`/`live` 字段 + `bootstrap`/`shutdown`/`spawn_thread`/`register/unregister/iter_live` | 新代码 | TDD |

**`VmError` 不变**(非 `VmThreadError`,memory 重命名备注已记)。

---

## 8. TDD 子层分解(每子层 红→绿→闸门→commit)

- **V-1 重命名/折进(行为保持 refactor)**
  - `Runtime`→`Vm`、字段直移、`Arc<Vm>`、`VmThread.shared→vm`、accessor 转发。
  - 闸门:**全套既有测试绿**(行为零改)+ `Vm:Send+Sync`/`Arc<Vm>:Send+Sync` 断言绿 + clippy 净。
  - 无新 RED(纯 refactor;既有测试即安全网)。

- **V-2 `VmPhase` + `bootstrap()` 幂等入口**
  - RED:`bootstrap()` 第二次调用须幂等返 Ok(不重跑 Phase1/2/3);首调前置 `Running`。lib 测:mock 注册表断 phase 转换 + 幂等。
  - GREEN:实现 `phase` + `VmThread::bootstrap`(复用 self 跑 launch 三步;Option B)。
  - 闸门:`bootstrap` 跑后 `initLevel==2`、`javaLangInvokeInited==true`(复用 launch.rs tests 的断言)+ 幂等。

- **V-3 `ThreadManager` 活线程集 + `main_thread` 单例**
  - RED:`spawn_thread` 后 `iter_live()` 含该 Thread;`terminate` 后不含;`main_thread` 跨 `from_vm` 派生的 VmThread 稳定同引用。
  - GREEN:`live` register/unregister/iter_live;`main_thread` 上提。
  - 闸门:lib `threads_live_registry` 测(start N→live 见 N、join 后见 0)+ `main_thread_singleton` 测。

- **V-4 spawn/terminate 编排上移到 `Vm`**
  - `spawn_thread`/`join_thread`/中断族迁 `impl Vm`;`run_thread_body`/`terminate_thread`(调 unregister)/`dispatch_uncaught_exception` 留 VmThread。
  - 闸门:**既有线程闸门全绿**(thread_start_join / thread_interrupt / object_wait_notify / synchronized_block / thread_uncaught / thread_constructor)——行为保持。

- **V-5 Shutdown hooks + `Vm::shutdown()`**
  - RED:绑 `Runtime.addShutdownHook`/`removeShutdownHook`;`Vm::shutdown()` 跑注册的 hook(断 hook.run 副作用发生)+ join-all live。
  - GREEN:`shutdown_hooks` 表 + `shutdown()` 串行跑 hook + join live 非守护。
  - 闸门:lib `shutdown_runs_hooks` + 集成 `tests/shutdown_hook.rs`(javac:注册一 hook 写 static flag,shutdown 后断 flag 置)。

每子层一 commit(`-m` 多条,末尾 `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`)。

---

## 9. 风险与非目标

**风险**
- V-1 改动面大(~439 类型位)——靠 IDE 符号重命名 + 既有测试安全网;rust-analyzer 诊断过期,以 `cargo build --lib --tests`/clippy 为准(memory)。
- `VmThread::bootstrap` 复用调用方 VmThread(Option B)跑 launch 三步——launch.rs 内部 `&mut VmThread` 假设(§6 NLL trick)须仍成立;phase 经 `self.vm.phase`(Mutex),`Arc<Vm>` deref 后 `self.vm.heap` 等透明,预期无借用回归,但须验。
- shutdown hook 串行跑(不真起 OS 线程)——与 HotSpot「hook 各自线程并发」不同;记为已知简化。

**非目标(顺延)**
- 真 GC(heap 仍只增不收,§9.5)——但形状 GC-ready。
- 完整 `Shutdown` 序列(halting / 优先级 / daemon 阻止退出)。
- `Thread.enumerate`/`activeCount` native(活线程集已就位,绑 native 顺延)。
- 进程全局单例化(保持实例制)。
- `vm` 局部变量→`jt` 渐进改名(独立债,本层不动)。

---

## 10. HotSpot 保真引用

- `Universe`(`src/hotspot/share/memory/universe.hpp`):`_collected_heap`、主线程、`is_fully_initialized`。
- `Threads`(`src/hotspot/share/runtime/threads.hpp`):`_thread_list`、`add/remove`、`threads_do`(GC/safepoint visitor)。
- `JavaThread`(`src/hotspot/share/runtime/javaThread.hpp`):每线程栈 + `run`/`thread_entry`。
- `System.initPhase1/2/3`(`System.java:1720/1929/1952`):Phase1/2/3 引导序列(rustj `launch.rs` 等价)。
- `Runtime.addShutdownHook` / `ApplicationShutdownHooks` / `Shutdown.shutdown`。
