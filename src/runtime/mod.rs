//! 运行时栈帧模型。对应 HotSpot `runtime/frame.*` / `runtime/stackValue*`。
//!
//! 本层定义数据与存取语义;字节码分派循环(执行)见 [`interpreter`]。

pub mod class_loader;
pub mod frame;
pub mod heap;
pub mod interpreter;
pub mod local_vars;
pub mod operand_stack;
pub mod slot;
pub mod string_pool;
pub mod vm;

pub use frame::{Frame, FrameError};
pub use heap::Heap;
pub use interpreter::{Interpreter, Value, VmError};
pub use local_vars::LocalVars;
pub use operand_stack::OperandStack;
pub use slot::{Reference, Slot};
pub use string_pool::StringPool;
pub use vm::{DEFAULT_STACK_LIMIT, Vm};
