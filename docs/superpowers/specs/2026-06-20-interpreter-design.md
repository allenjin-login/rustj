# Layer 3 — 字节码解释器设计(rustj)

- 日期:2026-06-20
- 对应 HotSpot 源:`src/hotspot/share/interpreter/zero/bytecodeInterpreter.{cpp,hpp}`、`cpu/zero/bytecodeInterpreter_zero.hpp`
- 上游:`[[hotspot-rust-migration-project]]` 的第 3 层;依赖 Layer 1(类文件/常量池/元数据)与 Layer 2(`Opcode`、`Frame`/`OperandStack`/`LocalVars`/`Slot`)。

## 1. 目标

真正**执行** JVM 字节码。把 HotSpot Zero 解释器(纯 C++、可移植、最适合移植)的分派循环搬到**安全 Rust**(零 unsafe)。

完成判据:对一个用 `javac` 编译的真实 Java 类中的 `static int` 方法,解释器能从字节码算出与 JVM 完全一致的结果(含整数溢出回绕、除零语义)。

## 2. 分层路线(用户已确认:先 int 子集,逐档补全,最终必须覆盖全数值 + 方法调用)

| 增量 | 范围 | 完成判据 |
|------|------|----------|
| **3.1 int 核心子集** | int 常量/加载/存储/算术/位运算/栈操作/分支/`ireturn`+`return`,单栈帧 | 跑通真实 `static int` 方法(add / 阶乘 / fib 迭代) |
| **3.2 全数值** | long/float/double 的加载/存储/算术/类型转换/比较 | 跑通含 long/double 的数值方法 |
| **3.3 方法调用** | `invokestatic`/`invokespecial`、跨栈帧 `*return`、简易帧管理器(调用栈) | 跑通多方法互调程序 |

本文档主述 **3.1**;3.2/3.3 在各自增量时细化,沿用同一分派循环与错误模型。

## 3. 架构

### 3.1 模块

新增 `runtime::interpreter`,目录式模块,预留 3.2/3.3 扩展:

```
src/runtime/interpreter/
  mod.rs   // Interpreter、interpret()、VmError、Value、操作数读取
```
(随 3.2/3.3 增大再拆 `ops_int.rs` / `ops_fp.rs` / `invoke.rs`。)

`runtime/mod.rs` 追加 `pub mod interpreter;`。`lib.rs` 不变。

### 3.2 核心类型

```rust
/// 解释器执行结果值。3.1 只用 Int / Void;3.2 起补 Long/Float/Double/Reference。
pub enum Value {
    Int(i32),
    Void,
    // 3.2+: Long(i64), Float(f32), Double(f64), Reference(Reference),
}

/// 运行时错误(JVM 语义层面)。
pub enum VmError {
    /// ArithmeticException:int/long 除零。
    DivideByZero,
    /// 当前子集不支持的指令(随增量推进而收敛)。
    UnsupportedOpcode(Opcode),
    /// PC 越过字节码末尾仍未返回。
    BadPc(usize),
    /// 操作数/局部变量栈帧错误(由 FrameError 映射而来)。
    Frame(FrameError),
    /// 常量池索引或类型不符。
    ConstantPool(ClassFileError),
    /// ldc 取到非 int 常量等。
    BadConstant(&'static str),
}

/// 解释器:持有字节码与常量池的不可变借用,在给定栈帧上执行。
pub struct Interpreter<'a> {
    code: &'a [u8],
    cp: &'a ConstantPool,
}
```

`VmError` 实现 `Display`/`Error`。`From<FrameError>`、`From<ClassFileError>`、`From<BytecodeError>` 提供映射,`?` 即可在循环内传播(`Opcode::from_u8` 返回 `BytecodeError`)。

### 3.3 入口

```rust
impl<'a> Interpreter<'a> {
    pub fn new(code: &'a [u8], cp: &'a ConstantPool) -> Self;
    /// 在 frame 上执行至 *return;返回结果值。
    pub fn interpret(&self, frame: &mut Frame) -> Result<Value, VmError>;
}
```

调用方(Layer 3.3 的帧管理器,或集成测试)负责构造 `Frame::new(max_locals, max_stack)` 并按方法签名初始化实参到局部变量 0..n,再调 `interpret`。

## 4. 分派循环(安全,无指针算术)

HotSpot 用 `while(1){ opcode=*pc; switch(opcode){ CASE(_iadd):... } }` + 计算跳转标签优化。Rust 用 `match Opcode`:

```rust
let mut pc: usize = 0;
loop {
    let op = Opcode::from_u8(code[pc])?;          // pc 已在循环内先校验
    match op {
        Opcode::Iadd => {
            let r = frame.operands.pop_int()?;
            let l = frame.operands.pop_int()?;
            frame.operands.push_int(l.wrapping_add(r))?;
            pc += 1;
        }
        Opcode::Idiv => {
            let r = frame.operands.pop_int()?;
            let l = frame.operands.pop_int()?;
            if r == 0 { return Err(VmError::DivideByZero); }
            frame.operands.push_int(l.wrapping_div(r))?;   // MIN/-1 自动回绕
            pc += 1;
        }
        Opcode::Iload => {
            let idx = self.read_u1(pc + 1)? as u16;
            frame.operands.push_int(frame.locals.get_int(idx)?)?;
            pc += 2;
        }
        Opcode::Ifeq => {
            let v = frame.operands.pop_int()?;
            let off = self.read_s2(pc + 1)? as i64;
            pc = if v == 0 { branch(pc, off) } else { pc + 3 };
        }
        Opcode::Ireturn => {
            return Ok(Value::Int(frame.operands.pop_int()?));
        }
        Opcode::Return => return Ok(Value::Void),
        other => return Err(VmError::UnsupportedOpcode(other)),
    }
    // 循环末:pc 越界检查(防止跑飞),详见 §5。
}
```

**与 HotSpot 的关键差异(皆为安全/语义正确性)**

| 点 | HotSpot (C++) | rustj |
|----|---------------|-------|
| 分派 | 计算跳转标签表 | `match Opcode`(安全;computed goto 在 Rust 需 unsafe) |
| 操作数读取 | `pc[1]`、`Bytes::get_Java_u2(pc+1)` 原始指针 | 切片索引 + `from_be_bytes`,每次带 `pc+n <= code.len()` 越界检查 |
| 栈/局部变量 | `STACK_INT(-2)` 等裸指针宏 | Layer 2 已有的类型化 `pop_int/push_int/get_int/set_int`,带容量与类型检查 |
| 整数溢出 | 平台溢出(未定义行为被 JVM 规范约束为回绕) | 显式 `wrapping_*`(Java 语义:补码回绕) |
| PC | `address` 指针 | `usize` 字节偏移;跳转用饱和算术,§5 |

### 4.1 操作数读取辅助

字节码本就是大端(JVMS §4.10.2),复用大端约定。在 `Interpreter` 上加越界检查的小函数:

```rust
fn read_u1(&self, at: usize) -> Result<u8, VmError>     // at 须 < len
fn read_u2(&self, at: usize) -> Result<u16, VmError>    // at, at+1
fn read_s1(&self, at: usize) -> Result<i8, VmError>
fn read_s2(&self, at: usize) -> Result<i16, VmError>    // 分支偏移
```

越界 → `VmError::BadPc(at)`。

### 4.2 分支跳转

`goto`/`if*` 的操作数是**相对当前指令起点**的有符号 16 位偏移(JVMS §6.5 goto)。目标 = `pc + offset`。为防 `usize` 下溢与上溢:

```rust
fn branch(pc: usize, offset: i64) -> Result<usize, VmError> {
    let target = pc as i64 + offset;
    if target < 0 { return Err(VmError::BadPc(pc)); }
    Ok(target as usize)   // 上界由循环末越界检查兜底
}
```

`goto_w`/`jsr_w`(s4)在 3.1 不出现,3.x 再加。

## 5. PC 安全性

- 进循环前若 `code` 为空 → `BadPc`。
- 取 `Opcode::from_u8(code[pc])` 前,若 `pc >= code.len()` → `BadPc(pc)`(指令跑到末尾仍未 `*return`)。
- 每个分支目标在**下一次循环取指时**统一校验,不在跳转处重复。
- javac 合法字节码不会越界;校验为防御与清晰错误。

## 6. 整数语义(JVMS §6.5,与 HotSpot 一致)

- `iadd/isub/imul`:`wrapping_add/sub/mul`。
- `idiv`:`r==0` → `DivideByZero`;否则 `wrapping_div`(`MIN/-1` 回绕为 `MIN`,无异常——与 Java 一致)。
- `irem`:Java `%` 向零截断;`MIN % -1 == 0`(无除零);`r==0` → `DivideByZero`。
- `ineg`:`wrapping_neg`。
- 移位:`ishl` 移 `v & 0x1f`;`ishr` 算术右移(`>>` on i32);`iushr` 逻辑右移(`((v as u32) >> s) as i32`)。
- 位运算:`iand/ior/ixor` 直接。

## 7. 3.1 指令清单(实现次序 = TDD 批次)

1. **常量压栈**:`iconst_m1`、`iconst_0..5`、`bipush`、`sipush`、`ldc`(仅 Integer)。
2. **加载/存储**:`iload`、`iload_0..3`、`istore`、`istore_0..3`。
3. **算术/位运算**:`iadd`、`isub`、`imul`、`idiv`、`irem`、`ineg`、`iand`、`ior`、`ixor`、`ishl`、`ishr`、`iushr`。
4. **栈操作**:`dup`、`pop`。
5. **分支**:`ifeq/ifne/iflt/ifge/ifgt/ifle`、`if_icmpeq/if_icmpne/if_icmplt/if_icmpge/if_icmpgt/if_icmple`、`goto`。
6. **返回**:`ireturn`、`return`。

清单外的指令统一 `VmError::UnsupportedOpcode`(随 3.2/3.3 收敛)。

## 8. 测试策略(TDD:写测试看红 → 实现看绿)

- **单元(每组)**:手搓 `code: &[u8]` + `Frame::new(...)`,断言栈/返回值/PC 行为;含越界、除零、`MIN/-1`、回绕等边界。
- **集成(执行真实字节码的闸门)**:`tests/interpret_int_methods.rs`:用 `javac` 编译一个小 Java 类(纯 `static int` 方法:`add(int,int)`、`factorial(int)` 迭代、`fib(int)` 迭代),用 Layer 1 解析,对每个方法 `resolve_code` + 取 `code`,构造栈帧并把实参写入局部变量 0..,`interpret` 后断言与 JVM(`java` 或人工核验)一致。
- 每条指令先有失败测试再实现;批次内保持全绿后再扩批次。

## 9. 不做(YAGNI,留给后续增量)

- `long/float/double` 指令(3.2)、`invoke*`/`new`/`*aload`/`*astore`/`athrow`/异常表(3.3 及之后)。
- 字节码校验器(Verifier):信任 `javac` 产物;解释器只做越界/类型运行时检查。
- 计算跳转/模板解释器等性能优化。

## 10. 风险与对策

| 风险 | 对策 |
|------|------|
| 签名在 3.2 需扩 `Value` | 现在就定义 `Value` 枚举(3.1 仅 Int/Void),后续加变体不破坏调用方 |
| 分支 `usize` 下溢 | `branch()` 用 `i64` 中间量 + 符号判定 |
| 整数语义细微偏差(irem 截断方向、移位掩码) | 每个 opcode 一条专门单测锁定;对照 JVMS |
| 操作数读取越界导致 panic | 全部走 `read_u1/u2/s1/s2` 返回 `VmError`,循环内 `?` 传播,无 unwrap/索引裸用 |
