# Layer 4.7 `athrow` + 异常表 设计

> 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_athrow)` 及异常分派
> (`exception_table` 扫描 + 栈展开)。配合 4.5/4.6,补齐**用户异常的抛出与捕获**
> ——`throw new MyExc()`、`try { ... } catch (E e) { ... }`。
> 核心是**异常表扫描**:抛出点 pc 落在某条目的 `[start_pc, end_pc)` 内,且
> `catch_type`(0 = catch-all)匹配抛出对象的运行时类,则跳到 `handler_pc` 并把
> 异常对象压空后的操作数栈;无匹配则**沿调用栈向上传播**。

## 1. 目标

让 rustj 执行真实 Java 的用户异常,与 JVM 一致:

```java
try {
    throw new MyExc();        // new + athrow
} catch (MyExc e) {           // exception_table: [start,end) handler catch_type=MyExc
    return 1;
}
```

以及跨帧:`methodA() { try { methodB(); } catch (E e) {...} }`,`methodB` 内 `throw`
无本帧处理者 → 传播到 `methodA` 的 invoke 处理者。

**范围限定(本层):** 仅**用户 `athrow` 抛出的异常**可被捕获。JVM 内部错误
(`NullPointer`/`ClassCastException`/`DivideByZero`/`ArrayIndexOutOfBounds`/
`NegativeArraySize`)仍是不可捕获的 `VmError`(它们转成可捕获异常对象需核心类
`java/lang/*Exception` 可加载,顺延到 4.7b)。零 unsafe。

## 2. 异常的传播表示

`VmError` 增变体携带抛出对象:

```rust
/// 用户 athrow 抛出的异常(沿调用栈传播,直到被异常表处理者捕获)。
ThrownException(Reference),
```

`Reference` 已 `Copy`/`PartialEq`/`Eq`,VmError 保持 `#[derive(Debug, Clone, PartialEq, Eq)]`。

**栈展开机制 = Rust 调用栈**:invoke 仍是递归 `interpret_with`;抛出异常以
`Err(ThrownException(r))` 沿 Rust 栈上传,每层 invoke 调用点检查**调用者帧**
的异常表。无需显式帧栈(与既有"Rust 栈即 JVM 栈"一致,4.2b SOE 同此)。

## 3. 异常表来源

`CodeAttribute.exception_table: Vec<ExceptionTableEntry>`(Layer 1 已深解析,
`{start_pc, end_pc, handler_pc, catch_type}`)。`Interpreter` 当前仅持 `code` +
`cp`,**需增 `exception_table` 引用**。

```rust
pub struct Interpreter<'a> {
    code: &'a [u8],
    cp: &'a ConstantPool,
    exception_table: &'a [ExceptionTableEntry],  // 新增
}
```

- `Interpreter::new(code, cp)` 保留,`exception_table = &[]`(既有 ~67 处数值/单元
  测试调用零改动——它们无 athrow,空表正确)。
- 新增 `Interpreter::new_with_exception_table(code, cp, exception_table)`:4 处
  invoke 构造被调用者解释器、集成闸门构造入口用之。

## 4. 异常表扫描 `find_handler`

```rust
/// 在 `table` 里找覆盖 `pc` 且匹配 `exc` 运行时类的处理者;返回 `handler_pc`。
/// catch_type == 0 → catch-all(finally / catch(Throwable) 的原始 0);否则解析
/// 目标类名,用 `ClassRegistry::is_instance(运行时类, 目标)` 判匹配。
/// 顺序扫描,**首条匹配胜出**(JVMS 要求表内顺序即优先级)。
fn find_handler(
    interp: &Interpreter<'_>,
    vm: &Vm<'_>,
    table: &[ExceptionTableEntry],
    pc: usize,
    exc: Reference,
) -> Result<Option<usize>, VmError> {
    for e in table {
        if (e.start_pc as usize) <= pc && pc < (e.end_pc as usize) {
            let hit = if e.catch_type == 0 {
                true
            } else {
                let target = resolve_class_name(interp.cp(), e.catch_type)?;
                let exc_class = runtime_class_name(vm, exc)?;  // InstanceOop.class_name
                let reg = vm.registry().ok_or(VmError::BadConstant("异常分派需类注册表"))?;
                reg.is_instance(&exc_class, &target)
            };
            if hit {
                return Ok(Some(e.handler_pc as usize));
            }
        }
    }
    Ok(None)
}
```

`runtime_class_name(vm, exc)`:经堆取 `Oop::Instance(i)`,`i.class_name()`(own String)。
数组对象不可能是 athrow 对象(用户 `throw` 的必是 Throwable 子类实例),故只处理
`Instance`;遇 `Array`/悬空 → `BadConstant`。形同 4.6 `type_check::object_type`,
可置 `interpreter/exception.rs` 子模块。

## 5. `athrow`(0xbf)

格式:`opcode(1)`,`pc += 1`(若抛出则不返回)。栈:`..., objectref → [抛出]`。

```rust
Opcode::Athrow => {
    let exc = frame.operands.pop_reference()?;
    if exc.is_null() {
        return Err(VmError::NullPointer);  // throw null → NPE(JVM 语义)
    }
    match exception::find_handler(self, vm, self.exception_table, pc, exc)? {
        Some(h) => {
            frame.operands.clear();
            frame.operands.push_reference(exc)?;
            pc = h;  // 进入处理者
        }
        None => return Err(VmError::ThrownException(exc)),  // 本帧无处理者 → 上传
    }
}
```

**关键:** 处理前 `operands.clear()`(JVM 规定:进入异常处理者时操作数栈只保留该异常对象)。

## 6. invoke 跨帧传播

被调用者递归 `interpret_with` 可能返 `Err(ThrownException(r))`。调用者须在**调用者帧**
异常表(invoke 指令的 pc)找处理者。为此 invoke 子模块的入口函数:

1. 签名增 `caller_table: &[ExceptionTableEntry]` + `caller_pc: usize`;
2. 返回类型从 `Result<(), VmError>` 改为 `Result<InvokeFlow, VmError>`:

```rust
/// invoke 后调用者分派循环的流向。
pub(super) enum InvokeFlow {
    /// 正常返回(含 void);调用方推进 pc(+3 / +5)。
    Fallthrough,
    /// 捕获了被调用者抛出的异常,已设好处理帧;调用方跳到 handler_pc(不推进)。
    Jump(usize),
}
```

3. 统一"返回值回填 + 异常捕获"为单一 helper(消除 4 处重复的 `match (return_type, result)`):

```rust
fn finish_invoke(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    caller_table: &[ExceptionTableEntry],
    caller_pc: usize,
    result: Result<Value, VmError>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    match result {
        Ok(v) => {
            // 既有 void/非 void 回填逻辑(原各 invoke 末尾的 match 块)
            match (return_type, v) {
                (ReturnDescriptor::Void, Value::Void) => {}
                (ReturnDescriptor::FieldType(_), Value::Void) => {
                    return Err(VmError::BadConstant("invoke 期望返回值,被调用者返回 void"));
                }
                (ReturnDescriptor::FieldType(_), val) => push_return(frame, val)?,
                (ReturnDescriptor::Void, _) => {
                    return Err(VmError::BadConstant("invoke void 方法返回了值"));
                }
            }
            Ok(InvokeFlow::Fallthrough)
        }
        Err(VmError::ThrownException(exc)) => {
            match exception::find_handler(interp, vm, caller_table, caller_pc, exc)? {
                Some(h) => {
                    frame.operands.clear();
                    frame.operands.push_reference(exc)?;
                    Ok(InvokeFlow::Jump(h))
                }
                None => Err(VmError::ThrownException(exc)),  // 继续上传
            }
        }
        Err(e) => Err(e),  // 内部错误原样上传(本层不可捕获)
    }
}
```

各 invoke 函数末尾用 `finish_invoke(interp, frame, vm, table, pc, result, md.return_type)`
取代原 `match (md.return_type, result) { ... } Ok(())`。

分派臂(`mod.rs`):

```rust
Opcode::Invokestatic => {
    let index = self.read_u2(pc + 1)?;
    match invoke::invoke_static(self, frame, vm, index, self.exception_table, pc)? {
        invoke::InvokeFlow::Fallthrough => pc += 3,
        invoke::InvokeFlow::Jump(h) => pc = h,
    }
}
```

(invokevirtual/special/interface 同形,`pc += 5` 对 invokeinterface,其余 +3。)

## 7. `OperandStack::clear`

```rust
pub fn clear(&mut self) { self.slots.clear(); }
```

异常处理者进入前清空操作数栈(JVMS:处理者激活时操作数栈仅含异常对象)。

## 8. 错误模型

新增 `VmError::ThrownException(Reference)`(+ Display 臂)。`athrow null` 复用既有
`NullPointer`(throw null 的 NPE;该 NPE 顺延到 4.7b 才可捕获)。无其他新错误。

## 9. 顺延项

- **内部异常可捕获化(4.7b)**:`NullPointer`/`ClassCastException`/`DivideByZero`/
  `ArrayIndexOutOfBounds`/`NegativeArraySize` 转成可捕获的 `ThrownException` 对象,
  需核心类 `java/lang/*Exception` 可加载(或合成最小 Throwable 层次)。当前它们以
  `VmError` 上传、`finish_invoke` 的 `Err(e) => Err(e)` 分支原样传播,**不可被
  try/catch 捕获**。
- **`catch (Throwable)` / 核心异常类**:`catch_type` 指向未加载的 `java/lang/Throwable`
  时,`is_instance` 看不到它(超类链在未加载类处断)→ 不匹配。用户层 try/catch 须用
  **已加载的用户异常类**(集成闸门用自定义 `BaseExc`/`SubExc` 层次)。
- **finally 完整语义**:javac 把 finally 内联 + catch-all(catch_type 0)处理者重抛;
  catch_type 0 的捕获本层支持,但 finally 内 `return`/嵌套重抛的边界顺延验证。
- **synchronized**(monitorenter/monitorexit)的隐式异常:顺延。
- **异常填充栈轨迹**(getMessage/toString):需类库/本地方法,顺延。

## 10. 测试策略

TDD 红绿 + 真实字节码闸门:

1. **单元**(`exception.rs` 或 `mod.rs`):手构字节码 + 异常表条目,测
   `find_handler`/athrow 各分支:
   - catch_type 匹配(命中 → handler_pc;pc 出 [start,end) → 不命中);
   - catch_type 0(catch-all 命中);
   - 无处理者 → `Err(ThrownException)`;
   - athrow null → `NullPointer`。
2. **集成闸门**(`tests/throw.rs`):`javac` 编自定义异常层次 `BaseExc extends Throwable` /
   `SubExc extends BaseExc` / `OtherExc`,测:
   - 精确 catch(`throw new SubExc()` 被 `catch (SubExc)`)→ 1;
   - 超类 catch(`throw new SubExc()` 被 `catch (BaseExc)`,验 is_instance 跨用户层次)→ 1;
   - 不匹配(被 `catch (OtherExc)`)→ 传播,顶层 `run_err` 断 `ThrownException`;
   - 跨帧(被调用者 throw 无本帧处理者,调用者 invoke 处 catch)→ 1;
   - catch_type 0 / finally:minimal(若 javac 字节码过繁则降为单元覆盖)。
3. 无 javac 则跳过闸门。

每任务先红(看失败原因正确)后绿,频繁提交。

## 11. 自检

- **范围:** `athrow` + 异常表扫描 + 跨帧传播;内部异常可捕获化明确顺延(4.7b)。
- **一致性:** 传播借 Rust 栈(同 4.2b SOE);`find_handler` 复用 4.6 `is_instance`;
  `finish_invoke` 统一 4 处 invoke 的返回/异常处理(DRY)。
- **null 语义:** `throw null` → NPE(顺延可捕获);异常对象 null 不会进入处理者。
- **最小性:** 不加显式帧栈;不加核心异常类加载;不重构分派循环为 Step 枚举(仅 invoke
  端引入 `InvokeFlow`,athrow 内联)。
