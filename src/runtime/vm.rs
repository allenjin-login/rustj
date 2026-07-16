//! 执行上下文:对象堆 + 类注册表 + 帧深度计数。对应 HotSpot `JavaThread`
//! 执行所需的共享状态 + 栈深度检查。
//!
//! 4.1:对象/字段/`invokestatic` 路径需注册表([`Vm::new`])。运行时异常(NPE/算术
//! 异常等)统一为 `ThrownException`、须在堆上分配异常对象——故即便纯数值字节码也可能
//! 需要注册表(便捷入口 `interpret()` 自带注册表);[`Vm::default`] 仅空堆 + 无注册表,
//! 供确不抛异常的纯数值测试。4.2b:帧深度计数 + 可配置上限([`Vm::with_stack_limit`]);
//! 超限时解释器抛 `java/lang/StackOverflowError`(统一为 `ThrownException`)。
//!
//! Phase B.2.3b(T7)职责分解:Class/Module 镜像法 → [`mirrors`]、对象管程 →
//! [`monitors`]、异常元数据 + 栈轨迹 → [`exceptions`]、线程管理器 + main 线程 → [`threads`]。
//! 本模块留核心结构([`Vm`]/[`Runtime`]/[`ThreadContext`]/[`CallFrame`]/[`MonitorState`])、
//! 构造、堆/池/注册表 accessor、栈帧法(T8 下沉 [`ThreadContext`])。

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::oops::ClassRegistry;
use crate::runtime::heap::Heap;
use crate::runtime::string_pool::StringPool;
use crate::runtime::Reference;
use crate::runtime::interpreter::{launch, VmError};

/// VM 生命周期阶段(Phase V)。粗粒度跟踪 Rust 侧生命周期——Java 侧 `initLevel` 0..3 仍由
/// `launch.rs` 经字节码置(Java 层事实源,不复刻到 Rust 避免双源真相)。`VmThread::bootstrap`
/// Created→Bootstrapping→Running;`Vm::shutdown`(V-5)Running→ShuttingDown。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VmPhase {
    Created,
    Bootstrapping,
    Running,
    /// `Vm::shutdown`(V-5)构造;本层 bootstrap 幂等匹配已引用,暂无构造点。
    #[allow(dead_code)]
    ShuttingDown,
}

mod exceptions;
mod mirrors;
mod monitors;
mod threads;

/// 默认帧深度上限。高于 ackermann(3,3) 的递归深度(~120),正常小测试不会误触;
/// 可经 [`Vm::with_stack_limit`] 调整(SOE 测试用小值快速触发)。
pub const DEFAULT_STACK_LIMIT: u32 = 512;

/// **哨兵"偏移"**:堆外「下一线程 tid」计数器,由 `Thread.getNextThreadIdOffset()`
/// (Thread.java:2628)返回。HotSpot 把该计数器放堆外(注释:"off-heap and shared with the VM");
/// rustj 以 [`super::vm::threads::ThreadManager`] 的 `next_tid` 承载。`Unsafe.getLongVolatile(null, 此值)`
/// 与 `compareAndSetLong(null, 此值, ..)`(jdk_internal.rs)特判路由至此——解锁 `ThreadIdentifiers.next()`
/// = `getAndAddLong(null, NEXT_TID_OFFSET, 1)`(Thread 构造器 tid 分配)。负值避开实例 ord(小正)
/// 与数组偏移(≥ ARRAY_BYTE_BASE_OFFSET=16)的命名空间。
pub(crate) const NEXT_THREAD_ID_OFFSET: i64 = i64::MIN + 7;

/// `Thread$FieldHolder.threadStatus` 的 JVMTI 状态位(`javaThreadStatus.hpp:33-60`)。`holder.threadStatus`
/// 原始 int 即此位掩码(NEW=0)。`Thread.start()`(Thread.java:1468)据此 `!= 0` 抛 `IllegalThreadStateException`;
/// 子线程终止时 `ensure_join`(javaThread.cpp:674)复位为 TERMINATED。`VM.toThreadState` 按位解码。
#[allow(dead_code)] // NEW=0 为初值默认,无需显式写入(仅文档 JVMTI NEW 状态)。
pub(crate) const THREAD_STATUS_NEW: i32 = 0;
pub(crate) const THREAD_STATUS_RUNNABLE: i32 = 0x0001 | 0x0004; // JVMTI_THREAD_STATE_ALIVE | _RUNNABLE
pub(crate) const THREAD_STATUS_TERMINATED: i32 = 0x0002; // JVMTI_THREAD_STATE_TERMINATED

/// 一个 Java 栈帧的身份切片(供栈轨迹):声明类内部名 + 方法名 + 抛出点 bci。
///
/// `pc` = 当前指令起始字节码偏移(`run()` 分派前写入);抛出时即抛点 bci,
/// 陷入被调用者后冻结于调用点 invoke bci。行号由 `format_trace` 经类注册表
/// 查 `LineNumberTable`(最大 `start_pc ≤ pc`)解析。不含描述符(重载按名+pc 范围匹配,
/// 顺延)。拥有 `String`:`push_frame` 来源生命周期不一(字节码帧借自常量池 / native 帧
/// 借自调用方局部串),统一 owned 入栈最简。
#[derive(Debug, Clone)]
pub struct CallFrame {
    pub class: String,
    pub method: String,
    pub pc: u32,
}

/// 每线程执行上下文(对应 HotSpot `JavaThread` 的栈区 + 线程身份)。Vm 单线程入口下,Vm 持
/// "当前线程"的 ThreadContext;Phase B.3 真并发后每 OS 线程一个,经 `Arc<Mutex<Runtime>>` 共享。
///
/// 持 Java 调用栈、帧深度(SOE 检测)、上限、线程镜像句柄——皆为**线程隔离态**(CLAUDE.md §6
/// "调用栈归属线程"的落实,Phase B.1 起 call_stack 不再是 Vm 顶层字段)。镜像句柄惰性分配:
/// `Vm::new` 时 Thread 类未必加载,首调 `currentThread` 时经 [`Vm::main_thread`] 填入。
pub(crate) struct ThreadContext {
    /// 当前活动 Java 调用栈(逐帧 push/pop),供栈轨迹捕获。
    pub(crate) call_stack: Vec<CallFrame>,
    /// 当前嵌套帧数(进入一帧 +1,退出 −1;SOE 检测用)。
    pub(crate) frame_depth: u32,
    /// 帧深度上限;`frame_depth >= stack_limit` 时再调用 → StackOverflowError。
    pub(crate) stack_limit: u32,
    /// 此上下文对应的 `java/lang/Thread` 镜像句柄(惰性;`Thread.currentThread` 返此)。
    pub(crate) thread_ref: Option<Reference>,
}

impl ThreadContext {
    /// 主线程上下文(main 线程单例;镜像句柄惰性,thread_ref 初始 None)。
    pub(crate) fn new_main() -> Self {
        Self {
            call_stack: Vec::new(),
            frame_depth: 0,
            stack_limit: DEFAULT_STACK_LIMIT,
            thread_ref: None,
        }
    }

    /// 入一个 Java 栈帧(类内部名 + 方法名)。`interpret_with` 入口与 `native::invoke`
    /// 入口各推一帧。克隆入 owned [`CallFrame`](各来源生命周期不一)。`pc` 初始 0,
    /// 由 [`Self::set_top_frame_pc`] 在 `run()` 分派前持续刷新。
    pub(crate) fn push_frame(&mut self, class: &str, method: &str) {
        self.call_stack.push(CallFrame {
            class: class.to_string(),
            method: method.to_string(),
            pc: 0,
        });
    }

    /// 退一个 Java 栈帧(与 `push_frame` 配对;`interpret_with`/`native::invoke` 出口调)。
    pub(crate) fn pop_frame(&mut self) {
        self.call_stack.pop();
    }

    /// 自栈顶(最新帧)向下第 `depth_from_top` 层帧的声明类内部名(0 = 栈顶)。
    ///
    /// 供 `Reflection.getCallerClass`(@CallerSensitive 基础设施)等栈帧回溯 native 用。
    /// 栈深不足(无对应层)→ `None`。`native::invoke` 已为本 native 推入自身帧(即栈顶),
    /// 故 `depth_from_top=2` = "调用 getCallerClass 的方法"的**调用者**。
    pub(crate) fn frame_class_at(&self, depth_from_top: usize) -> Option<&str> {
        let n = self.call_stack.len();
        n.checked_sub(1)
            .and_then(|last| last.checked_sub(depth_from_top))
            .and_then(|i| self.call_stack.get(i))
            .map(|f| f.class.as_str())
    }

    /// 刷新**栈顶**帧的 bci(`run()` 分派前调,记当前指令起始)。抛出时即抛点 bci;
    /// 调用者陷入被调用者后,其顶帧 pc 冻结于 invoke 点(其 run loop 挂起前最后写入)。
    /// 栈为空(匿名纯算术帧)时无操作。
    pub(crate) fn set_top_frame_pc(&mut self, pc: u32) {
        if let Some(top) = self.call_stack.last_mut() {
            top.pc = pc;
        }
    }
}

/// 对象管程(对应 HotSpot `ObjectMonitor` 的 rustj 阻塞子集;Phase B.3a)。每对象惰性分配一个,
/// `entry` Condvar 在 `monitor_enter` 被他人持有时阻塞等待,owner 归零时 `notify_one` 唤醒等待者。
/// Phase B.3c:`wait_cvar` 给 `Object.wait` 阻塞用,`notify`/`notifyAll` 推 `wake_seq` 并 `wait_cvar`
/// 唤醒;`waiters` 记等待者数(`ObjectMonitor::_wait_set` 的 rustj 子集),空集时 notify no-op。
///
/// B.1 起 owner 判定 + 重入计数;B.3a 前重入不阻塞(无 Condvar)→ 真并发丢失更新;B.3a 阻塞至空闲。
pub(crate) struct JavaMonitor {
    /// 锁态:`owner` = 持有者 Thread 镜像句柄(`None` = 空闲)、`count` = 重入计数、`waiters` = wait
    /// 等待者数(B.3c)、`wake_seq` = notify/notifyAll 推进的唤醒序号(B.3c:wait_timeout_while 谓词)。
    pub(crate) inner: Mutex<MonitorInner>,
    /// 入口条件变量:被他人持有时 `wait`,owner 释放时 `notify_one`。
    pub(crate) entry: Condvar,
    /// `Object.wait` 条件变量(B.3c):waiter 释管程后在此阻塞;notify/notifyAll 推 `wake_seq` 后唤醒。
    pub(crate) wait_cvar: Condvar,
}

/// 管程锁态(`JavaMonitor::inner` 的载荷)。`owner`/`count`/`waiters`/`wake_seq` 经 `inner` Mutex 保护。
pub(crate) struct MonitorInner {
    pub(crate) owner: Option<Reference>,
    pub(crate) count: u64,
    /// `Object.wait` 等待者计数(B.3c)。`ObjectMonitor::_wait_set` 大小的 rustj 子集;notify/notifyAll
    /// 据 `>0` 判是否有等待者(空集 → no-op,objectMonitor.cpp:2111/2139)。
    pub(crate) waiters: u64,
    /// 唤醒序号(B.3c):每次 notify/notifyAll 自增。waiter 入 wait 时记当前值作谓词,
    /// `wait_cvar.wait_timeout_while(guard, |i| i.wake_seq == my_seq)` —— 抗 spurious wakeup
    ///(谓词真=未被 notify→ 继续等;notify 推序号→ 谓词假→ 唤醒)。
    pub(crate) wake_seq: u64,
}

impl JavaMonitor {
    /// 构造空闲管程(`owner=None`、`count=0`、`waiters=0`、`wake_seq=0`)。
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(MonitorInner {
                owner: None,
                count: 0,
                waiters: 0,
                wake_seq: 0,
            }),
            entry: Condvar::new(),
            wait_cvar: Condvar::new(),
        }
    }
}

/// **跨线程共享态**(Phase B.2.3a/b):Vm 持有的「所有线程共享」字段集合——对象堆、类注册表、
/// 字符串池、管程表、异常元数据、Class/Module 镜像表、线程管理器。逐字段 `Mutex` 包装,
/// `Vm.shared` 持 `Arc<Runtime>`——多线程经 `Vm::from_shared(Arc::clone(&vm.shared))` 派生
/// 各自 Vm、共享并发改写。对应 HotSpot 跨 `JavaThread` 共享的全局结构(`JavaHeap`/
/// `SystemDictionary`/`StringTable`/`ObjectMonitor` 表等);线程隔离态留 [`Vm::thread`]。
/// `pub(crate)`:`from_shared` 签名须命名。
pub(crate) struct Vm {
    /// 对象堆(Mutex:Phase B.2.3b 共享态——`Arc<Runtime>` 多线程并发改堆的前置)。
    heap: Mutex<Heap>,
    /// 类注册表。**B.3.0 移除 `'a`**:owned `Arc<ClassRegistry>`(`load_or_replace` 须 `&mut`,
    /// 故注册表先 owned 载入完毕、再 `Arc::new` 包后传 [`Vm::new`])。owned clone 经
    /// [`Vm::registry`] 出借,保 §6 NLL trick(`Arc` 独立 local 绑定,不借 `&self`)。
    registry: Option<Arc<ClassRegistry>>,
    /// 字符串 intern 池(4.8):文本 → 堆引用,以本 Vm 的堆为后盾。Mutex(B.2.3b 共享态)。
    string_pool: Mutex<StringPool>,
    /// 对象管程表(对象句柄 → per-object `JavaMonitor`)。Phase B.3a:每对象惰性分配一个
    /// `Arc<JavaMonitor>`(owner/count + `entry` Condvar 阻塞);跨线程共享态。
    pub(crate) monitors: Mutex<HashMap<Reference, Arc<JavaMonitor>>>,
    /// 线程管理器(tid 分配;B.3b 增线程表)。T7 从顶层 `next_tid` 收编为 [`threads::ThreadManager`]。
    pub(crate) threads: threads::ThreadManager,
    /// 异常 → 元数据(帧 / cause / detailMessage),键 = 异常对象句柄。Mutex(B.2.3b 共享态)。
    /// `ExceptionMeta` 在 [`exceptions`](`pub(super)` 供本字段命名类型)。
    exception_meta: Mutex<HashMap<Reference, exceptions::ExceptionMeta>>,
    /// Class 镜像 intern 表(4.10t):内部类名(`java/lang/Foo`、`int`、`[I` …)→ 唯一 Class
    /// 镜像引用。对应 HotSpot 每个 `Klass` 持有单一 `_java_mirror`(Class 对象)。保证
    /// `Foo.class == Foo.class`、`obj.getClass() == Foo.class` 等 Class 对象身份相等。
    /// Mutex(B.2.3b 共享态)。
    class_mirrors: Mutex<HashMap<String, Reference>>,
    /// Class 镜像反查表(4.12):镜像引用 → 所表示类型的内部名。供 Class native
    /// (`getSuperclass`/`isInstance`/`isAssignableFrom`/`initClassName`…)由镜像反查类。
    /// 镜像现为真 `java/lang/Class` Instance,Instance 本身不记所表示的类 → 须此表。
    /// Mutex(B.2.3b 共享态)。
    mirror_class: Mutex<HashMap<Reference, String>>,
    /// 命名 Module 镜像表(4.14a):模块名(`java.base`)→ 真 `java/lang/Module` Instance 引用。
    /// 同名模块恒同引用(对应 HotSpot 每个 `Module` 类实例单例)。`name` 字段填模块名;
    /// 无名模块走 [`Vm::unnamed_module`](单例,`name` 字段 null)。Mutex(B.2.3b 共享态)。
    module_mirrors: Mutex<HashMap<String, Reference>>,
    /// 无名模块单例引用(惰性分配,4.14a)。`Module.getName()` 返 null → `isNamed()`=false。
    /// Mutex(B.2.3b 共享态)。
    unnamed_module: Mutex<Option<Reference>>,
    /// VM 生命周期阶段(Phase V)。`bootstrap` Created→Bootstrapping→Running;`shutdown`→ShuttingDown。
    phase: Mutex<VmPhase>,
}

impl Vm {
    /// 构造共享态(空堆、空池、空表;tid 起始 1)。`registry` = `Some` 经 [`Vm::new`],
    /// `None` 经 [`Vm::default`](无注册表纯数值测试)。B.3.0:`registry` 为 owned `Arc`(非借用)。
    fn new(registry: Option<Arc<ClassRegistry>>) -> Self {
        Self {
            heap: Mutex::new(Heap::new()),
            registry,
            string_pool: Mutex::new(StringPool::new()),
            monitors: Mutex::new(HashMap::new()),
            threads: threads::ThreadManager::new(),
            exception_meta: Mutex::new(HashMap::new()),
            class_mirrors: Mutex::new(HashMap::new()),
            mirror_class: Mutex::new(HashMap::new()),
            module_mirrors: Mutex::new(HashMap::new()),
            unnamed_module: Mutex::new(None),
            phase: Mutex::new(VmPhase::Created),
        }
    }
}

/// 执行上下文:拥有对象堆,借用类注册表,跟踪帧嵌套深度。
///
/// Phase B.2.3a:共享字段归入 [`shared`](Self.shared)([`Runtime`]),线程隔离态
///([`thread`](Self.thread))留本结构。B.2.3b:`shared: Arc<Runtime>`,每线程经
/// [`Vm::from_shared`](`Vm::from_shared(Arc::clone(&vm.shared))`) 派生各自 Vm、共享同一
/// `Arc<Runtime>`(字段全 Mutex → `Runtime: Send + Sync` → `Arc<Runtime>: Send + Sync`)。
/// 执行上下文:拥有对象堆,共享类注册表,跟踪帧嵌套深度。
///
/// Phase B.2.3a:共享字段归入 [`shared`](Self.shared)([`Runtime`]),线程隔离态
///([`thread`](Self.thread))留本结构。B.2.3b:`shared: Arc<Runtime>`,每线程经
/// [`Vm::from_shared`](`Vm::from_shared(Arc::clone(&vm.shared))`) 派生各自 Vm、共享同一
/// `Arc<Runtime>`(字段全 Mutex → `Runtime: Send + Sync` → `Arc<Runtime>: Send + Sync`)。
/// **B.3.0**:无 `'a` lifetime(`registry` 为 owned `Arc<ClassRegistry>`)→ `Runtime: 'static`
/// → `Arc<Runtime>: 'static` → B.3b `thread::spawn(move || …)` 跨线程共享 `Arc::clone`。
pub struct VmThread {
    /// 跨线程共享态(堆/注册表/池/管程/镜像表/线程管理器)。`Arc` 共享;字段全 Mutex(`Arc::clone` 派生线程)。
    runtime: Arc<Vm>,
    /// 当前线程隔离态(调用栈/帧深度/线程镜像)。
    pub(crate) thread: ThreadContext,
}

impl VmThread {
    /// 构造带类注册表的 Vm(空堆,默认深度上限)。**B.3.0**:`registry` 为 owned `Arc<ClassRegistry>`
    ///(`load_orreplace` 须 `&mut`,故注册表先 owned 载入完毕、再 `Arc::new` 包后传入)。
    /// 取 `impl Into<Arc<ClassRegistry>>`:调用方可传 owned `ClassRegistry`(`Arc::new` 由本方法包)
    /// 或既有 `Arc<ClassRegistry>`(B.3b 线程派生 `Arc::clone(&shared_registry)`);`Vm::new(reg)`
    /// → `Vm::new(reg)`(去 `&`,owned 移交)。
    pub fn new(registry: impl Into<Arc<ClassRegistry>>) -> Self {
        Self {
            runtime: Arc::new(Vm::new(Some(registry.into()))),
            thread: ThreadContext::new_main(),
        }
    }

    /// 从既有共享态派生新 Vm(B.3b 真线程:每线程各持 `Arc::clone` 的共享态 + 独立 `ThreadContext`)。
    /// 调用方先 [`Vm::shared_arc`] 取 `Arc::clone(&vm.shared)`,再经本方法构造派生线程的 Vm。
    /// 共享态(堆/池/管程/镜像表)跨线程共享;线程隔离态(调用栈/帧深度/线程镜像)各独立。
    pub(crate) fn from_vm(runtime: Arc<Vm>) -> Self {
        Self {
            runtime,
            thread: ThreadContext::new_main(),
        }
    }

    /// 取共享态的 `Arc::clone`(供 [`Vm::from_shared`] 派生线程 Vm;`shared` 字段私有)。
    /// B.3.0:返 `Arc<Runtime>`(`'static`),B.3b `start_thread` `move` 进 `thread::spawn` 闭包。
    pub(crate) fn vm_arc(&self) -> Arc<Vm> {
        Arc::clone(&self.runtime)
    }

    /// 当前 VM 生命周期阶段(Phase V)。
    #[allow(dead_code)] // 仅 #[cfg(test)] 闸门引用 → 非 test lib 构建视为 dead(V-3/V-4 起常途消费)。
    pub(crate) fn phase(&self) -> VmPhase {
        *self.runtime.phase.lock().unwrap()
    }

    /// **VM 引导入口(Phase V-2,Option B)**:串行驱动 `launch.rs` Phase1/2/3 + `VmPhase` 状态机
    /// (Created→Bootstrapping→Running)。**幂等**:phase 已 `Running`/`ShuttingDown` 时 no-op 返 Ok
    /// (不重跑三步);`Bootstrapping`(同线程重入)→ `Err`。调用方 VmThread 直接跑三步、即为主线程
    /// (phase 经 `self.vm.phase` 跨 `from_vm` 派生线程共享)。
    pub fn bootstrap(&mut self) -> Result<(), VmError> {
        {
            let mut p = self.runtime.phase.lock().unwrap();
            match *p {
                VmPhase::Created => *p = VmPhase::Bootstrapping,
                VmPhase::Running | VmPhase::ShuttingDown => return Ok(()), // 幂等
                VmPhase::Bootstrapping => {
                    return Err(VmError::BadConstant(
                        "bootstrap 重入:phase Bootstrapping(同线程不应重入)",
                    ))
                }
            }
        }
        launch::initialize_system_class(self)?; // Phase 1:savedProps 引导
        launch::bootstrap_module_system(self)?; // Phase 2:模块层 + initLevel(2)
        launch::bootstrap_java_lang_invoke(self)?; // Phase 3 lite:java.lang.invoke
        *self.runtime.phase.lock().unwrap() = VmPhase::Running;
        Ok(())
    }

    /// 设置帧深度上限(builder)。SOE 测试用小值快速触发。
    pub fn with_stack_limit(mut self, limit: u32) -> Self {
        self.thread.stack_limit = limit;
        self
    }

    /// 对象堆(Mutex 守卫;Phase B.2.3b)。inline 调用经 `Deref` 不破;跨语句绑定 须提取 owned
    ///(`.cloned()`)——`MutexGuard` 借 `&self`,持 guard 跨 `&mut vm` 会 E0502。
    pub fn heap(&self) -> MutexGuard<'_, Heap> {
        self.runtime.heap.lock().unwrap()
    }

    /// 对象堆(可变访问经 Mutex 内部可变性;`&self` 即可,调用方 `&mut vm` 自动协变)。
    pub fn heap_mut(&self) -> MutexGuard<'_, Heap> {
        self.runtime.heap.lock().unwrap()
    }

    /// 字符串 intern 池(4.8/4.10i):文本 → 堆引用的纯备忘;真 String 实例构造在
    /// interpreter(`string::intern`),本池仅保证「同文本恒同引用」。
    pub(crate) fn string_pool(&self) -> MutexGuard<'_, StringPool> {
        self.runtime.string_pool.lock().unwrap()
    }

    /// 字符串 intern 池(可变;经 MutexGuard 内部可变性,`&self` 即可,同 `heap_mut`)。
    pub(crate) fn string_pool_mut(&self) -> MutexGuard<'_, StringPool> {
        self.runtime.string_pool.lock().unwrap()
    }

    /// 类注册表(若启用)。**B.3.0**:返 owned `Arc<ClassRegistry>`(cheap refcount clone)。
    /// `Arc` 为独立 local 绑定、不借 `&self` —— 取出 `&LoadedClass`(借 `Arc`)后仍可 `&mut self`
    ///(保 §6 NLL trick:递归 `interpret_with` 等)。`ClassRegistry` 经 deref 透明用(`.get`/…);
    /// `load_or_replace` 须 `&mut`,故 registry 须**建 Vm 前** owned 载入完毕。
    pub fn registry(&self) -> Option<Arc<ClassRegistry>> {
        self.runtime.registry.clone()
    }

    // ---- 栈帧法(T8 下沉 impl ThreadContext;Vm 薄转发,保调用点零改动)----

    /// 入一个 Java 栈帧(转发 [`ThreadContext::push_frame`])。
    pub(crate) fn push_frame(&mut self, class: &str, method: &str) {
        self.thread.push_frame(class, method);
    }

    /// 退一个 Java 栈帧(转发 [`ThreadContext::pop_frame`])。
    pub(crate) fn pop_frame(&mut self) {
        self.thread.pop_frame();
    }

    /// 自栈顶向下第 `depth_from_top` 层帧的声明类内部名(转发 [`ThreadContext::frame_class_at`])。
    pub(crate) fn frame_class_at(&self, depth_from_top: usize) -> Option<&str> {
        self.thread.frame_class_at(depth_from_top)
    }

    /// 刷新栈顶帧 bci(转发 [`ThreadContext::set_top_frame_pc`])。
    pub(crate) fn set_top_frame_pc(&mut self, pc: u32) {
        self.thread.set_top_frame_pc(pc);
    }
}

impl Default for VmThread {
    fn default() -> Self {
        Self {
            runtime: Arc::new(Vm::new(None)),
            thread: ThreadContext::new_main(),
        }
    }
}

#[cfg(test)]
mod monitor_tests {
    //! Layer 4.41 / Phase B.1:`monitorenter/monitorexit` 真重入 + IMSE。
    use super::*;
    use crate::oops::{ClassRegistry, InstanceOop, Oop};
    use crate::runtime::VmError;

    /// 分配一个锁对象(裸 Instance,类名 "Lock")。owner 经 `main_thread` 解析(无 Thread 预载
    /// 时返 null——单线程下 owner 一致即可测重入/释放/IMSE 机制)。
    fn lock_obj(vm: &mut VmThread) -> Reference {
        vm.heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("Lock".into(), vec![])))
    }

    /// **RED→GREEN**(S2):同对象两次 monitorenter(重入 count=2)→ holds_lock=true;一次 exit
    /// (count=1)仍持有;再次 exit(count=0)释放 → holds_lock=false。验证重入计数 + 释放。
    #[test]
    fn monitor_enter_reentry_and_exit_releases() {
        let reg = ClassRegistry::new();
        let mut vm = VmThread::new(reg);
        let obj = lock_obj(&mut vm);
        vm.monitor_enter(obj).expect("enter #1");
        vm.monitor_enter(obj).expect("enter #2 (重入)");
        assert!(vm.holds_lock(obj).unwrap(), "重入后应持有");
        vm.monitor_exit(obj).expect("exit #1");
        assert!(vm.holds_lock(obj).unwrap(), "count>0 仍持有");
        vm.monitor_exit(obj).expect("exit #2 (释放)");
        assert!(!vm.holds_lock(obj).unwrap(), "count=0 应释放");
    }

    /// **RED→GREEN**(S2):monitorexit 一个未持有的对象 → IllegalMonitorStateException
    ///(`monitorexit` 要求当前线程持有;JVMS §6.5 monitorexit)。验证 IMSE 抛出。
    #[test]
    fn monitor_exit_unheld_throws_imse() {
        let reg = ClassRegistry::new();
        let mut vm = VmThread::new(reg);
        let obj = lock_obj(&mut vm);
        let err = vm.monitor_exit(obj).unwrap_err();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(Oop::Instance(i)) = heap.get(r) else {
            panic!("IMSE 应为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/IllegalMonitorStateException");
    }

    /// **RED→GREEN**(S2):monitorenter null → NullPointerException(JVMS §6.5 monitorenter)。
    #[test]
    fn monitor_enter_null_throws_npe() {
        let reg = ClassRegistry::new();
        let mut vm = VmThread::new(reg);
        let err = vm.monitor_enter(Reference::null()).unwrap_err();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(Oop::Instance(i)) = heap.get(r) else {
            panic!("NPE 应为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/NullPointerException");
    }
}

#[cfg(test)]
mod sync_assertions {
    //! Layer 4.42 / Phase B.2.1:`Vm` 须为 `Sync`——B.3 真并发(`Arc<Mutex<Runtime>>:
    //! Send+Sync`)的前置。当前 `Vm` 经 `registry: Option<&'a ClassRegistry>` 借注册表,
    //! 而 `ClassRegistry`/`LoadedClass` 持 `RefCell`(static_storage/flat_cache/init_state/
    //! class_modules),`RefCell: !Sync` → `Vm: !Sync` → 此断言**编译失败**(RED)。把四处
    //! `RefCell` 改 `Mutex` 后 `ClassRegistry: Sync` → `Vm: Sync` → 编译通过(GREEN)。
    //!
    //! Phase B.2.1 续:`Vm: Send` 同理达成(`registry: &'a ClassRegistry: Send` ⟸
    //! `ClassRegistry: Sync`)。Heap→Mutex 的「`&Vm` 共享引用互斥改堆」能力顺延至 B.2.3
    //! (Runtime 拆分):单独包 `Mutex<Heap>` 须把 ~30 处 `vm.heap().get()` match/let-else
    //! 重构为「先提取 owned 再 `&mut vm`」(`MutexGuard` 的 `Drop` 延长 `&self` 借用到作用域末,
    //! 破坏 §6 NLL 即用即释),无 Runtime 视图拆分上下文则成纯机械搅动,故并入 B.2.3。
    //!
    //! Phase B.2.3a(已落):`Runtime` 结构已内联提取(`Vm { shared, thread }`,owned、无 Mutex、
    //! 行为保持)——确立「共享 vs 线程隔离」字段边界,本断言仍绿(`Runtime: Sync` ⟸ 各字段皆 Sync)。
    //! B.2.3b 待做:`shared: &'a Runtime` 视图 + 逐字段 `Mutex` + `let heap = vm.heap();` 绑定修 E0716
    //!(`MutexGuard` 借 Runtime(referent)非 vm → 持 guard 不阻塞 `&mut vm`,E0502 自动消失)。
    use super::VmThread;
    use crate::oops::ClassRegistry;

    fn assert_sync<T: ?Sized + Sync>() {}
    fn assert_send<T: ?Sized + Send>() {}
    fn assert_static<T: 'static>(_: &T) {}

    /// 断言 `vm_arc()` 返 `Arc<Vm>: 'static`(B.3b `thread::spawn(move || Arc::clone(&vt.vm))`
    /// 的前置:spawn 闭包须 `'static`;Vm 无生命周期参数,registry 为 owned `Arc<ClassRegistry>`)。
    #[test]
    fn vm_arc_is_static() {
        let reg = ClassRegistry::new();
        let vm = VmThread::new(reg);
        assert_static(&vm.vm_arc());
    }

    /// `Vm: Sync`(B.2.1):各共享字段全 `Mutex`,registry 为 `Arc<ClassRegistry>`(`ClassRegistry: Sync`)
    /// → `Runtime: Sync` → `Vm: Sync`。B.3.0 移除 `'a` 后 Vm 无生命周期参数,直接断言即可。
    #[test]
    fn vm_is_sync() {
        assert_sync::<VmThread>();
    }

    /// `Vm: Send`(B.2.1):B.3 `Arc<Mutex<Runtime>>: Send+Sync` 须 `Runtime: Send` → `Vm: Send`
    ///(`ClassRegistry: Send+Sync`,B.2.1 已达)。B.3.0 后无生命周期参数,直接断言。
    #[test]
    fn vm_is_send() {
        assert_send::<VmThread>();
    }

    /// 断言 `Arc<Vm>: Send + Sync`(B.3b 跨线程 `Arc::clone` 共享 Vm 的前置:各共享字段全
    /// `Mutex` → `Vm: Send+Sync` → `Arc<Vm>: Send+Sync`)。
    #[test]
    fn arc_vm_is_send_sync() {
        // Vm 无生命周期参数(registry 为 owned Arc<ClassRegistry>)。
        assert_send::<std::sync::Arc<super::Vm>>();
        assert_sync::<std::sync::Arc<super::Vm>>();
    }

    /// `from_vm(vt.vm_arc())` 派生的 VmThread 与原 VmThread **共享同一 `Arc<Vm>`**(堆/池/
    /// 管程/镜像表)。在 vt 堆上分配的对象,经 vm2(from_vm)同引用可见。
    #[test]
    fn from_vm_shares_arc_vm() {
        use crate::oops::{InstanceOop, Oop};
        let reg = ClassRegistry::new();
        let vm = VmThread::new(reg);
        // 在 vm 的共享堆上分配一个对象(无须经注册表/intern)。
        let r = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("probe".into(), vec![])));
        // vm_arc + from_vm 派生共享态 vm2(各自独立 ThreadContext)。
        let vm2 = VmThread::from_vm(vm.vm_arc());
        let heap = vm2.heap();
        let oop = heap
            .get(r)
            .expect("共享堆:vm 分配的对象在 vm2 须可见");
        assert!(
            matches!(oop, Oop::Instance(i) if i.class_name() == "probe"),
            "from_vm 须共享 Vm 堆(同引用同对象)"
        );
    }
}

#[cfg(test)]
mod concurrent_monitor_tests {
    //! Phase B.3a:真阻塞管程闸门。两 OS 线程经 [`Vm::from_shared`](`Arc::clone`)共享同一
    //! [`Runtime`],对同一锁对象 `monitor_enter/exit` 包夹**非原子**读-改-写共享计数。阻塞管程
    //! 串行化临界区 → 总数 == 2N;当前重入不阻塞(owner 不判 / 无 Condvar)→ 两线程同时进入临界区
    //! → 竞态丢失更新 → 总数 < 2N(RED)。GREEN:[`JavaMonitor`] + `Condvar` 阻塞至 owner 空闲。
    use super::*;
    use crate::oops::{ClassRegistry, InstanceOop, Oop};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;

    /// 每线程迭代次数(够大以放大竞态;yield_now 进一步拉宽丢失更新窗口)。
    const ITERS: u64 = 2000;

    /// worker 线程体:派生共享 Vm,ITERS 次 enter → 非原子 RMW → exit。
    fn worker(shared: Arc<Vm>, lock: Reference, counter: &AtomicU64) {
        let mut vm = VmThread::from_vm(shared);
        for _ in 0..ITERS {
            vm.monitor_enter(lock).expect("monitor_enter");
            // 非原子读-改-写:正确性**仅**靠管程串行化保证(yield_now 拉宽竞态窗口)。
            let v = counter.load(Ordering::Relaxed);
            thread::yield_now();
            counter.store(v + 1, Ordering::Relaxed);
            vm.monitor_exit(lock).expect("monitor_exit");
        }
    }

    /// **RED→GREEN**:两线程并发各 ITERS 次自增共享计数,管程须串行化 → 总数 == 2·ITERS。
    #[test]
    fn monitor_serializes_concurrent_increment() {
        let vm = VmThread::new(ClassRegistry::new());
        let shared = vm.vm_arc();
        let lock = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("Lock".into(), vec![])));
        let counter = Arc::new(AtomicU64::new(0));

        let (c1, c2) = (Arc::clone(&counter), Arc::clone(&counter));
        let (s1, s2) = (Arc::clone(&shared), Arc::clone(&shared));
        let t1 = thread::spawn(move || worker(s1, lock, &c1));
        let t2 = thread::spawn(move || worker(s2, lock, &c2));
        t1.join().expect("t1 未 panic");
        t2.join().expect("t2 未 panic");

        assert_eq!(
            counter.load(Ordering::Relaxed),
            2 * ITERS,
            "阻塞管程须串行化并发自增(无丢失更新)"
        );
    }
}

#[cfg(test)]
mod concurrent_wait_tests {
    //! Phase B.3c:`Object.wait/notify/notifyAll` 真阻塞语义闸门。移植 `ObjectSynchronizer::wait`
    //!(synchronizer.cpp:514)+`ObjectMonitor::wait`(objectMonitor.cpp:1732)与 `notify`/`notifyAll`
    //!(2108/2136):`object_wait` 释管程(owner/count 归零、entry.notify_one)→ `wait_cvar.wait_timeout_while`
    //! 阻塞(抗 spurious wakeup:`wake_seq` 谓词)→ 唤醒后重获管程(恢复重入计数);`object_notify[_all]`
    //! 推 `wake_seq` 并 `wait_cvar.notify_one[_all]`。CHECK_OWNER→IMSE、millis<0→IAE、null→NPE、无等待者→no-op。
    use super::*;
    use crate::oops::{ClassRegistry, InstanceOop, Oop};
    use crate::runtime::VmError;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// 分配一个裸 `Lock` Instance 作管程锁对象(`monitor_enter`/`object_wait` 据 `main_thread` 解析 owner)。
    fn lock_obj(vm: &mut VmThread) -> Reference {
        vm.heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("Lock".into(), vec![])))
    }

    /// **RED→GREEN**:wait(millis>0) 须真阻塞约 millis。RED:旧 4.13 no-op wait 立返 → elapsed < 75ms。
    /// GREEN:真 `wait_cvar.wait_timeout_while(150ms)` 阻塞满超时(`wake_seq` 谓词抗 spurious 唤醒)。
    #[test]
    fn object_wait_blocks_for_timeout() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let lock = lock_obj(&mut vm);
        vm.monitor_enter(lock).expect("monitor_enter");
        let start = Instant::now();
        vm.object_wait(lock, 150).expect("wait(150) 须 owner==本线程");
        let elapsed = start.elapsed();
        vm.monitor_exit(lock).expect("monitor_exit");
        assert!(
            elapsed >= Duration::from_millis(75),
            "wait(150) 须阻塞 ~150ms,实际 {elapsed:?}(no-op wait 立返 < 75ms)"
        );
    }

    /// notifier 循环 notify(每 10ms)直到 waiter 报完成——保证 waiter 一旦进入 wait 即被唤醒(无丢信号)。
    /// GREEN:waiter 在 < 2s 内被唤醒;notify 失效 → waiter 等 5000ms 超时 → elapsed > 2s。
    #[test]
    fn object_notify_wakes_waiting_thread() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let lock = lock_obj(&mut vm);
        let shared = vm.vm_arc();
        let done = Arc::new(AtomicU64::new(0));

        let (s_wait, d_wait) = (Arc::clone(&shared), Arc::clone(&done));
        let waiter = thread::spawn(move || {
            let mut vm = VmThread::from_vm(s_wait);
            vm.monitor_enter(lock).expect("waiter enter");
            vm.object_wait(lock, 5000).expect("waiter wait");
            vm.monitor_exit(lock).expect("waiter exit");
            d_wait.store(1, Ordering::SeqCst);
        });
        // 给 waiter 进入 wait 一点时间(释管程、阻塞于 wait_cvar)。
        thread::sleep(Duration::from_millis(100));
        let (s_not, d_not) = (Arc::clone(&shared), Arc::clone(&done));
        let notifier = thread::spawn(move || {
            let mut vm = VmThread::from_vm(s_not);
            while d_not.load(Ordering::SeqCst) == 0 {
                vm.monitor_enter(lock).expect("notifier enter");
                vm.object_notify(lock).expect("notifier notify");
                vm.monitor_exit(lock).expect("notifier exit");
                thread::sleep(Duration::from_millis(10));
            }
        });
        let start = Instant::now();
        waiter.join().expect("waiter 未 panic");
        notifier.join().expect("notifier 未 panic");
        let elapsed = start.elapsed();
        assert_eq!(done.load(Ordering::SeqCst), 1, "waiter 须被唤醒并报完成");
        assert!(
            elapsed < Duration::from_secs(2),
            "waiter 须被 notify 唤醒(<2s),实际 {elapsed:?}(notify 失效→等满 5s 超时)"
        );
    }

    /// notifyAll 唤醒**全部**等待者。两 waiter,notifier 循环 notify_all 直到两 waiter 都报完成。
    #[test]
    fn object_notify_all_wakes_all_waiters() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let lock = lock_obj(&mut vm);
        let shared = vm.vm_arc();
        let done = Arc::new(AtomicU64::new(0));

        let spawn_waiter = |shared: Arc<Vm>, done: Arc<AtomicU64>| {
            thread::spawn(move || {
                let mut vm = VmThread::from_vm(shared);
                vm.monitor_enter(lock).expect("waiter enter");
                vm.object_wait(lock, 5000).expect("waiter wait");
                vm.monitor_exit(lock).expect("waiter exit");
                done.fetch_add(1, Ordering::SeqCst);
            })
        };
        let w1 = spawn_waiter(Arc::clone(&shared), Arc::clone(&done));
        thread::sleep(Duration::from_millis(50));
        let w2 = spawn_waiter(Arc::clone(&shared), Arc::clone(&done));
        thread::sleep(Duration::from_millis(100));
        let (s_not, d_not) = (Arc::clone(&shared), Arc::clone(&done));
        let notifier = thread::spawn(move || {
            let mut vm = VmThread::from_vm(s_not);
            while d_not.load(Ordering::SeqCst) < 2 {
                vm.monitor_enter(lock).expect("notifier enter");
                vm.object_notify_all(lock).expect("notifier notifyAll");
                vm.monitor_exit(lock).expect("notifier exit");
                thread::sleep(Duration::from_millis(10));
            }
        });
        let start = Instant::now();
        w1.join().expect("w1 未 panic");
        w2.join().expect("w2 未 panic");
        notifier.join().expect("notifier 未 panic");
        let elapsed = start.elapsed();
        assert_eq!(done.load(Ordering::SeqCst), 2, "notifyAll 须唤醒两 waiter");
        assert!(
            elapsed < Duration::from_secs(2),
            "两 waiter 须被 notifyAll 唤醒(<2s),实际 {elapsed:?}"
        );
    }

    /// 未持有管程调 wait → IllegalMonitorStateException(`ObjectSynchronizer::wait` CHECK_OWNER)。
    #[test]
    fn object_wait_without_monitor_throws_imse() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let lock = lock_obj(&mut vm);
        let err = vm.object_wait(lock, 0).unwrap_err();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(Oop::Instance(i)) = heap.get(r) else {
            panic!("IMSE 应为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/IllegalMonitorStateException");
    }

    /// 未持有管程调 notify → IllegalMonitorStateException(`ObjectSynchronizer::notify` CHECK_OWNER)。
    #[test]
    fn object_notify_without_monitor_throws_imse() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let lock = lock_obj(&mut vm);
        let err = vm.object_notify(lock).unwrap_err();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(Oop::Instance(i)) = heap.get(r) else {
            panic!("IMSE 应为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/IllegalMonitorStateException");
    }

    /// wait(null) → NullPointerException(jvm.cpp `JVM_MonitorWait`:handle==nullptr)。
    #[test]
    fn object_wait_null_throws_npe() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let err = vm.object_wait(Reference::null(), 0).unwrap_err();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(Oop::Instance(i)) = heap.get(r) else {
            panic!("NPE 应为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/NullPointerException");
    }

    /// wait(负 timeout) → IllegalArgumentException(`ObjectSynchronizer::wait`:516 millis<0)。
    #[test]
    fn object_wait_negative_timeout_throws_iae() {
        let mut vm = VmThread::new(ClassRegistry::new());
        let lock = lock_obj(&mut vm);
        vm.monitor_enter(lock).unwrap();
        let err = vm.object_wait(lock, -1).unwrap_err();
        vm.monitor_exit(lock).unwrap();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let heap = vm.heap();
        let Some(Oop::Instance(i)) = heap.get(r) else {
            panic!("IAE 应为异常实例");
        };
        assert_eq!(i.class_name(), "java/lang/IllegalArgumentException");
    }
}
