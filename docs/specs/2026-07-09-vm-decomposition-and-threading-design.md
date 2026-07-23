# Vm 分解 + 全套真线程 — 设计与路线图

**日期**:2026-07-09
**弧线**:Phase B.2.3b → B.3a → B.3b → B.3c
**承接**:`2026-07-08-thread-and-concurrency-roadmap.md`(B.1/B.2/B.3 总图)、`hotspot-rust-migration-project.md` B.2.3 段。
**北极星**:rustj 能跑**真多线程 Java 程序**——多 OS 线程共享一个对象堆,`synchronized` 跨线程互斥,`Thread.start0` 真起线程,`wait/notify` 线程协作。

---

## 1. 动机

`Vm` 当前承担 6 类职责(~35 法):堆/池、Class 镜像、Module 镜像、栈帧/异常轨迹、管程、线程镜像——典型 god object(§6「文件变大=职责过多」)。同时解释器单线程(`&mut Vm` 递归),`Thread.start0` 是空操作桩,管程非阻塞。

用户铁令(2026-07-09):**全套真线程** + **全提 5 管理器**(从 Vm 转移出去)+ **加线程管理器**。本设计把这两件事与 B.2.3b(共享态 Mutex 化)合并为一个连贯的并发弧线,分 4 层落地。

## 2. 关键设计决策

### 2.1 `Arc<VmShared>` 由 Vm 拥有(非视图借用)

`Vm<'a> { shared: Arc<VmShared<'a>>, thread: ThreadContext }` —— Vm **拥有**一个 `Arc<VmShared>`(共享指针)。

- **为何 Arc 非视图(`&'a VmShared`)**:视图要求 VmShared 外部拥有 → `Vm::new` 须改取 `&'a VmShared`。但 `tests/` 集成测试是**外部 crate**,只能用 `pub` 项 → 视图须把 `VmShared`/`VmShared::new` 升 `pub`(泄露内部),且 ~80 `Vm::new` + ~17 `Vm::default()` 构造点全改写。**Arc 下 `Vm::new(&'a ClassRegistry) -> Vm<'a>` 签名不变**(内部 `Arc::new(VmShared::new(Some(registry)))`),`Default` 亦不变(`Arc::new(VmShared::new(None))`)→ **构造点零改动**,`VmShared` 保持 `pub(crate)`。线程派生经 `Vm::from_shared(Arc::clone(&vm.shared))`(`pub(crate)`)。
- **E0502 处理**:Arc 的 `deref` 使 guard 借用链绑回 `&self`(vm)→ 持 guard 跨 `&mut vm` 冲突。**对策:管理器封装锁定**(见 2.4)——管理器法在方法内部 `lock()`→操作→`drop(guard)`→返 owned 值(Reference/Slot/String 等 Copy 或 owned);调用方永不持 guard → 无 E0502。这正与 5 管理器分解同一件事(管理器=封装锁+owned 返回)。
- §6 NLL `&'a` registry 模式保留:`registry: Option<&'a ClassRegistry>` 仍是 `Copy` 的 `&'a` 存储引用,`vm.registry()` 读出后与 `&self` 解耦。
- `interpret_with(&mut Frame, &mut Vm)` 签名不变。

### 2.2 共享用 `std::thread::scope`(非 detach)

scoped 线程允许借/持有非 `'static` 数据 → `Arc<VmShared<'a>>`(registry 借用 `'a`)可在 scope 内克隆共享,registry **不必改 Arc<'static>**(避免全 `vm.registry()` 调用点涟漪)。Thread.start0 在 scope 内 `spawn`:子线程 `Vm::from_shared(Arc::clone(&vm.shared))` 共享同一 VmShared + 新 ThreadContext(新 tid)跑 `target.run()` 真字节码;join = scope 的 join。

> **取舍**:scope 要求线程在 VM 生命周期内 join,不能真 detach。Java daemon/长寿命线程顺延(须 registry→Arc<'static>,涟漪大)。当前弧线覆盖 join-able 线程(绝大多数用例)。

### 2.3 真管程 `JavaMonitor`(std Mutex + Condvar,零 unsafe 零依赖)

```
struct JavaMonitor {
    inner: Mutex<MonitorInner>,   // { owner: Option<Reference>, count: u64 }
    entry: Condvar,               // monitorenter 争用阻塞
    // B.3c 增:wait_set: Mutex<Vec<...>> + wait_cvar: Condvar
}
```
MonitorManager:`Mutex<HashMap<Reference, Arc<JavaMonitor>>>`(惰性 per-object)。`Arc<JavaMonitor>` 句柄离开 registry 锁后单独 lock → **阻塞时不持 registry 锁**(防死锁)。owner = Thread 镜像句柄(每 ThreadContext.thread_ref);重入 count++、归零 owner=None + `entry.notify_one`。

### 2.4 五管理器(组合进 VmShared,各自 Mutex 状态,封装锁定)

| 管理器 | 原Vm 法 | 状态 |
|---|---|---|
| `ClassMirrors` | intern_class_mirror / mirror_internal_name / alloc_class_mirror_instance / populate_class_mirror_fields / set_class_instance_field / set_instance_field_by_name | `Mutex<HashMap<String,Reference>>` + `Mutex<HashMap<Reference,String>>` |
| `ModuleMirrors` | intern_named_module / unnamed_module / alloc_module_instance / module_for_class | `Mutex<HashMap<String,Reference>>` + `Mutex<Option<Reference>>` |
| `ExceptionTrace` | record_trace / record_cause / record_message / frame_source / frame_location_suffix / exception_frames / format_trace | `Mutex<HashMap<Reference,ExceptionMeta>>` |
| `MonitorManager` | monitor_enter / monitor_exit / holds_lock | `Mutex<HashMap<Reference,Arc<JavaMonitor>>>` |
| `ThreadManager` | main_thread / alloc_main_thread / next_thread_tid +(B.3b)线程表 | `Mutex<u64>` + `Mutex<HashMap>` |

- 栈帧法(push_frame/pop_frame/frame_class_at/set_top_frame_pc)本属 `ThreadContext`(call_stack 已在其内)→ 下沉到 `impl ThreadContext`。
- **方法组织**:各管理器子模块文件(`mirrors.rs`/`monitors.rs`/`threads.rs`/`exceptions.rs`)以 `impl<'a> VmShared<'a>` 块提供;**封装锁定**:法内 `lock()`→操作→drop→返 owned(Reference/Slot/Option<String>/bool 等)。Vm 作薄门面 `vm.foo()` → `self.shared.foo()` 转发 —— **方法调用点零改动**(仅 Vm::new 构造不变,见 2.1)。
- **drop-before-recurse**(§6 纪律,B.2.1 已立):管理器法若需在持锁后递归 `&mut self`(如 intern_class_mirror 缓存未命中→alloc_class_mirror),须先 `drop(guard)` 再 `&mut self`;同字段重锁须 drop 再 lock(防 std Mutex 非重入自死锁)。
- **借用协作**:管理器法多需访问堆/registry。`&self`(=&VmShared via Arc deref)可锁多字段(细粒度 per-field Mutex)。

### 2.5 Vm: Send + Sync

`Vm: Send` ⟸ `Arc<VmShared>: Send` + `ThreadContext: Send`;`Arc<VmShared>: Send+Sync` ⟸ `VmShared: Send+Sync` ⟸ 各字段 `Send+Sync`(`Mutex<T>: Send+Sync ⟸ T: Send`,Heap/HashMap/u64/Option 皆 Send;`&'a ClassRegistry: Send+Sync` ⟸ ClassRegistry: Sync,B.2.1 已达)。`sync_assertions::{vm_is_sync, vm_is_send}` 守卫。

## 3. 分层路线图(每层:brainstorm→spec→红→绿→javac 闸门→commit,§4)

### B.2.3b — 5 管理器提取 + Arc/Mutex(地基)
- 5 管理器结构 + 状态 Mutex 化 + 子模块 `impl VmShared` 法(封装锁定,owned 返回)+ Vm 薄门面。
- `Vm.shared: Arc<VmShared>`;`Vm::new(&'a ClassRegistry)`/`Default` **不变**;新增 `Vm::from_shared(Arc<VmShared>)`(`pub(crate)`)。
- Heap/StringPool → `Mutex`;`heap`/`heap_mut`/`string_pool`/`string_pool_mut` accessor 改返 `MutexGuard`(inline 调用点经 `Deref` 不破)。
- **~30 处跨语句 `&Oop`/`&str` 绑定**(clinit:56 / exception / invoke×6 / java_io / jdk_internal×N 等)+ **~15 内部法**(intern_class_mirror / format_trace / monitor_* / alloc_main_thread 等)→ 封装为 owned 返回 / drop-before-recurse。
- **行为保持**(单线程全绿);sync_assertions 仍绿。
- **闸门**:全套现有测试 + clippy + 零 unsafe。

### B.3a — 真管程(JavaMonitor + Condvar)
- `JavaMonitor` 结构;MonitorManager 惰性 `HashMap<Reference, Arc<JavaMonitor>>`。
- `monitor_enter`:owner==本线程→count++;否则 `entry.wait` 阻塞至无主;null→NPE。
- `monitor_exit`:count--,归零 owner=None + `notify_one`;未持/owner 不符→IMSE。
- `holds_lock`:读 owner==本线程 && count>0。
- **闸门**:javac 多线程 synchronized 互斥门(两线程同对象 monitorenter 串行)+ 重入 + IMSE + 单线程回归。

### B.3b — Thread.start0 真起线程 + join
- Thread.start0 native:`std::thread::scope` 内 spawn,target = Thread.target 字段(Runnable 镜像);`Vm::from_shared(Arc::clone(&vm.shared))` + 新 ThreadContext(新 tid)跑 `target.run()` 真字节码。
- Thread.join:scope join(或 join 状态 + Condvar 协调)。
- ThreadManager 线程表:live Thread 句柄 + 状态(new/runnable/terminated)。
- **闸门**:javac 两线程并发递增共享计数(synchronized 块内)→ 正确总数;join 后子线程 terminated。

### B.3c — wait/notify/notifyAll
- `JavaMonitor` 增 `wait_set: Mutex<Vec<...>>` + `wait_cvar: Condvar`。
- Object.wait(J)V:释本线程管程(owner=None,count 清零)→ `wait_set` 入队 → `wait_cvar.wait` 阻塞 → 唤醒后重获管程。
- notify/notifyAll:从 wait_set 取一个/全部 `wait_cvar.notify`。
- native 桥:Object.wait/notify/notifyAll、超时变体顺延。
- **闸门**:javac 生产者-消费者(两线程经 wait/notify 协作)。

## 4. 约束与边界

- `#![deny(unsafe_code)]` 不放宽;零依赖(std::sync/Arc/Condvar/Scope 全安全封装)。
- 不改 `jdk-master` 源码;移植语义(管程移植 `ObjectSynchronizer::enter/exit`)。
- bind native 描述符以本机 jmod(jdk-25.0.2)为准。
- **顺延**:daemon/长寿命 detach 线程(须 registry→Arc<'static>);Thread.interrupt;优先级/线程组;park/unpark;真实 itable/vtable;GC。

## 5. 风险

- **B.2.3b 封装重构面**:~30 外部 + ~15 内部位需逐个封装/drop-before-recurse(非纯机械,须思考 owned 返回形态)。对策:TDD 每管理器独立红→绿;`cargo check` 驱动;先 vm.rs 内部自洽再清外部。**优势(相对视图)**:Vm::new/Default 不变 → src/+tests/ 构造点零改动,VmShared 保持 pub(crate)。
- **std Mutex 非重入**:管理器法须 drop-before-recurse(B.2.1 纪律);monitor_enter 阻塞前须释所有 VmShared 锁。
- **Arc + scope 借用**:VmShared 须比所有 scoped 线程长寿 → VM 入口持 VmShared(经 Vm 拥有 Arc),scope 包整个运行期;子线程 `Arc::clone` 共享。
