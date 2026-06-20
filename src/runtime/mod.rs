//! 运行时栈帧模型。对应 HotSpot `runtime/frame.*` / `runtime/stackValue*`。
//!
//! 本层只定义数据与存取语义;字节码分派循环(执行)是 Layer 3。

pub mod frame;
pub mod local_vars;
pub mod operand_stack;
pub mod slot;

pub use frame::{Frame, FrameError};
pub use local_vars::LocalVars;
pub use operand_stack::OperandStack;
pub use slot::{Reference, Slot};
