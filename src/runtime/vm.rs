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
//! 本模块留核心结构([`Vm`]/[`VmShared`]/[`ThreadContext`]/[`CallFrame`]/[`MonitorState`])、
//! 构造、堆/池/注册表 accessor、栈帧法(T8 下沉 [`ThreadContext`])。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::oops::ClassRegistry;
use crate::runtime::heap::Heap;
use crate::runtime::string_pool::StringPool;
use crate::runtime::Reference;

mod exceptions;
mod mirrors;
mod monitors;
mod threads;

/// 默认帧深度上限。高于 ackermann(3,3) 的递归深度(~120),正常小测试不会误触;
/// 可经 [`Vm::with_stack_limit`] 调整(SOE 测试用小值快速触发)。
pub const DEFAULT_STACK_LIMIT: u32 = 512;

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
/// "当前线程"的 ThreadContext;Phase B.3 真并发后每 OS 线程一个,经 `Arc<Mutex<VmShared>>` 共享。
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
}

/// 对象管程状态(对应 HotSpot 对象头 mark word 的锁态子集;Layer 4.41 / Phase B.1)。
///
/// `owner` = 持有者线程的 Thread 镜像句柄;`count` = 重入计数(同线程多次 monitorenter
/// 累加,monitorexit 减,归零释放)。单线程下 owner 恒为当前线程;B.3 真并发后多线程争用。
#[derive(Clone, Copy)]
pub(crate) struct MonitorState {
    pub(crate) owner: Reference,
    pub(crate) count: u32,
}

/// **跨线程共享态**(Phase B.2.3a/b):Vm 持有的「所有线程共享」字段集合——对象堆、类注册表、
/// 字符串池、管程表、异常元数据、Class/Module 镜像表、线程管理器。逐字段 `Mutex` 包装,
/// `Vm.shared` 持 `Arc<VmShared>`——多线程经 `Vm::from_shared(Arc::clone(&vm.shared))` 派生
/// 各自 Vm、共享并发改写。对应 HotSpot 跨 `JavaThread` 共享的全局结构(`JavaHeap`/
/// `SystemDictionary`/`StringTable`/`ObjectMonitor` 表等);线程隔离态留 [`Vm::thread`]。
/// `pub(crate)`:`from_shared` 签名须命名。
pub(crate) struct VmShared<'a> {
    /// 对象堆(Mutex:Phase B.2.3b 共享态——`Arc<VmShared>` 多线程并发改堆的前置)。
    heap: Mutex<Heap>,
    registry: Option<&'a ClassRegistry>,
    /// 字符串 intern 池(4.8):文本 → 堆引用,以本 Vm 的堆为后盾。Mutex(B.2.3b 共享态)。
    string_pool: Mutex<StringPool>,
    /// 对象管程(对象句柄 → 锁状态)。跨线程共享态(B.2.3b 加 Mutex);单线程下 owner 恒为当前线程。
    pub(crate) monitors: Mutex<HashMap<Reference, MonitorState>>,
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
}

impl<'a> VmShared<'a> {
    /// 构造共享态(空堆、空池、空表;tid 起始 1)。`registry` = `Some` 经 [`Vm::new`],
    /// `None` 经 [`Vm::default`](无注册表纯数值测试)。
    fn new(registry: Option<&'a ClassRegistry>) -> Self {
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
        }
    }
}

/// 执行上下文:拥有对象堆,借用类注册表,跟踪帧嵌套深度。
///
/// Phase B.2.3a:共享字段归入 [`shared`](Self.shared)([`VmShared`]),线程隔离态
///([`thread`](Self.thread))留本结构。B.2.3b:`shared: Arc<VmShared>`,每线程经
/// [`Vm::from_shared`](`Vm::from_shared(Arc::clone(&vm.shared))`) 派生各自 Vm、共享同一
/// `Arc<VmShared>`(字段全 Mutex → `VmShared: Send + Sync` → `Arc<VmShared>: Send + Sync`)。
pub struct Vm<'a> {
    /// 跨线程共享态(堆/注册表/池/管程/镜像表/线程管理器)。`Arc` 共享;字段全 Mutex(`Arc::clone` 派生线程)。
    shared: Arc<VmShared<'a>>,
    /// 当前线程隔离态(调用栈/帧深度/线程镜像)。
    pub(crate) thread: ThreadContext,
}

impl<'a> Vm<'a> {
    /// 构造带类注册表的 Vm(空堆,默认深度上限)。
    pub fn new(registry: &'a ClassRegistry) -> Self {
        Self {
            shared: Arc::new(VmShared::new(Some(registry))),
            thread: ThreadContext::new_main(),
        }
    }

    /// 从既有共享态派生新 Vm(B.3b 真线程:每线程各持 `Arc::clone` 的共享态 + 独立 `ThreadContext`)。
    /// 调用方先 [`Vm::shared_arc`] 取 `Arc::clone(&vm.shared)`,再经本方法构造派生线程的 Vm。
    /// 共享态(堆/池/管程/镜像表)跨线程共享;线程隔离态(调用栈/帧深度/线程镜像)各独立。
    #[allow(dead_code)] // B.3b 真线程派生用;T6 引入,测试 + B.3b 调用
    pub(crate) fn from_shared(shared: Arc<VmShared<'a>>) -> Self {
        Self {
            shared,
            thread: ThreadContext::new_main(),
        }
    }

    /// 取共享态的 `Arc::clone`(供 [`Vm::from_shared`] 派生线程 Vm;`shared` 字段私有)。
    #[allow(dead_code)] // B.3b 真线程派生用;T6 引入,测试 + B.3b 调用
    pub(crate) fn shared_arc(&self) -> Arc<VmShared<'a>> {
        Arc::clone(&self.shared)
    }

    /// 设置帧深度上限(builder)。SOE 测试用小值快速触发。
    pub fn with_stack_limit(mut self, limit: u32) -> Self {
        self.thread.stack_limit = limit;
        self
    }

    /// 对象堆(Mutex 守卫;Phase B.2.3b)。inline 调用经 `Deref` 不破;跨语句绑定 须提取 owned
    ///(`.cloned()`)——`MutexGuard` 借 `&self`,持 guard 跨 `&mut vm` 会 E0502。
    pub fn heap(&self) -> MutexGuard<'_, Heap> {
        self.shared.heap.lock().unwrap()
    }

    /// 对象堆(可变访问经 Mutex 内部可变性;`&self` 即可,调用方 `&mut vm` 自动协变)。
    pub fn heap_mut(&self) -> MutexGuard<'_, Heap> {
        self.shared.heap.lock().unwrap()
    }

    /// 字符串 intern 池(4.8/4.10i):文本 → 堆引用的纯备忘;真 String 实例构造在
    /// interpreter(`string::intern`),本池仅保证「同文本恒同引用」。
    pub(crate) fn string_pool(&self) -> MutexGuard<'_, StringPool> {
        self.shared.string_pool.lock().unwrap()
    }

    /// 字符串 intern 池(可变;经 MutexGuard 内部可变性,`&self` 即可,同 `heap_mut`)。
    pub(crate) fn string_pool_mut(&self) -> MutexGuard<'_, StringPool> {
        self.shared.string_pool.lock().unwrap()
    }

    /// 类注册表(若启用)。
    ///
    /// 返回的引用与注册表本身同寿命(`'a`),不依赖本次对 `self` 的借用——
    /// 这样取出 `&'a LoadedClass` 后仍可再借 `&mut self`(如递归 `interpret_with`)。
    pub fn registry(&self) -> Option<&'a ClassRegistry> {
        self.shared.registry
    }

    // ---- 栈帧法(T8 下沉 impl ThreadContext;当前 impl Vm 薄持有)----

    /// 入一个 Java 栈帧(类内部名 + 方法名)。`interpret_with` 入口与 `native::invoke`
    /// 入口各推一帧。克隆入 owned [`CallFrame`](各来源生命周期不一)。`pc` 初始 0,
    /// 由 [`Self::set_top_frame_pc`] 在 `run()` 分派前持续刷新。
    pub(crate) fn push_frame(&mut self, class: &str, method: &str) {
        self.thread.call_stack.push(CallFrame {
            class: class.to_string(),
            method: method.to_string(),
            pc: 0,
        });
    }

    /// 退一个 Java 栈帧(与 `push_frame` 配对;`interpret_with`/`native::invoke` 出口调)。
    pub(crate) fn pop_frame(&mut self) {
        self.thread.call_stack.pop();
    }

    /// 自栈顶(最新帧)向下第 `depth_from_top` 层帧的声明类内部名(0 = 栈顶)。
    ///
    /// 供 `Reflection.getCallerClass`(@CallerSensitive 基础设施)等栈帧回溯 native 用。
    /// 栈深不足(无对应层)→ `None`。`native::invoke` 已为本 native 推入自身帧(即栈顶),
    /// 故 `depth_from_top=2` = "调用 getCallerClass 的方法"的**调用者**。
    pub(crate) fn frame_class_at(&self, depth_from_top: usize) -> Option<&str> {
        let n = self.thread.call_stack.len();
        n.checked_sub(1)
            .and_then(|last| last.checked_sub(depth_from_top))
            .and_then(|i| self.thread.call_stack.get(i))
            .map(|f| f.class.as_str())
    }

    /// 刷新**栈顶**帧的 bci(`run()` 分派前调,记当前指令起始)。抛出时即抛点 bci;
    /// 调用者陷入被调用者后,其顶帧 pc 冻结于 invoke 点(其 run loop 挂起前最后写入)。
    /// 栈为空(匿名纯算术帧)时无操作。
    pub(crate) fn set_top_frame_pc(&mut self, pc: u32) {
        if let Some(top) = self.thread.call_stack.last_mut() {
            top.pc = pc;
        }
    }
}

impl Default for Vm<'_> {
    fn default() -> Self {
        Self {
            shared: Arc::new(VmShared::new(None)),
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
    fn lock_obj(vm: &mut Vm<'_>) -> Reference {
        vm.heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("Lock".into(), vec![])))
    }

    /// **RED→GREEN**(S2):同对象两次 monitorenter(重入 count=2)→ holds_lock=true;一次 exit
    /// (count=1)仍持有;再次 exit(count=0)释放 → holds_lock=false。验证重入计数 + 释放。
    #[test]
    fn monitor_enter_reentry_and_exit_releases() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
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
        let mut vm = Vm::new(&reg);
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
        let mut vm = Vm::new(&reg);
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
    //! Layer 4.42 / Phase B.2.1:`Vm` 须为 `Sync`——B.3 真并发(`Arc<Mutex<VmShared>>:
    //! Send+Sync`)的前置。当前 `Vm<'a>` 经 `registry: Option<&'a ClassRegistry>` 借注册表,
    //! 而 `ClassRegistry`/`LoadedClass` 持 `RefCell`(static_storage/flat_cache/init_state/
    //! class_modules),`RefCell: !Sync` → `Vm: !Sync` → 此断言**编译失败**(RED)。把四处
    //! `RefCell` 改 `Mutex` 后 `ClassRegistry: Sync` → `Vm: Sync` → 编译通过(GREEN)。
    //!
    //! Phase B.2.1 续:`Vm: Send` 同理达成(`registry: &'a ClassRegistry: Send` ⟸
    //! `ClassRegistry: Sync`)。Heap→Mutex 的「`&Vm` 共享引用互斥改堆」能力顺延至 B.2.3
    //! (VmShared 拆分):单独包 `Mutex<Heap>` 须把 ~30 处 `vm.heap().get()` match/let-else
    //! 重构为「先提取 owned 再 `&mut vm`」(`MutexGuard` 的 `Drop` 延长 `&self` 借用到作用域末,
    //! 破坏 §6 NLL 即用即释),无 VmShared 视图拆分上下文则成纯机械搅动,故并入 B.2.3。
    //!
    //! Phase B.2.3a(已落):`VmShared` 结构已内联提取(`Vm { shared, thread }`,owned、无 Mutex、
    //! 行为保持)——确立「共享 vs 线程隔离」字段边界,本断言仍绿(`VmShared: Sync` ⟸ 各字段皆 Sync)。
    //! B.2.3b 待做:`shared: &'a VmShared` 视图 + 逐字段 `Mutex` + `let heap = vm.heap();` 绑定修 E0716
    //!(`MutexGuard` 借 VmShared(referent)非 vm → 持 guard 不阻塞 `&mut vm`,E0502 自动消失)。
    use super::Vm;
    use crate::oops::ClassRegistry;

    fn assert_sync<T: ?Sized + Sync>() {}
    fn assert_send<T: ?Sized + Send>() {}

    /// `Vm<'a>: Sync` 蕴含 `&'a ClassRegistry: Sync` → `ClassRegistry: Sync`。
    #[test]
    fn vm_is_sync() {
        fn check<'a>(_: &'a ClassRegistry) {
            assert_sync::<Vm<'a>>();
        }
        let _ = check;
    }

    /// `Vm<'a>: Send`(B.2.1):B.3 `Arc<Mutex<VmShared>>: Send+Sync` 须 `VmShared: Send`,
    /// 进而 `Vm: Send`(`registry: &'a ClassRegistry: Send` ⟸ `ClassRegistry: Sync`,B.2.1 已达)。
    #[test]
    fn vm_is_send() {
        fn check<'a>(_: &'a ClassRegistry) {
            assert_send::<Vm<'a>>();
        }
        let _ = check;
    }

    /// **T6**(B.2.3b):`Arc<VmShared<'a>>: Send + Sync`——B.3b `thread::spawn` 跨线程共享 `Arc::clone`
    /// 的前置。各共享字段全 `Mutex`(registry 仍 `&'a` 不可变)→ `VmShared: Send+Sync` →
    /// `Arc<VmShared>: Send+Sync`。RED(任一字段非 Send/Sync)→ 编译失败。
    #[test]
    fn arc_vmshared_is_send_sync() {
        fn check<'a>(_: &'a ClassRegistry) {
            assert_send::<std::sync::Arc<super::VmShared<'a>>>();
            assert_sync::<std::sync::Arc<super::VmShared<'a>>>();
        }
        let _ = check;
    }

    /// **T6**(B.2.3b):`from_shared(vm.shared_arc())` 派生的 Vm 与原 Vm **共享同一 `Arc<VmShared>`**
    /// (堆/池/管程/镜像表)。在 vm 堆上分配的对象,经 vm2(from_shared)同引用可见。
    #[test]
    fn from_shared_shares_arc_vmshared() {
        use crate::oops::{InstanceOop, Oop};
        let reg = ClassRegistry::new();
        let vm = Vm::new(&reg);
        // 在 vm 的共享堆上分配一个对象(无须经注册表/intern)。
        let r = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("probe".into(), vec![])));
        // shared_arc + from_shared 派生共享态 vm2(各自独立 ThreadContext)。
        let vm2 = Vm::from_shared(vm.shared_arc());
        let heap = vm2.heap();
        let oop = heap
            .get(r)
            .expect("共享堆:vm 分配的对象在 vm2 须可见");
        assert!(
            matches!(oop, Oop::Instance(i) if i.class_name() == "probe"),
            "from_shared 须共享 VmShared 堆(同引用同对象)"
        );
    }
}
