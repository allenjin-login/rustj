# Layer 2: 字节码定义 + 栈帧模型 (Rust 迁移设计)

> 对应 HotSpot `interpreter/bytecodes.hpp`(操作码表)、`runtime/frame.*` / `runtime/stackValue*`(栈帧)。

## 目标

为执行引擎打地基:
1. **全部标准 Java 操作码**的枚举(0–202)+ 助记符 + 指令格式/长度。
2. **栈帧模型**:局部变量表、操作数栈、Frame。

- 零 unsafe(沿用 crate 级 `#![deny(unsafe_code)]`)。
- 模块化:`bytecode/` 与 `runtime/` 各自独立、可单测。
- 不实现分派循环(那是 Layer 3),只定义数据与抽象,以及栈/局部变量的存取语义。

## 非目标

字节码分派/解释执行(Layer 3)、对象堆(Layer 4)、类链接/校验。

## 模块结构

```
src/
  bytecode/
    mod.rs
    opcode.rs        // Opcode 枚举 + from_u8 + name + format/length
  runtime/
    mod.rs
    slot.rs          // Slot 枚举 + Reference(不透明句柄)
    operand_stack.rs // OperandStack
    local_vars.rs    // LocalVars
    frame.rs         // Frame = locals + operand stack + pc
```

## Opcode 设计

- `#[repr(u8)] enum Opcode`,变体名用助记符大写(`Iadd`、`Iload`、`Goto` …),判别值即 JVMS 操作码字节。
- `Opcode::from_u8(u8) -> Result<Opcode, BytecodeError>`:覆盖标准 0–201,`breakpoint`(202)单列;未知字节返回错误(254/255 等保留码亦视为未知)。
- `Opcode::name(&self) -> &'static str`:小写助记符(`iadd`)。
- `Opcode::format(&self) -> Format`:操作数布局;`Format::length()` 给出**固定**指令长度(含操作码字节);`tableswitch`/`lookupswitch`/`wide` 为 `Variable`。

```rust
pub enum Format {
    None,        // 长度 1
    U1,          // 长度 2(bipush, ldc, newarray)
    U2,          // 长度 3(*load #idx, getstatic, …)
    Branch,      // s2,长度 3(if*/goto/jsr)
    BranchWide,  // s4,长度 5(goto_w/jsr_w)
    Iinc,        // u1 var, s1 delta,长度 3
    Multianewarray, // u2 + u1,长度 4
    Variable,    // tableswitch / lookupswitch / wide
}
```

## 栈帧模型

### `Slot`(JVMS §2.6.1/§2.6.2)

long/double 为 category-2,占**两个连续槽位**:第一个持有完整值(`Long`/`Double`),第二个为 `Top` 占位。

```rust
pub enum Slot {
    Int(i32), Float(f32), Long(i64), Double(f64),
    Reference(Reference), ReturnAddress(u16), Top,
}
```

### `Reference`

不透明对象句柄,`None` 表 null。解耦于堆(Layer 4 赋予真实含义):

```rust
pub struct Reference(Option<u32>);
impl Reference {
    pub const fn null() -> Self;
    pub fn from_id(id: u32) -> Self;
    pub fn is_null(self) -> bool;
    pub fn id(self) -> Option<u32>;
}
```

### `OperandStack` / `LocalVars`

- 固定容量(`max_stack` / `max_locals`,单位:槽位)。
- 类型化存取:`push_int`/`pop_int`/`push_long`/`pop_long` …;`long`/`double` 进出两个槽位。
- 越界、溢出/下溢、类型不符 → `FrameError`,全程 `Result`,不 panic。

### `Frame`

```rust
pub struct Frame { pub locals: LocalVars, pub operands: OperandStack, pub pc: u16 }
```

`pc` 为当前指令偏移。方法体(`CodeAttribute`)与常量池由 Layer 3 解释器与 Frame 配对持有,本层保持解耦。

## 错误类型

新增 `runtime::FrameError`:`Overflow`、`Underflow`、`TypeMismatch`、`BadLocalIndex`、`BadPc`。
新增 `bytecode::BytecodeError`:`UnknownOpcode(u8)`。

## 测试策略

- Opcode:`from_u8` 全表正确性、名称、格式/长度;未知码报错。
- OperandStack:int/long 进出、long 占两槽、溢出/下溢/类型不符。
- LocalVars:long 跨两槽读写、越界报错。
- Frame:构造与默认 pc=0。

## 构建顺序

opcode → slot → operand_stack → local_vars → frame。
