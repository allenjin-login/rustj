# Layer 4.5 `areturn` + `Value::Reference` 设计

> 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_areturn)`。
> 当前 `Value = Int/Long/Float/Double/Void`,**无引用变体**——任何返回对象/数组引用的方法
> 都无法执行。本层补这一缺口:加 `Value::Reference`,`areturn` 返回它,`invoke` 把它压回
> 调用者栈。这是"返回引用"的最小地基,解锁大量真实 Java(工厂方法、getter、`clone()` 等)。

## 1. 目标

让 rustj 执行返回引用的方法:

```java
static int[] makeArray() { return new int[5]; }
static int use() { return makeArray().length; }  // 5
```

零 unsafe。

## 2. `Value` 增 `Reference` 变体

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Reference(Reference), // 新增
    Void,
}
```

`Reference`(`Option<u32>` newtype)已是 `Copy`/`Clone`/`PartialEq`/`Debug`,故 `Value`
保持 `Copy`。`Reference` 经 `use super::slot::Reference` 已在 `mod.rs` 顶部导入。

## 3. `areturn` 分派

栈:`..., objectref → [空]`。弹一个引用,作为方法返回值。

```rust
Opcode::Areturn => {
    let v = frame.operands.pop_reference()?;
    return Ok(Value::Reference(v));
}
```

> 与 `ireturn`/`lreturn`/`freturn`/`dreturn` 同形(弹栈 → `Ok(Value::*` 早退)。无 PC 推进
> (return 提前退出循环)。

## 4. `invoke` 回填:返回引用压回调用者栈

`interpreter/invoke.rs` 的 `push_return` 增臂:

```rust
fn push_return(frame: &mut Frame, v: Value) -> Result<(), VmError> {
    match v {
        Value::Int(x) => frame.operands.push_int(x)?,
        Value::Long(x) => frame.operands.push_long(x)?,
        Value::Float(x) => frame.operands.push_float(x)?,
        Value::Double(x) => frame.operands.push_double(x)?,
        Value::Reference(r) => frame.operands.push_reference(r)?,
        Value::Void => {}
    }
    Ok(())
}
```

`invokestatic`/`invokevirtual`/`invokespecial`/`invokeinterface` 均经 `push_return`,一处
改动覆盖所有 invoke 变体。

## 5. 穷尽匹配审计

`Value` 是 `pub enum`,新增变体会让所有穷尽 `match` 编译失败——这是特性,确保不漏。
已知穷尽匹配点:`push_return`(本层补)。其余 `Value` 使用多为构造/比较(`assert_eq!(...,
Value::Int(...))`)或 `interpret` 返回值传递,非穷尽匹配,无需改。编译器会标出任何遗漏。

## 6. 测试策略

TDD 红绿 + 真实字节码闸门:

1. **单元**(`mod.rs` tests):
   - `areturn_returns_null_reference`:`aconst_null; areturn` → `Value::Reference(null)`。
   - `areturn_returns_local_reference`:local0 = `Reference::from_id(7)`;`aload_0; areturn`
     → `Value::Reference(from_id(7))`。
   - `invoke_static_returns_reference`:`invokestatic` 一个 `areturn` 引用的方法,调用者栈收到
     该引用(经注册表构造;断言压回后可 `astore`/读回)。
2. **集成闸门**(`tests/areturn.rs`):`javac` 编 `makeArray()` 返回 `int[]`、`use()` 调它读
   `.length`;断言 `use()` == 5(验证 areturn + push_return 端到端)。无 javac 则跳过。

每任务先红(看失败原因正确)后绿,频繁提交。

## 7. 顺延项

- `checkcast`/`instanceof`(需类层次赋值兼容判定;与 `Value::Reference` 解耦,独立层);
- `athrow` + 异常表(大层);
- 返回引用后的 `toString()`/`equals()` 等(需类库/本地方法,远期)。

## 8. 自检

- 范围:仅 `Value::Reference` + `areturn` + `push_return` 臂;不动其他返回指令。
- 类型一致:`Reference` 已 `Copy`/已导入;`areturn` 与 `ireturn` 同形;`push_return` 单点
  覆盖所有 invoke。
- 最小性:不引入 `checkcast`(顺延);不存"返回类型校验"(校验器层顺延)。
