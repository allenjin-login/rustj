//! 执行上下文:对象堆 + 类注册表 + 帧深度计数。对应 HotSpot `JavaThread`
//! 执行所需的共享状态 + 栈深度检查。
//!
//! 4.1:对象/字段/`invokestatic` 路径需注册表([`Vm::new`])。运行时异常(NPE/算术
//! 异常等)统一为 `ThrownException`、须在堆上分配异常对象——故即便纯数值字节码也可能
//! 需要注册表(便捷入口 `interpret()` 自带注册表);[`Vm::default`] 仅空堆 + 无注册表,
//! 供确不抛异常的纯数值测试。4.2b:帧深度计数 + 可配置上限([`Vm::with_stack_limit`]);
//! 超限时解释器抛 `java/lang/StackOverflowError`(统一为 `ThrownException`)。

use std::collections::HashMap;

use crate::oops::ClassRegistry;
use crate::runtime::heap::Heap;
use crate::runtime::string_pool::StringPool;
use crate::runtime::Reference;

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
    pub(crate) frame_depth: u32,
    /// 帧深度上限;`frame_depth >= stack_limit` 时再调用 → 抛 `StackOverflowError`。
    pub(crate) stack_limit: u32,
    /// 当前活动 Java 调用栈(逐帧 push/pop),供栈轨迹捕获。
    call_stack: Vec<CallFrame>,
    /// 异常 → 元数据(帧 / cause / detailMessage),键 = 异常对象句柄。
    exception_meta: HashMap<Reference, ExceptionMeta>,
}

impl<'a> Vm<'a> {
    /// 构造带类注册表的 Vm(空堆,默认深度上限)。
    pub fn new(registry: &'a ClassRegistry) -> Self {
        Self {
            heap: Heap::new(),
            registry: Some(registry),
            string_pool: StringPool::new(),
            frame_depth: 0,
            stack_limit: DEFAULT_STACK_LIMIT,
            call_stack: Vec::new(),
            exception_meta: HashMap::new(),
        }
    }

    /// 设置帧深度上限(builder)。SOE 测试用小值快速触发。
    pub fn with_stack_limit(mut self, limit: u32) -> Self {
        self.stack_limit = limit;
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

    /// 刷新**栈顶**帧的 bci(`run()` 分派前调,记当前指令起始)。抛出时即抛点 bci;
    /// 调用者陷入被调用者后,其顶帧 pc 冻结于 invoke 点(其 run loop 挂起前最后写入)。
    /// 栈为空(匿名纯算术帧)时无操作。
    pub(crate) fn set_top_frame_pc(&mut self, pc: u32) {
        if let Some(top) = self.call_stack.last_mut() {
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
            .frames = self.call_stack.clone();
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
        use crate::oops::Oop;
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
            frame_depth: 0,
            stack_limit: DEFAULT_STACK_LIMIT,
            call_stack: Vec::new(),
            exception_meta: HashMap::new(),
        }
    }
}
