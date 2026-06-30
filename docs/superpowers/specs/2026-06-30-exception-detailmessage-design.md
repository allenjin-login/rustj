# Layer 4.10o — 异常 detailMessage(框架 + 算术 "/ by zero")

## 背景

异常债之一:JVM 自动抛出的异常**无 detailMessage**(仅类名)。`/ by zero`、
`arraycopy: type mismatch …` 等诊断信息丢失,排查靠猜。

## 现状 / 设计

异常元数据原先散为两个并行映射(`traces`/`causes`)。再加 message 会成第三张表 ——
遂**合并**为单一 `exception_meta: HashMap<Reference, ExceptionMeta>`:

```rust
#[derive(Default, Clone)]
struct ExceptionMeta {
    frames: Vec<CallFrame>,    // fillInStackTrace 捕获
    cause: Option<Reference>,  // Throwable.cause
    message: Option<String>,   // Throwable.detailMessage
}
```

- `record_trace` / `record_cause` / `record_message` 各写一字段(`entry().or_default()`)。
- `format_trace` 渲染 `Class[: message]\n\tat …`,沿 cause 追链 `Caused by: Class[: msg]`。
- 头异常三字段皆空 → 空串(旧契约)。

镜像 HotSpot:`THROW(vmSymbols::X())` 无消息;`THROW_MSG(vmSymbols::X(), msg)`
带消息(对应 `new X(String)`)。rustj:`throw_exception` ↔ THROW;新增
`throw_exception_with_message` ↔ THROW_MSG(throw 后 `record_message`)。

> 真 `Throwable.getMessage/getCause/getStackTrace` 字段回填是更大的独立层(需载
> `StackTraceElement`、按名写实例字段);当前先以此并行结构服务 `format_trace` 诊断。

## 本层落地(已验证)

`idiv`/`irem`/`ldiv`/`lrem` 除零 → `ArithmeticException("/ by zero")`
(bytecodeInterpreter 四处除零分支,消息恒为 "/ by zero"):

```rust
throw_exception_with_message(vm, "java/lang/ArithmeticException", "/ by zero")
```

## TDD

`arithmetic_exception_carries_by_zero_message`:idiv 除零 → `format_trace` 含
`java/lang/ArithmeticException: / by zero`。

## 顺延(下一子层 4.10p)

`arraycopy` 忠实消息:HotSpot `typeArrayKlass::copy_array`(typeArrayKlass.cpp:108-174)
与 `objArrayKlass::copy_array`(objArrayKlass.cpp:244-316)的 `THROW_MSG` 已读全:

- 类型不符:`arraycopy: type mismatch: can not copy {T}[] into {T/Object}[]`(或 "object array[]")
- 非数组目的:`arraycopy: destination type {ext} is not an array`
- 负值:`arraycopy: source/destination index {n} out of bounds for {T|object array}[{len}]` /
  `length {n} is negative`
- 越界:`arraycopy: last source/destination index {n} out of bounds for …[{len}]`
- checkcast 失败:`throw_array_store_exception`(消息待核)

需:描述符→类型名映射(`I`→`int`、`B`→`byte`…)、arraycopy.rs 检查序对齐 HotSpot 分支。
null→NPE 仍无消息(jvm.cpp:297 `THROW(...NPE())` 无 msg)。
