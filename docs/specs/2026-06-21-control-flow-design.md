# Layer 4.4 控制流补全 设计

> 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_if_acmpeq)` /
> `CASE(_if_acmpne)` / `CASE(_ifnull)` / `CASE(_ifnonnull)` /
> `CASE(_tableswitch)` / `CASE(_lookupswitch)` / `CASE(_goto_w)`。
> 本层补齐**非废弃**的分支与 switch 指令族,完成"控制流"维度。
> `jsr`/`ret`/`jsr_w`(自 Java 6 起不再由 javac 生成)与 `wide`(局部变量 ≥256,
> 罕见)顺延。

## 1. 目标

让 rustj 执行真实 Java 中的引用比较分支(`== null`、`a == b` 引用相等)与
`switch` 语句,与 javac 编译的 `.class` 一致:

- `if_acmpeq`/`if_acmpne`(两引用相等/不等);
- `ifnull`/`ifnonnull`(引用为 null/非 null);
- `tableswitch`(密集 switch,low..high 跳转表);
- `lookupswitch`(稀疏 switch,key→offset 对);
- `goto_w`(4 字节偏移无条件跳转)。

零 unsafe。

## 2. 引用比较分支

`if_acmpeq`/`if_acmpne`/`ifnull`/`ifnonnull` 均为 `Branch` 格式(2 字节有符号偏移,
偏移相对**指令地址**)。

### 2.1 `if_acmpeq`/`if_acmpne`

栈:`..., value1, value2 → ...`。弹两个引用,比较是否**同一引用**
(`Reference` 相等,即同一堆 id 或同为 null;不做对象内容/类型相等)。
`if_acmpeq`:相等跳;`if_acmpne`:不等跳。

### 2.2 `ifnull`/`ifnonnull`

栈:`..., value → ...`。弹一个引用。`ifnull`:为 null 跳;`ifnonnull`:非 null 跳。
null 判定 = `Reference::is_null()`。

### 2.3 实现

沿用既有 `cond1`/`cond2` 的形(弹栈 + 读 s2 偏移 + `branch_target`),新增两个辅助:

```rust
/// 单引用分支:弹 v,`pred(v)` 为真则跳,否则 pc+3。
fn cond_ref1(&self, pc, pred: impl Fn(Reference) -> bool, frame) -> Result<usize, VmError> {
    let v = frame.operands.pop_reference()?;
    let off = self.read_s2(pc + 1)?;
    Ok(if pred(v) { Self::branch_target(pc, off)? } else { pc + 3 })
}

/// 双引用分支:弹 b(顶)、a(底),`pred(a,b)` 为真则跳,否则 pc+3。
fn cond_ref2(&self, pc, pred: impl Fn(Reference, Reference) -> bool, frame) -> Result<usize, VmError> {
    let b = frame.operands.pop_reference()?;
    let a = frame.operands.pop_reference()?;
    let off = self.read_s2(pc + 1)?;
    Ok(if pred(a, b) { Self::branch_target(pc, off)? } else { pc + 3 })
}
```

分派臂:`ifnull`/`ifnonnull` → `cond_ref1`;`if_acmpeq`/`if_acmpne` → `cond_ref2`。

## 3. switch

`tableswitch`/`lookupswitch` 为 `Variable` 格式:操作码后 **0–3 字节填充**使后续
i4 数据从方法代码起算的 **4 字节边界**开始;所有分支偏移为 **i4**(有符号 32 位),
且相对**switch 指令地址**。

### 3.1 填充对齐

opcode 在 `pc`;其后首字节为 `pc+1`。填充字节数 `pad` 使 `pc + 1 + pad` 为 4 的倍数:

```rust
let pad = (4 - ((pc + 1) % 4)) % 4;
```

(边界:`(pc+1) % 4 == 0` ⇒ pad 0;`% 4 == 1` ⇒ pad 3;依此类推。)

### 3.2 `tableswitch`

布局(填充后,均为大端 i4):
```
default(4) | low(4) | high(4) | jump_offsets[(high - low + 1)](4 each)
```
执行:弹 `index`(int)。若 `index < low || index > high` → 跳 `default`;否则跳
`jump_offsets[index - low]`。所有偏移相对 `pc`。

### 3.3 `lookupswitch`

布局(填充后):
```
default(4) | npairs(4) | npairs × (match(4), offset(4))
```
执行:弹 `key`(int)。线性扫描匹配对(校验器保证按 match 升序;为正确性线性即可,
npairs 通常很小),命中则跳其 offset,否则跳 default。偏移相对 `pc`。

> 不做二分查找(YAGNI);记录"可优化为二分"于顺延项。

### 3.4 i4 读取与宽分支目标

新增:

```rust
fn read_s4(&self, at: usize) -> Result<i32, VmError> {
    let b0 = self.read_u1(at)?;
    let b1 = self.read_u1(at + 1)?;
    let b2 = self.read_u1(at + 2)?;
    let b3 = self.read_u1(at + 3)?;
    Ok(i32::from_be_bytes([b0, b1, b2, b3]))
}

/// 宽(i32)分支目标:`pc + offset`,负下溢 → BadPc。
fn branch_target_w(pc: usize, offset: i32) -> Result<usize, VmError> {
    let target = (pc as i64) + (offset as i64);
    if target < 0 { return Err(VmError::BadPc(pc)); }
    Ok(target as usize)
}
```

switch 与 goto_w 共用 `read_s4` 与 `branch_target_w`(switch 偏移 i32、goto_w 偏移 i32)。

## 4. `goto_w`

`BranchWide` 格式:4 字节有符号偏移。执行:读 `pc+1` 的 i4,`branch_target_w(pc, off)`。
与 `goto` 仅偏移宽度不同。

## 5. 分派臂位置

在 `mod.rs` 分派循环的"控制流"块(既有 `If*`/`IfIcmp*`/`Goto` 附近)追加:

```rust
Opcode::IfAcmpeq => pc = self.cond_ref2(pc, |a, b| a == b, frame)?,
Opcode::IfAcmpne => pc = self.cond_ref2(pc, |a, b| a != b, frame)?,
Opcode::Ifnull => pc = self.cond_ref1(pc, |v| v.is_null(), frame)?,
Opcode::Ifnonnull => pc = self.cond_ref1(pc, |v| !v.is_null(), frame)?,
Opcode::Tableswitch => pc = self.table_switch(pc, frame)?,
Opcode::Lookupswitch => pc = self.lookup_switch(pc, frame)?,
Opcode::GotoW => {
    let off = self.read_s4(pc + 1)?;
    pc = Self::branch_target_w(pc, off)?;
}
```

`table_switch`/`lookup_switch` 作为 `Interpreter` 的私有方法(操作 `self.code`/`self.read_*`)。

## 6. 顺延项

- `jsr`/`ret`/`jsr_w`(子例程,现代 javac 不生成);
- `wide`(局部变量 ≥256 的宽索引);
- `lookupswitch` 二分查找优化;
- `athrow` + 异常表(独立大层);
- `checkcast`/`instanceof`(需类层次/组件类型判定,与引用返回 `areturn`/`Value::Reference` 一并)。

## 7. 测试策略

TDD 红绿 + 真实字节码闸门:

1. **单元**(lib `mod.rs` tests):
   - `ifnull`/`ifnonnull`:压 null/非 null 引用,断言跳/不跳。
   - `if_acmpeq`/`if_acmpne`:压相等引用 / 不等引用 / 两 null,断言分支。
   - `tableswitch`:构造跳转表(如 low=0 high=2),index 命中各槽 + 越界走 default。
   - `lookupswitch`:稀疏 key 命中 + 未命中走 default。
   - `goto_w`:4 字节偏移跳转。
2. **集成闸门**(`tests/control_flow.rs`):`javac` 编译含 `== null` 判定、引用相等、
   `switch(int)`(密集→tableswitch、稀疏→lookupswitch)的 Java,断言与 JVM 一致。
   无 javac 则跳过。

每任务先红(看失败原因正确)后绿,频繁提交。

## 8. 自检

- 范围:7 条控制流指令;`jsr`/`ret`/`wide`/`athrow`/`checkcast`/`instanceof` 明确顺延。
- 偏移语义:switch 与 goto_w 偏移相对**指令地址**(`pc`),i32,经 `branch_target_w`;
  `if*`/`if_acmp*` 偏移 i16,经既有 `branch_target`。填充对齐公式已给并覆盖边界。
- 类型一致:`cond_ref1`/`cond_ref2` 与既有 `cond1`/`cond2` 同形;`Reference` 已是
  `PartialEq`/`Copy`,可直接 `a == b`。
