//! 异常元数据 + 栈轨迹(Phase B.2.3b T7 从 [`super::vm`] 分解)。
//!
//! `Throwable` 三要素(捕获帧 / cause / detailMessage)的 rustj 侧并行镜像,键 = 异常对象句柄;
//! 对应共享态 `VmShared.exception_meta`。`format_trace` 渲染诊断文本;`frame_source`/`exception_frames`
//! 供 `StackTraceElement` native 构造 STE。`ExceptionMeta` 为 `pub(super)`:`VmShared`(`super::vm`)
//! 持 `HashMap<Reference, ExceptionMeta>` 须命名类型。

use crate::oops::Oop;
use crate::runtime::{Reference, Vm};

use super::CallFrame;

/// 异常的元数据(`Throwable` 三要素的 rustj 侧镜像):捕获帧 / cause / detailMessage。
///
/// 键 = 异常对象句柄(`Reference` 单调不复用)。`format_trace` 据此渲染
/// `Class: message\n\tat …\nCaused by: …`。真 `Throwable.getMessage/getCause/getStackTrace`
/// 字段回填是更大的独立层;当前先以此并行结构服务诊断输出。
#[derive(Default, Clone)]
pub(super) struct ExceptionMeta {
    /// 抛出点快照的调用链(`Throwable.fillInStackTrace`)。空 = 未捕获。
    pub(super) frames: Vec<CallFrame>,
    /// 包裹 cause(`Throwable.cause` / `new X(cause)`)。
    pub(super) cause: Option<Reference>,
    /// detailMessage(`Throwable.detailMessage`,如 "/ by zero")。
    pub(super) message: Option<String>,
}

impl Vm {
    /// 在抛出点快照当前调用链,绑定到异常句柄(此刻 `call_stack` 满)。
    /// 等价 HotSpot `Throwable.fillInStackTrace` 捕获语义——stub 异常不经真 `<init>`,
    /// 故 `throw_exception` 直接调之;`fillInStackTrace` native 亦调之(为真 Throwable 预留)。
    pub(crate) fn record_trace(&mut self, exc: Reference) {
        let frames = self.thread.call_stack.clone();
        let mut meta = self.shared.exception_meta.lock().unwrap();
        meta.entry(exc).or_default().frames = frames;
    }

    /// 登记包裹异常的 cause(对应 `new ExceptionInInitializerError(cause)` 设 `Throwable.cause`)。
    /// `format_trace` 据此追链渲染 "Caused by:"——被包异常**自身**的轨迹携带真正抛出点
    /// (如 clinit 内部位置),从而顶层不再丢失根因。
    pub(crate) fn record_cause(&mut self, wrapper: Reference, cause: Reference) {
        let mut meta = self.shared.exception_meta.lock().unwrap();
        meta.entry(wrapper).or_default().cause = Some(cause);
    }

    /// 登记异常的 detailMessage(对应 `Throwable.detailMessage`,如 "/ by zero")。
    /// `format_trace` 据此在头类后渲染 ": <message>"。供 JVM 自动抛出点带上诊断消息。
    pub(crate) fn record_message(&mut self, exc: Reference, message: impl Into<String>) {
        let msg = message.into();
        let mut meta = self.shared.exception_meta.lock().unwrap();
        meta.entry(exc).or_default().message = Some(msg);
    }

    /// 解析一帧的源文件名 + 行号(`(file, line)`),供 [`Self::format_trace`] 与
    /// `StackTraceElement.initStackTraceElements` 构造 STE。经注册表查声明类 → 同名方法且
    /// `pc` 落在 `code` 长度内(重载按 pc 范围消歧)→ 其 `LineNumberTable` 取最大
    /// `start_pc ≤ pc` 的 `line_number`;配合 `SourceFile` 文件名。文件名与行号**须同时**
    /// 可得(对齐 HotSpot:无文件则不印行);否则 `None`。镜像 `Method::line_number_from_bci`。
    pub(crate) fn frame_source(&self, f: &CallFrame) -> Option<(String, u16)> {
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
            (Some(f_name), Some(line)) => Some((f_name.to_string(), line)),
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
    /// 返 owned `Vec`(exception_meta 已 Mutex 化;无法返借用切片——B.2.3b)。
    pub(crate) fn exception_frames(&self, exc: Reference) -> Option<Vec<CallFrame>> {
        let meta = self.shared.exception_meta.lock().unwrap();
        meta.get(&exc).map(|m| m.frames.clone())
    }

    /// 格式化异常的栈轨迹文本:`ExcClass[: message]\n\tat Class.method(File.java:LINE)`、
    /// **最内(抛出)帧在前**(Java 惯例)。随后沿 cause 链每跳输出
    /// `\nCaused by: <cause 类>[: message]` + cause 自身帧。深度上限 64(防环/失控链)。
    /// 无快照且无 cause/message → 空串(旧契约)。供测试/诊断;顶层未捕获时自动打印。
    pub fn format_trace(&self, exc: Reference) -> String {
        // 沿 cause 链渲染:每跳单次锁 exception_meta 取 owned(frames/message/cause),释 guard 再
        // 读 heap 取类名(持 guard 重锁 exception_meta / 与 heap 锁序冲突均规避;B.2.3b)。
        // **始终打印头异常类名**:native 分派抛出的异常(如 UnsatisfiedLinkError)无 frames/message,
        // 但类名本身对调试关键(曾因此红测得空轨迹)。
        let mut out = String::new();
        let mut cur = Some(exc);
        let mut head = true;
        let mut depth = 0u32;
        while let Some(e) = cur {
            if depth >= 64 {
                break;
            }
            depth += 1;
            let class = match self.shared.heap.lock().unwrap().get(e) {
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
            // 每跳单次锁 exception_meta 取 owned(frames/message/cause),释 guard 再渲染。
            let (frames, message, cause) = {
                let meta = self.shared.exception_meta.lock().unwrap();
                match meta.get(&e) {
                    Some(m) => (m.frames.clone(), m.message.clone(), m.cause),
                    None => (Vec::new(), None, None),
                }
            };
            if let Some(msg) = &message {
                out.push_str(": ");
                out.push_str(msg);
            }
            // call_stack 入栈序 = 外层→内层(抛出帧在最末);Java 惯例最内帧首 → 逆序打印。
            for f in frames.iter().rev() {
                out.push_str("\n\tat ");
                out.push_str(&f.class);
                out.push('.');
                out.push_str(&f.method);
                let loc = self.frame_location_suffix(f);
                if !loc.is_empty() {
                    out.push_str(&loc);
                }
            }
            cur = cause;
        }
        out
    }
}
