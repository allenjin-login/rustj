# Layer 4.7b — 异常/错误模型统一(design spec)

日期:2026-06-27
状态:设计
前置:Layer 4.7(athrow + 异常表 + 跨帧传播,已闭环)

## 1. 动机(技术债)

Layer 4.7 落地后,`VmError` 存在**两条并行的错误通道**,违反 JVM 语义且债务持续累积:

1. **Java 异常通道**:`VmError::ThrownException(Reference)`——用户 `athrow` 抛出的异常
   对象,经异常表分派、可被 `try/catch` 捕获。✅ 语义正确。
2. **Rust 枚举通道**:`NullPointer`/`ClassCastException`/`DivideByZero`/
   `ArrayIndexOutOfBounds`/`NegativeArraySize`/`AbstractMethodError`/`StackOverflow`
   ——这些**本应是可捕获的 Java 异常**(NPE、CCE、ArithmeticException 等),却以
   Rust 枚举值绕过异常表,**任何 `try/catch` 都捕不到**。❌ 与 JVM 不一致。

**债务复合点**:约 24 处指令处理器 `return Err(VmError::X)`。每新增一个会失败的指令
(数组协变 ArrayStoreException、类链接 VerifyError……)都会再硬编码一条不可捕获路径。
越晚统一,回填点越多。

`VmError` 里**真正不可捕获**的只有解释器内部故障:`BadConstant`(损坏字节码/校验失败)、
`BadPc`、`Frame`(栈帧槽类型错)、`ConstantPool`、`UnsupportedOpcode`。这些对应
`VerifyError`/`InternalError` 性质,在当前层不抛给用户代码捕获。

## 2. 目标 / 非目标

### 目标
- **单一 Java 错误通道**:所有 Java 异常(用户 athrow + 运行时异常)统一为
  `VmError::ThrownException(Reference)`,经异常表分派,**同帧与跨帧均可捕获**。
- **可加载的引导异常类**:标准 `java.lang.*` 异常层次(Throwable/Error/Exception/
  RuntimeException 及常见子类)作为**注册表桩**存在,使 `catch(Throwable)`/
  `catch(Exception)`/`catch(NullPointerException)` 等即使未显式加载也能匹配。
- **架构鲜明**:`VmError` 二分清晰——`ThrownException`(Java 语义,可捕获)vs 内部故障
  (不可捕获)。异常分派单点化(同帧循环 + 跨帧 invoke 共用一个 `find_handler` 路径)。

### 非目标(顺延)
- 异常的**栈轨迹**(`Throwable.fillInStackTrace`/`getStackTrace`)。
- 异常的**消息/cause 字段**(`Throwable.getMessage`/`getCause`/`<init>(String)`)——
  桩类无字段,异常对象仅承载类名。
- `finally` 的**正常路径复制**(javac 已在字节码层复制 finally;运行时无需特殊处理,
  4.7 闸门已验证 catch-all via finally)。
- 真实 JDK `Throwable.class` 加载(用合成桩替代)。
- 校验器抛 `VerifyError` 的完整语义(桩阶段内部故障仍走 `BadConstant`)。

## 3. 设计

### 3.1 引导异常类——注册表桩(单一机制)

标准异常类作为**合成 `ClassFile`(仅类名 + 超类名,空字段/方法)在 `ClassRegistry::new()`
时加载**。复用既有全部机制(`load`/`new_instance`/`supertypes_of`/`is_instance`),**零
特殊分支**——桩就是普通 `LoadedClass`。

- 新增 `oops::bootstrap::synth_classfile(name, super_name) -> ClassFile`:构造最小常量池
  (`Utf8(name)`、`Utf8(super_name)`、`Class{name}`、`Class{super}`)+ `this_class`/`super_class`,
  空 fields/methods/interfaces,经既有 `ConstantPool::parse` 解析。
- `ClassRegistry::new()` 调 `install_bootstrap()` 载入下表全部类。
- **为何 eager**:Vm 以**不可变**借用持注册表(`registry: Option<&'a ClassRegistry>`),
  抛异常处无法 `&mut` 安装;故必须在 Vm 借用前(构造期)装好。

引导层次表(`extends` 关系,单一真相源):

```
java/lang/Object                         (根)
└ java/lang/Throwable
   ├ java/lang/Error
   │  ├ java/lang/AbstractMethodError
   │  └ java/lang/StackOverflowError
   └ java/lang/Exception
      └ java/lang/RuntimeException
         ├ java/lang/NullPointerException
         ├ java/lang/ClassCastException
         ├ java/lang/ArithmeticException
         ├ java/lang/ArrayStoreException
         ├ java/lang/NegativeArraySizeException
         └ java/lang/IndexOutOfBoundsException
            ├ java/lang/ArrayIndexOutOfBoundsException
            └ java/lang/StringIndexOutOfBoundsException
```

(扁平 `(name, super_name)` 表;后续可扩展 `IllegalArgumentException` 等。)

### 3.2 抛出辅助 `throw_exception`

`fn throw_exception(vm: &mut Vm, class_name: &str) -> VmError`:取注册表桩类 →
`new_instance` → `heap_mut().alloc` → `VmError::ThrownException(ref)`。沿用 4.2 的
`'a` 借用技巧(`registry()` 返回 `&'a ClassRegistry` 不绑 `&self`,取 `&'a LoadedClass`
后仍可 `&mut vm`)。

各指令处理器把 `return Err(VmError::NullPointer)` 改为
`return Err(throw_exception(vm, "java/lang/NullPointerException"))`。映射:

| 现变体 | Java 类 |
|---|---|
| `NullPointer` | `java/lang/NullPointerException` |
| `ClassCastException` | `java/lang/ClassCastException` |
| `DivideByZero` | `java/lang/ArithmeticException` |
| `ArrayIndexOutOfBounds` | `java/lang/ArrayIndexOutOfBoundsException` |
| `NegativeArraySize` | `java/lang/NegativeArraySizeException` |
| `AbstractMethodError` | `java/lang/AbstractMethodError` |
| `StackOverflow` | `java/lang/StackOverflowError` |

`ArrayStoreException`(数组协变层用)与 `StringIndexOutOfBoundsException` 桩先行铺好,供
后续层。

### 3.3 同帧捕获——分派循环重构(核心架构改动)

**问题**:当前指令 `?` 传播 `Err` 直接逃出 `interpret_with`,**本帧异常表从不被咨询**
(仅 `athrow` 内联咨询)。故指令抛出的 NPE 在本帧不可捕获。

**改法**:把单步分派从 `loop` 体抽成 `dispatch(op, frame, vm, &mut pc) -> Result<Step, VmError>`,
`Step = Continue | Return(Value)`。循环统一处理 `ThrownException`:

```rust
loop {
    if pc >= code.len() { return Err(BadPc(pc)); }
    let op = Opcode::from_u8(code[pc])?;
    match self.dispatch(op, frame, vm, &mut pc) {
        Ok(Step::Continue) => {}
        Ok(Step::Return(v)) => return Ok(v),
        Err(VmError::ThrownException(exc)) => {
            match exception::find_handler(self, vm, self.exception_table, pc, exc)? {
                Some(h) => { frame.operands.clear(); frame.operands.push_reference(exc)?; pc = h; }
                None => return Err(VmError::ThrownException(exc)),
            }
        }
        Err(e) => return Err(e),
    }
}
```

- `pc` 在 `Err` 路径仍指向**故障指令**(`?` 在 `pc += n` 之前返回)→ `find_handler` 用对 pc。
- `athrow` 臂简化为:弹引用(null → `throw_exception(NPE)`)→ `Err(ThrownException(exc))`;
  **表查找移到循环统一处理**,与指令抛出合一(消重复)。
- 跨帧:invoke 的 `finish_invoke` 已在 4.7 咨询**调用者**表;`Step` 不影响它。同帧(循环)+
  跨帧(`finish_invoke`)共用 `find_handler`。

### 3.4 `VmError` 清理

移除变体 `NullPointer`/`ClassCastException`/`DivideByZero`/`ArrayIndexOutOfBounds`/
`NegativeArraySize`/`AbstractMethodError`/`StackOverflow`(全转为 `ThrownException`)。
保留内部故障:`UnsupportedOpcode`/`BadPc`/`Frame`/`ConstantPool`/`BadConstant`/`ThrownException`。
`Display` 对应更新。

## 4. 风险与缓解

- **dispatch 抽取触及 ~200 臂**:机械(`pc += n`→`*pc += n`、`return Ok(v)`→
  `return Ok(Step::Return(v))`)。239 测试 + 新闸门逐臂钉死行为。分阶段提交(先桩+throw+
  跨帧,后 dispatch 抽取)。
- **桩类污染既有注册表**:`new()` 装入标准类 → 既有 `is_instance`/`supertypes_of` 测试
  可能受影响(用户异常类现能上行到 Throwable)。TDD 阶段逐个验证、必要时调整断言。
- **borrow 检查**:`dispatch(&self, frame: &mut, vm: &mut, pc: &mut)` 四参;self 不可变、
  余可变,互不别名。沿用既有 `&'a` 技巧。

## 5. 测试计划

- **单元**:`bootstrap` 模块——桩类全部加载、`is_instance` 标准层次(NPE 是 Exception/
  Throwable、不是 Error)、`new_instance` 可造 NPE 对象。
- **单元**:`dispatch`/`find_handler`——同帧指令抛 NPE 被本帧 `catch(NPE)` 捕获(红→绿)。
- **既有测试回填**:7 处断言 `VmError::NullPointer` 等改为匹配 `ThrownException`(类名核对)。
- **集成闸门 `tests/throw_internal.rs`**(javac):getfield-on-null → catch(NPE) 返回标记;
  idiv-by-zero → catch(ArithmeticException);数组越界 → catch(ArrayIndexOutOfBoundsException);
  跨帧调用者 catch;未捕获 NPE 传播为 ThrownException。
- 全闸门:cargo test 全绿、clippy `-D warnings`、零 unsafe。
