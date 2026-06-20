//! JVM 字节码操作码定义。对应 HotSpot `interpreter/bytecodes.hpp` 的标准 Java 操作码集。
//!
//! 只覆盖 JVMS 标准操作码(0–202);HotSpot 内部 `fast_*` / `_nofast_*` 等
//! 优化操作码不属于 class 文件格式,本层不纳入(它们是运行时改写产物)。

pub mod opcode;

pub use opcode::{BytecodeError, Format, Opcode};
