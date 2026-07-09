# Thread 与并发路线图(Phase B)

**日期**:2026-07-08
**状态**:设计(待用户确认起步范围)
**起源**:用户方向变更——"解决线程问题、实现 Thread 类;把 `call_stack: Vec<CallFrame>` 从 Vm 移除(线程持有);还有 monitorenter/monitorexit"。AskUserQuestion 答复:**Vm 与 ThreadContext 完全分离** + **真 OS 线程(`std::thread`)**。

---

## 1. 目标(北极星对齐)

rustj 北极星 = 加载并运行真实 `java.base`。当前已**结构性加载 7332/7332=100%**(Layer 4.32)、单线程端到端跑真 Integer/String/集合/indy/Class/Module/反射/动态库/NIO 入口。Thread 与并发是"运行更复杂真 Java 程序"的下一前线,但**非加载 java.base 的必需**(单线程已加载)。

用户要求把线程做成一等公民:`call_stack` 归属线程、`monitorenter/monitorexit` 真实管程、`Thread` 真对象、最终 `start0` 起真 OS 线程。

## 2. 现状架构(单线程假设,CLAUDE.md §6)

- `Vm<'a>` 持有全部执行态:`heap: Heap(Vec<Oop>)`、`registry`、`string_pool`、`call_stack: Vec<CallFrame>`、`frame_depth/stack_limit`、`exception_meta/class_mirrors/mirror_class/module_mirrors: HashMap`、`unnamed_module/main_thread`。
- 类级可变状态用 `RefCell`(`LoadedClass::{static_storage, flat_cache, init_state}`、`ClassRegistry::class_modules`)——§6 明定。
- `interpret_with(&mut Frame, &mut Vm)` → `run` → `invoke`(同 `&mut Vm`)→ `native::invoke`(同)→ `launch` 全链**独占 `&mut Vm`**。§6 借用技巧(`'a` 不绑 `&self`)建立在此独占上。
- `monitorenter/monitorexit`(`mod.rs:1325-1336`):弹 objref + null→NPE 的**空操作**,无重入计数、无 IMSE。
- `Thread.currentThread`(Layer 4.40):惰性 `new_instance`(不跑 `<init>`)的 main 单例。

## 3. 真并发的架构冲击(why 一步到不了)

`std::thread` 要求跨线程共享的状态 `Send + Sync`。当前障碍:

| 状态 | 现载体 | 障碍 | 真并发要求 |
|---|---|---|---|
| `Heap`(`Vec<Oop>`) | `&mut self` 方法 | 非 `Sync` | `Mutex<Heap>` 或分片锁(alloc 是热路径) |
| `LoadedClass.static_storage` | `RefCell<Vec<Slot>>` | `RefCell` 非 `Sync` | `Mutex`/`RwLock` |
| `LoadedClass.{flat_cache, init_state}` | `RefCell` | 同上 | 同上 |
| `ClassRegistry.class_modules` | `RefCell<HashMap>` | 同上 | 同上 |
| `Vm` 共享表(string_pool/exception_meta/mirrors) | `HashMap` + `&mut Vm` | `&mut` 不能跨线程 | `Arc<Mutex<VmShared>>` |
| 调用栈(call_stack/frame_depth/stack_limit) | `Vm` 字段 | 应隔离 | 每 `ThreadContext` 独有 |
| `&mut Vm` 全链签名 | `interpret_with/invoke/native` | `&mut` 不 `Sync` | 拆 `Arc<Mutex<VmShared>> + ThreadContext` |
| 对象管程 | 无 | wait/notify 需 Condvar | per-对象 `Mutex+Condvar`(或 owner/count 表 + park) |
| `start0/join` | 无 | OS 线程生命周期 | `JoinHandle` 表 + 线程退出同步 |
| `Thread.interrupt/wait/notify` | 无 | 协作式中断 + 条件变量 | park/unpark(`Condvar`)|

`Oop`/`Slot`/`Reference` 全是纯数据 → 自动 `Send`,堆加锁即可 `Sync`(无需重造对象模型)。障碍**集中在 `RefCell` 与 `&mut Vm`**——即 §6 的核心设计本身。

**结论**:真并发是**数十层**的工程,且会让解释器热路径(getfield/putfield/invoke/alloc)全部加锁,在无 GC 的堆上进一步拖慢。必须分阶段,每阶段独立交付 + 闸门 + commit。

## 4. 三阶段路线图

### Phase B.1 —— ThreadContext 分离 + monitor 真实化(单线程语义,建议本层)

**价值**:调用栈归属正确(线程持有)、`synchronized` 块语义忠实(重入/IMSE)、`Thread` 真对象。**不破坏现有 315 lib + 全部集成闸门**(对外 API 兼容转发)。

- **`ThreadContext` 独立类型**:`{ call_stack: Vec<CallFrame>, frame_depth: u32, stack_limit: u32, thread_ref: Reference }`。`call_stack` **不再是 `Vm` 直接字段**——下沉到 `ThreadContext`(用户要求)。
- **Vm 持当前线程上下文**(过渡形态):`Vm { shared..., current_thread: ThreadContext }`。Vm 的 `push_frame/pop_frame/set_top_frame_pc/frame_class_at/frame_depth/stack_limit/with_stack_limit/record_trace/exception_frames` **转发**到 `self.current_thread.*`(`exception_meta` 仍留 Vm——异常对象跨"线程"共享,且键为句柄)。
- **`monitorenter/monitorexit` 真重入**:`Vm.monitors: HashMap<Reference, MonitorState{ owner: Reference, count: u32 }>`。enter:未锁→owner=当前线程/count=1;已持有→count+1;被他人持→(单线程)不会发生。exit:count-1,归零释放;owner 不匹配或 count==0 → `IllegalMonitorStateException`。null objref → NPE(保留)。
- **`holdsLock(Object)Z` native**(Thread.java:2178):查 `monitors[obj].owner == 当前线程`。
- **Thread 镜像核心字段**:惰性填 `name`("main")、`tid`(递增,首=1)、`daemon`(false)、`priority`(NORM_PRIORITY=5)、`group`(null 桩)、`contextClassLoader`(可挂 getSystemClassLoader 结果)。`Thread.currentThread` 返此镜像(替代 4.40 的裸单例)。
- **`start0/sleep0/yield0` 单线程桩**:`sleep0(nanos)`→`std::thread::sleep`;`yield0`→`std::thread::yield_now`;`start0`→**桩**:标记 `threadStatus` 已启动、**同步**在当前线程跑 `target.run()`(等价 B.3 前"线程不并发"语义),并在文档/注释标明 B.3 升级为真 spawn。
- **native 清单**:`holdsLock`、`getNextThreadIdOffset`(Unsafe 字段偏移,可桩)、`sleep0`/`yield0` 桩;`start0` 桩;`interrupt0`/`isAlive` 等顺延。

**闸门**:新 `tests/thread_context.rs`(call_stack 随线程、monitor 重入、IMSE、holdsLock、currentThread.name/tid)、`tests/monitor_reentry.rs`、`tests/synchronized_block.rs`(javac 编译 `synchronized(obj){}` 真字节码跑通)。

### Phase B.2 —— 共享态加锁(`Send + Sync` 基础设施)

为 B.3 铺路:把 `RefCell` 全替为 `Mutex`/`RwLock`,`Heap` 加锁,`Vm` 拆 `Arc<Mutex<VmShared>>`,`interpret_with` 签名真分离(`&Arc<Mutex<VmShared>>, &mut ThreadContext`)。**仍单线程执行**(无 spawn),但结构 `Sync`。工作量最大、最易破坏闸门——逐字段迁移 + 全程闸门守护。

### Phase B.3 —— 真并发语义

`start0` → `std::thread::spawn` 跑 `target.run()`(新线程持自己的 `ThreadContext` + `Arc` 到共享态);`Thread.join`(`JoinHandle` 表);`Object.wait/notify/notifyAll`(per-对象 `Condvar`,依赖 monitor);`Thread.interrupt`/`isInterrupted`;`park/unpark`(Unsafe/LockSupport)。JMM/happens-before、内存可见性顺延(当前靠 Mutex 的释放-获取序)。

## 5. 风险与权衡

- **B.1 爆炸半径**:`call_stack` 下沉到 `ThreadContext` + 转发,触及 vm.rs/interpret_with/invoke/native/launch 多处,但**对外签名不变**→ 315 闸门应稳。monitor 是新增 HashMap,不影响现有路径。
- **B.2 是最大风险点**:`RefCell→Mutex` 改变借用模型,可能触发 borrow checker 大面积返工;热路径加锁显著拖慢(单线程下 `Mutex` 仍有开销)。建议 B.2 内部再分若干子层(Heap→static_storage→init_state→flat_cache→class_modules→Vm 共享表)。
- **B.3 语义正确性**:wait/notify 需严格"在 synchronized 块内"检查(IMSE)、虚假唤醒重试、interrupt 与 wait 的交互——易出微妙 bug,需大量并发闸门。
- **GC 缺位**:多线程下堆只增不收会更快膨胀;B.3 后 GC 优先级上升。
- **性能**:全锁化解释器在单线程程序上会变慢;HotSpot 用复杂锁优化(偏向锁/轻量级锁/CAS),rustj 顺延。

## 6. 建议

**从 Phase B.1 起步**。理由:(1) 它是 B.2/B.3 的逻辑前提(ThreadContext 必须先存在);(2) 独立交付价值(调用栈归属正确 + synchronized 忠实 + Thread 真对象);(3) 不破坏现有闸门、风险可控、可在本会话完成并 commit。B.2/B.3 作为后续独立大层,各自有 spec + plan。

若用户坚持本层即要 `start0` 真并发,则必须**先完成 B.2**(共享态 Sync),否则 `std::thread::spawn` 无法持 `&mut Vm`——这会令本层膨胀至数十层、且中途破坏大量闸门。故即便终点是真并发,B.1 仍是不可跳过的第一步。
