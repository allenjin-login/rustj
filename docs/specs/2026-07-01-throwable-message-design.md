# Layer 4.10s — 真 `Throwable.getMessage()` / `getCause()` 经真实例字段

**日期**:2026-07-01
**北极星**:加载并运行真实 `java.base`,逐步退役合成桩。
**前置**:4.10r(真 `Throwable.getStackTrace()` + `capture_backtrace` 直接置真 Throwable 字段,
绕过 `<init>`)、4.10i(真 `java/lang/String`)、4.7b(`throw_exception`/`throw_exception_with_message`)。

## 动机

4.10r 让真 `getStackTrace()` 跑通,但 `getMessage()`/`getCause()` 对 **JVM 自动抛出**的异常返
null。根因:`throw_exception(_with_message)` 用 `new_instance` 分配异常实例(**不经真
`<init>`**),仅把 message/cause 存进并行的 `exception_meta`(`record_message`/`record_cause`),
供 `format_trace` 渲染。而真 `getMessage()`(Throwable.java:409)= `return detailMessage;`、
`getCause()`(448)= `return (cause==this ? null : cause);` 读的是**实例字段**,字段未填 → null。

**用户抛出**(`new RuntimeException("boom")`)则经真 `<init>` 字节码(`Throwable.<init>(String)`
行 393 `detailMessage = message;`、`<init>(String, Throwable)` 行 ~397 `this.cause = cause;`),
**已自动**置字段 → getMessage/getCause 本就正确。故缺口仅在自动抛出路径。

## Step 0 源码依据(JDK `src/java.base/`)

- `Throwable.detailMessage`(`String`,Throwable.java:138);`cause`(`Throwable`,初值 `= this`,205)。
- `getMessage()`(409):`return detailMessage;`(纯字段读字节码)。
- `getCause()`(448):`return (cause==this ? null : cause);`(`synchronized` 方法级,rustj 单线程忽略)。
- rustj `new_instance` 把引用字段默认置 **null**(非 `this` 哨兵)→ 无 cause 时 getCause:
  `null==this`? 否 → 返 null,语义正确。包裹时置 `cause`=真 cause 引用 → 返该引用。
- `Throwable.<init>(String)`(行 ~393):`fillInStackTrace(); detailMessage = message;` ——
  用户 `new X(msg)` 经 invokespecial 跑此字节码自置字段(`fillInStackTrace` native = `capture_backtrace`)。

## 方案:镜像 `capture_backtrace`,直接置真实例字段

新增 `set_throwable_field(vm, exc, name, ft, slot)`(interpreter/mod.rs,`pub(super)`):经真
`Throwable` 扁平布局解析字段全局序号(`instance_field`,沿用 4.2 前缀不变量:子类布局是超类
前缀 → 序号在子类实例一致),置 `exc` 实例该槽;桩(无该字段)→ `instance_field` 返 None →
静默跳过(与 `capture_backtrace` 之于 backtrace/depth 同构)。

- **`throw_exception_with_message`**:`record_message` 后,`set_throwable_field(exc,
  "detailMessage", String, intern(message))` —— 使自动抛出的 ArithmeticException 等的
  `getMessage()` 返正确消息。
- **`clinit.rs` EIIE 包裹**:`record_cause(eiie, cause)` 后,`set_throwable_field(eiie,
  "cause", Throwable, Slot::Reference(cause))` —— 使 `getCause()` 返被包根因。

**不动 `getMessage`/`getCause` 本身**(它们跑真字节码);不退役 `record_message`/`record_cause`
(`format_trace` 仍读 `exception_meta`,二者并行,顺延统一)。

## 变更点

- `src/runtime/interpreter/mod.rs`:新增 `set_throwable_field`;`throw_exception_with_message`
  调之置 `detailMessage`。
- `src/runtime/interpreter/clinit.rs`:EIIE 包裹处调之置 `cause`。
- `tests/throwable_message.rs`:javac + jmod 闸门 ——
  ① **自动抛出** idiv→ArithmeticException,catch 内 `e.getMessage().equals("/ by zero")` → 1
    (修前 getMessage 返 null → `.equals` 抛 NPE → 链有缺口);
  ② **用户抛出** `throw new RuntimeException("boom")`,catch 内 getMessage equals "boom" → 2
    (经真 `<init>`,验证既有路径);
  ③ **用户包裹** `new RuntimeException(new Exception("root"))` → `getCause() == root` → 3
    (经真 `<init>(Throwable)` 置 cause)。

## 测试(红→绿)

`Tm.java` 三静态法:`autoMessage` / `userMessage` / `userCause`,成功返 1/2/3,失配返负诊断。
预载 `ArithmeticException`/`RuntimeException`/`Exception`/`String` 闭包(确保异常类为真类,
带 Throwable 继承字段)。逐法 `interpret_with`,断言 `Value::Int` 哨兵。

## 顺延

- `record_message`/`record_cause` 与真字段并行(双写);待 `format_trace` 改读真字段后可退役并行结构。
- `getLocalizedMessage`/`addSuppressed`/`getSuppressed`/`initCause` 等其余 Throwable API。
- 真 `<init>` 链对自动抛出异常的完整支持(目前 new_instance 跳过 `<init>`,靠字段直填补齐)。
