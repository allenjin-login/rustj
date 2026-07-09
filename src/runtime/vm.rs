//! 执行上下文:对象堆 + 类注册表 + 帧深度计数。对应 HotSpot `JavaThread`
//! 执行所需的共享状态 + 栈深度检查。
//!
//! 4.1:对象/字段/`invokestatic` 路径需注册表([`Vm::new`])。运行时异常(NPE/算术
//! 异常等)统一为 `ThrownException`、须在堆上分配异常对象——故即便纯数值字节码也可能
//! 需要注册表(便捷入口 `interpret()` 自带注册表);[`Vm::default`] 仅空堆 + 无注册表,
//! 供确不抛异常的纯数值测试。4.2b:帧深度计数 + 可配置上限([`Vm::with_stack_limit`]);
//! 超限时解释器抛 `java/lang/StackOverflowError`(统一为 `ThrownException`)。

use std::collections::HashMap;

use crate::oops::{ClassRegistry, Oop};
use crate::runtime::heap::Heap;
use crate::runtime::string_pool::StringPool;
use crate::runtime::{Reference, Slot, VmError};

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

/// 异常的元数据(`Throwable` 三要素的 rustj 侧镜像):捕获帧 / cause / detailMessage。
///
/// 键 = 异常对象句柄(`Reference` 单调不复用)。`format_trace` 据此渲染
/// `Class: message\n\tat …\nCaused by: …`。真 `Throwable.getMessage/getCause/getStackTrace`
/// 字段回填是更大的独立层;当前先以此并行结构服务诊断输出。
#[derive(Default, Clone)]
struct ExceptionMeta {
    /// 抛出点快照的调用链(`Throwable.fillInStackTrace`)。空 = 未捕获。
    frames: Vec<CallFrame>,
    /// 包裹 cause(`Throwable.cause` / `new X(cause)`)。
    cause: Option<Reference>,
    /// detailMessage(`Throwable.detailMessage`,如 "/ by zero")。
    message: Option<String>,
}

/// 执行上下文:拥有对象堆,借用类注册表,跟踪帧嵌套深度。
pub struct Vm<'a> {
    heap: Heap,
    registry: Option<&'a ClassRegistry>,
    /// 字符串 intern 池(4.8):文本 → 堆引用,以本 Vm 的堆为后盾。
    string_pool: StringPool,
    /// 当前嵌套帧数(进入一帧 +1,退出 −1)。
    pub(crate) thread: ThreadContext,
    /// 对象管程(对象句柄 → 锁状态)。跨线程共享态(B.2 加 Mutex);单线程下 owner 恒为当前线程。
    pub(crate) monitors: HashMap<Reference, MonitorState>,
    /// 下一线程 tid(Thread.tid 递增;main 线程=1)。
    next_tid: u64,
    /// 异常 → 元数据(帧 / cause / detailMessage),键 = 异常对象句柄。
    exception_meta: HashMap<Reference, ExceptionMeta>,
    /// Class 镜像 intern 表(4.10t):内部类名(`java/lang/Foo`、`int`、`[I` …)→ 唯一 Class
    /// 镜像引用。对应 HotSpot 每个 `Klass` 持有单一 `_java_mirror`(Class 对象)。保证
    /// `Foo.class == Foo.class`、`obj.getClass() == Foo.class` 等 Class 对象身份相等。
    class_mirrors: HashMap<String, Reference>,
    /// Class 镜像反查表(4.12):镜像引用 → 所表示类型的内部名。供 Class native
    /// (`getSuperclass`/`isInstance`/`isAssignableFrom`/`initClassName`…)由镜像反查类。
    /// 镜像现为真 `java/lang/Class` Instance,Instance 本身不记所表示的类 → 须此表。
    mirror_class: HashMap<Reference, String>,
    /// 命名 Module 镜像表(4.14a):模块名(`java.base`)→ 真 `java/lang/Module` Instance 引用。
    /// 同名模块恒同引用(对应 HotSpot 每个 `Module` 类实例单例)。`name` 字段填模块名;
    /// 无名模块走 [`Self::unnamed_module`](单例,`name` 字段 null)。
    module_mirrors: HashMap<String, Reference>,
    /// 无名模块单例引用(惰性分配,4.14a)。`Module.getName()` 返 null → `isNamed()`=false。
    unnamed_module: Option<Reference>,
}

impl<'a> Vm<'a> {
    /// 构造带类注册表的 Vm(空堆,默认深度上限)。
    pub fn new(registry: &'a ClassRegistry) -> Self {
        Self {
            heap: Heap::new(),
            registry: Some(registry),
            string_pool: StringPool::new(),
            thread: ThreadContext::new_main(),
            monitors: HashMap::new(),
            next_tid: 1,
            exception_meta: HashMap::new(),
            class_mirrors: HashMap::new(),
            mirror_class: HashMap::new(),
            module_mirrors: HashMap::new(),
            unnamed_module: None,
        }
    }

    /// 设置帧深度上限(builder)。SOE 测试用小值快速触发。
    pub fn with_stack_limit(mut self, limit: u32) -> Self {
        self.thread.stack_limit = limit;
        self
    }

    /// 对象堆。
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// 对象堆(可变)。
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// 字符串 intern 池(4.8/4.10i):文本 → 堆引用的纯备忘;真 String 实例构造在
    /// interpreter(`string::intern`),本池仅保证「同文本恒同引用」。
    pub(crate) fn string_pool(&self) -> &StringPool {
        &self.string_pool
    }

    /// 字符串 intern 池(可变)。
    pub(crate) fn string_pool_mut(&mut self) -> &mut StringPool {
        &mut self.string_pool
    }

    /// Class 镜像 intern(4.10t 起;4.12 退役 `Oop::Class`):同一内部类名恒返回同一 Class
    /// 镜像引用(对应 HotSpot 每 `Klass` 的单一 `_java_mirror`)。镜像现为**真 `java/lang/Class`
    /// Instance**——首次 `new_instance` 分配,置 VM 字段(`componentType`/`primitive`),并登记
    /// 反查表 `mirror_class`;后续命中直接返。使 `Foo.class == Foo.class`、
    /// `obj.getClass() == Foo.class` 等 Class 身份相等成立。
    /// `name`/`classLoader` 字段保持默认 null(`classLoader`=null 即 Bootstrap)。
    /// `module` 由 [`Self::populate_class_mirror_fields`](4.14a)按类所属模块填。
    /// `name` 由 `getName` 真字节码首次调用时经 `initClassName` 懒填。
    pub(crate) fn intern_class_mirror(&mut self, name: &str) -> Reference {
        if let Some(r) = self.class_mirrors.get(name) {
            return *r;
        }
        // 分配真 java/lang/Class Instance(须已加载:引导 Class 桩或经闭包预载的真 Class)。
        let r = self.alloc_class_mirror_instance();
        // 先缓存再填字段:数组组件互递归([LC→C、[[I→[I)经缓存命中终止。
        self.class_mirrors.insert(name.to_string(), r);
        self.mirror_class.insert(r, name.to_string());
        self.populate_class_mirror_fields(r, name);
        r
    }

    /// 镜像所表示类型的内部名(供 Class native 反查)。非镜像引用 → `None`。
    pub(crate) fn mirror_internal_name(&self, r: Reference) -> Option<&str> {
        self.mirror_class.get(&r).map(String::as_str)
    }

    /// 分配一个默认初始化的 `java/lang/Class` Instance。无注册表或 `java/lang/Class` 未加载
    /// (非真实运行场景)→ 返 null 兜底(调用方多为 native,返 null 镜像不致 panic)。
    fn alloc_class_mirror_instance(&mut self) -> Reference {
        let Some(reg) = self.registry() else {
            return Reference::null();
        };
        let Some(class_lc) = reg.get("java/lang/Class") else {
            return Reference::null();
        };
        let inst = reg.new_instance(class_lc);
        self.heap.alloc(Oop::Instance(inst))
    }

    /// 置 VM 管理的 Class 实例字段:`componentType`(数组→组件镜像)、`primitive`(原语→true)、
    /// `module`(4.14a:按类所属命名模块填 Module 镜像,未标记→无名模块)。字段经名查序号;
    /// `java/lang/Class` 未见该字段(桩精简)→ 静默跳过。
    fn populate_class_mirror_fields(&mut self, mirror: Reference, internal: &str) {
        if let Some(comp) = component_internal_of(internal) {
            let comp_mirror = self.intern_class_mirror(&comp);
            self.set_class_instance_field(mirror, "componentType", Slot::Reference(comp_mirror));
        }
        if is_primitive_keyword(internal) {
            self.set_class_instance_field(mirror, "primitive", Slot::Int(1));
        }
        // Class.module = 所属模块的 Module 镜像(命名模块按类→模块表;否则无名模块)。
        // 对应 Class.java:1011 `private transient Module module;`,getModule() 仅 `return module`。
        let module = self.module_for_class(internal);
        self.set_class_instance_field(mirror, "module", Slot::Reference(module));
    }

    /// 按**字段名**(忽略描述符)在 `java/lang/Class` 扁平实例字段中查序号并写槽。
    pub(crate) fn set_class_instance_field(&mut self, mirror: Reference, field_name: &str, slot: Slot) {
        self.set_instance_field_by_name(mirror, "java/lang/Class", field_name, slot);
    }

    /// 按**字段名**在指定声明类的扁平实例字段中查序号并写槽。类未加载或无此字段 → 静默跳过
    /// (供 Class 镜像字段 + Module 镜像 `name` 字段等 VM 管理实例共用)。
    fn set_instance_field_by_name(
        &mut self,
        obj: Reference,
        declaring_class: &str,
        field_name: &str,
        slot: Slot,
    ) {
        let Some(reg) = self.registry() else { return };
        let Some(lc) = reg.get(declaring_class) else { return };
        let Some(ord) = reg
            .flattened_instance_fields(lc)
            .iter()
            .position(|f| f.name == field_name)
        else {
            return;
        };
        if let Some(Oop::Instance(i)) = self.heap_mut().get_mut(obj) {
            i.set_field(ord, slot);
        }
    }

    /// 分配一个默认初始化的 `java/lang/Module` Instance(须已闭包预载)。无注册表或 Module
    /// 未加载 → 返 null 兜底。**不跑 `<init>`**(named/unnamed 两构造器分别调 defineModule0/
    /// 仅置字段;rustj 直接置 `name` 字段,绕过 native 注册)。
    fn alloc_module_instance(&mut self) -> Reference {
        let Some(reg) = self.registry() else {
            return Reference::null();
        };
        let Some(lc) = reg.get("java/lang/Module") else {
            return Reference::null();
        };
        let inst = reg.new_instance(lc);
        self.heap.alloc(Oop::Instance(inst))
    }

    /// 命名 Module 镜像(intern:同名恒同引用)。分配真 `java/lang/Module` Instance,置 `name`
    /// 字段 = intern(模块名)。对应 HotSpot 每个 `Module` 单例(JVM 侧 `java_lang_Module`)。
    /// `Module.getName()` 真字节码读 `name` 字段即得模块名;`isNamed()` = `name != null`。
    fn intern_named_module(&mut self, name: &str) -> Reference {
        if let Some(&r) = self.module_mirrors.get(name) {
            return r;
        }
        let r = self.alloc_module_instance();
        if r.is_null() {
            return r;
        }
        self.module_mirrors.insert(name.to_string(), r);
        // 置 Module.name = intern(模块名)(真 String 实例,供 getName/equals 用)。
        if let Ok(name_ref) = crate::runtime::interpreter::string::intern(self, name) {
            self.set_instance_field_by_name(r, "java/lang/Module", "name", Slot::Reference(name_ref));
        }
        r
    }

    /// 无名模块单例(惰性)。`Module(loader)` 未名构造器语义:`name`=null(默认)、`descriptor`=null。
    /// `getName()` 返 null、`isNamed()`=false。用户类(非模块源)经 [`Self::module_for_class`] 归此。
    fn unnamed_module(&mut self) -> Reference {
        if let Some(r) = self.unnamed_module {
            return r;
        }
        let r = self.alloc_module_instance();
        if !r.is_null() {
            self.unnamed_module = Some(r);
        }
        r
    }

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
    /// (§6 `'a` 不绑 `&self`),出块后 `heap_mut` 分配,再 `set_instance_field_by_name` 置字段。
    fn alloc_main_thread(&mut self) -> Reference {
        let inst = {
            let Some(reg) = self.registry else {
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

    /// 类内部名 → 所属模块的 Module 镜像(供 Class.module 字段填充):
    /// (1) `class_module` 命中 → 命名模块镜(load_closure 据「源容器模块」标记);
    /// (2) 数组(`[...`)→ 组件类的模块(递归剥维);
    /// (3) 未标记(用户类 / 原语 / 默认包)→ 无名模块。
    fn module_for_class(&mut self, internal: &str) -> Reference {
        if let Some(m) = self.registry().and_then(|r| r.class_module(internal)) {
            return self.intern_named_module(&m);
        }
        if let Some(comp) = component_internal_of(internal) {
            return self.module_for_class(&comp);
        }
        self.unnamed_module()
    }

    /// 类注册表(若启用)。
    ///
    /// 返回的引用与注册表本身同寿命(`'a`),不依赖本次对 `self` 的借用——
    /// 这样取出 `&'a LoadedClass` 后仍可再借 `&mut self`(如递归 `interpret_with`)。
    pub fn registry(&self) -> Option<&'a ClassRegistry> {
        self.registry
    }

    // ---- 栈轨迹捕获(4.10j+) ----

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

    /// 在抛出点快照当前调用链,绑定到异常句柄(此刻 `call_stack` 满)。
    /// 等价 HotSpot `Throwable.fillInStackTrace` 捕获语义——stub 异常不经真 `<init>`,
    /// 故 `throw_exception` 直接调之;`fillInStackTrace` native 亦调之(为真 Throwable 预留)。
    pub(crate) fn record_trace(&mut self, exc: Reference) {
        self.exception_meta
            .entry(exc)
            .or_default()
            .frames = self.thread.call_stack.clone();
    }

    /// 登记包裹异常的 cause(对应 `new ExceptionInInitializerError(cause)` 设 `Throwable.cause`)。
    /// `format_trace` 据此追链渲染 "Caused by:"——被包异常**自身**的轨迹携带真正抛出点
    /// (如 clinit 内部位置),从而顶层不再丢失根因。
    pub(crate) fn record_cause(&mut self, wrapper: Reference, cause: Reference) {
        self.exception_meta.entry(wrapper).or_default().cause = Some(cause);
    }

    /// 登记异常的 detailMessage(对应 `Throwable.detailMessage`,如 "/ by zero")。
    /// `format_trace` 据此在头类后渲染 ": <message>"。供 JVM 自动抛出点带上诊断消息。
    pub(crate) fn record_message(&mut self, exc: Reference, message: impl Into<String>) {
        self.exception_meta
            .entry(exc)
            .or_default()
            .message = Some(message.into());
    }

    /// 解析一帧的源文件名 + 行号(`(file, line)`),供 [`Self::format_trace`] 与
    /// `StackTraceElement.initStackTraceElements` 构造 STE。经注册表查声明类 → 同名方法且
    /// `pc` 落在 `code` 长度内(重载按 pc 范围消歧)→ 其 `LineNumberTable` 取最大
    /// `start_pc ≤ pc` 的 `line_number`;配合 `SourceFile` 文件名。文件名与行号**须同时**
    /// 可得(对齐 HotSpot:无文件则不印行);否则 `None`。镜像 `Method::line_number_from_bci`。
    pub(crate) fn frame_source(&self, f: &CallFrame) -> Option<(&str, u16)> {
        use crate::classfile::attributes::LineNumberEntry;
        use crate::constant_pool::ConstantPoolEntry;
        let reg = self.registry()?;
        let lc = reg.get(&f.class)?;
        let file = lc.cf.source_file_name();
        let pc = f.pc as usize;
        // 取同名且 pc 在 code 长度内的方法,解析最大 start_pc ≤ pc 的行号。
        let mut best: Option<(u16, u16)> = None; // (start_pc, line_number)
        for m in &lc.cf.methods {
            let Ok(ConstantPoolEntry::Utf8(name)) = lc.cf.constant_pool.get(m.name_index) else {
                continue;
            };
            if name.as_str() != f.method {
                continue;
            }
            let Some(code) = &m.code else {
                continue;
            };
            if pc >= code.code.len() {
                continue;
            }
            for &LineNumberEntry { start_pc, line_number } in &code.line_number_table {
                if start_pc as usize <= pc
                    && best.is_none_or(|(b_start, _)| start_pc >= b_start)
                {
                    best = Some((start_pc, line_number));
                }
            }
            if best.is_some() {
                break; // 首个匹配(含 pc)的方法即用
            }
        }
        match (file, best.map(|(_, line)| line)) {
            (Some(f_name), Some(line)) => Some((f_name, line)),
            _ => None,
        }
    }

    /// 渲染一帧的源位置后缀(`(File.java:LINE)`);无文件/无行号 → 空串(裸 `at Class.method`)。
    fn frame_location_suffix(&self, f: &CallFrame) -> String {
        match self.frame_source(f) {
            Some((file, line)) => format!("({file}:{line})"),
            None => String::new(),
        }
    }

    /// 取异常捕获的调用链快照(`Throwable.fillInStackTrace` / `throw_exception` 捕获)。
    /// 供 `Throwable.getStackTrace` native 构造 `StackTraceElement[]`。键 = 异常句柄。
    pub(crate) fn exception_frames(&self, exc: Reference) -> Option<&[CallFrame]> {
        self.exception_meta.get(&exc).map(|m| m.frames.as_slice())
    }


    /// 格式化异常的栈轨迹文本:`ExcClass[: message]\n\tat Class.method(File.java:LINE)`、
    /// **最内(抛出)帧在前**(Java 惯例)。随后沿 cause 链每跳输出
    /// `\nCaused by: <cause 类>[: message]` + cause 自身帧。深度上限 64(防环/失控链)。
    /// 无快照且无 cause/message → 空串(旧契约)。供测试/诊断;顶层未捕获时自动打印。
    pub fn format_trace(&self, exc: Reference) -> String {
        let Some(meta) = self.exception_meta.get(&exc) else {
            return String::new();
        };
        // 头异常无帧、无 cause、无 message → 无信息,返空串(旧契约)。
        if meta.frames.is_empty() && meta.cause.is_none() && meta.message.is_none() {
            return String::new();
        }
        let mut out = String::new();
        let mut cur = Some(exc);
        let mut head = true;
        let mut depth = 0u32;
        while let Some(e) = cur {
            if depth >= 64 {
                break;
            }
            depth += 1;
            let class = match self.heap.get(e) {
                Some(Oop::Instance(i)) => i.class_name().to_string(),
                _ => "<unknown>".to_string(),
            };
            if head {
                out.push_str(&class);
                head = false;
            } else {
                out.push_str("\nCaused by: ");
                out.push_str(&class);
            }
            if let Some(m) = self.exception_meta.get(&e)
                && let Some(msg) = &m.message
            {
                out.push_str(": ");
                out.push_str(msg);
            }
            // call_stack 入栈序 = 外层→内层(抛出帧在最末);Java 惯例最内帧首 → 逆序打印。
            if let Some(m) = self.exception_meta.get(&e) {
                for f in m.frames.iter().rev() {
                    out.push_str("\n\tat ");
                    out.push_str(&f.class);
                    out.push('.');
                    out.push_str(&f.method);
                    let loc = self.frame_location_suffix(f);
                    if !loc.is_empty() {
                        out.push_str(&loc);
                    }
                }
            }
            cur = self
                .exception_meta
                .get(&e)
                .and_then(|m| m.cause);
        }
        out
    }
}

impl Default for Vm<'_> {
    fn default() -> Self {
        Self {
            heap: Heap::new(),
            registry: None,
            string_pool: StringPool::new(),
            thread: ThreadContext::new_main(),
            monitors: HashMap::new(),
            next_tid: 1,
            exception_meta: HashMap::new(),
            class_mirrors: HashMap::new(),
            mirror_class: HashMap::new(),
            module_mirrors: HashMap::new(),
            unnamed_module: None,
        }
    }
}

/// 是否为原语关键字(`int`/`void`/…;非内部描述符 `I`)。原语 Class 镜像的 intern 名即关键字。
fn is_primitive_keyword(s: &str) -> bool {
    matches!(
        s,
        "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double" | "void"
    )
}

/// 数组内部名(`[I`/`[Ljava/lang/String;`/`[[I`)的**组件类型内部名**。非数组 → `None`。
/// 组件为原语时返关键字(`int`);为对象类时返内部名(`java/lang/String`);为嵌套数组返 `[I`。
fn component_internal_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix('[')?;
    match rest.chars().next()? {
        'B' => Some("byte".into()),
        'C' => Some("char".into()),
        'D' => Some("double".into()),
        'F' => Some("float".into()),
        'I' => Some("int".into()),
        'J' => Some("long".into()),
        'S' => Some("short".into()),
        'Z' => Some("boolean".into()),
        'L' => Some(rest.strip_prefix('L')?.strip_suffix(';')?.to_string()),
        '[' => Some(rest.to_string()),
        _ => None,
    }
}

// ---- 对象管程(monitorenter/monitorexit/holdsLock;Layer 4.41 / Phase B.1)----
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
        use std::collections::hash_map::Entry;
        match self.monitors.entry(obj) {
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
        let held = self
            .monitors
            .get(&obj)
            .is_some_and(|m| m.owner == owner && m.count > 0);
        if !held {
            return Err(crate::runtime::interpreter::throw_exception(
                self,
                "java/lang/IllegalMonitorStateException",
            ));
        }
        let count = self.monitors.get(&obj).unwrap().count;
        if count == 1 {
            self.monitors.remove(&obj);
        } else {
            self.monitors.get_mut(&obj).unwrap().count -= 1;
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
        Ok(self
            .monitors
            .get(&obj)
            .is_some_and(|m| m.owner == owner && m.count > 0))
    }

    /// 取并递增下一线程 tid(供 Thread 镜像 tid 字段;main 线程取首值 1,后续递增)。
    pub(crate) fn next_thread_tid(&mut self) -> u64 {
        let tid = self.next_tid;
        self.next_tid += 1;
        tid
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
        let Some(Oop::Instance(i)) = vm.heap().get(r) else {
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
        let Some(Oop::Instance(i)) = vm.heap().get(r) else {
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
}

