# 4.10j+ Java 栈轨迹捕获 —— 异常打印调用链

## 触发 / 目标

调试 `String.concat` 探针时,`UnsatisfiedLinkError` 抛出**无方法名**(stub 异常仅
`class_name`),不得不反复加临时 `eprintln` 定位是哪个 native 缺失。用户要求:「报错
打印栈轨迹,方便调试」。

**目标:** 抛出的异常携带 Java 调用链(class.method),在**未捕获冒到顶层**时自动 `eprintln`
栈轨迹;并提供 `Vm::format_trace` 供测试/诊断显式取用。

## Step 0 源码依据

- `System.arraycopy` 之外,本层聚焦 `Throwable.fillInStackTrace`:`java/lang/Throwable.java`
  构造器调 `fillInStackTrace()`(native),其把当前栈快照存入瞬态 `backtrace` 字段;HotSpot
  侧 `JVM_FillInStackTrace`(`prims/jvm.cpp`)走 `java_lang_Throwable::fill_in_stack_trace`
  → `StackTrace`。**rustj 无可走的 Rust 栈**,故须**显式维护** Java 调用栈。

## 现状(核对)

- `throw_exception(vm, class)`(`interpreter/mod.rs:53`)alloc 一个 stub `Instance`(仅
  `class_name + fields`),无消息、无回溯。
- `Vm`(`runtime/vm.rs:19`):`heap / registry / string_pool / frame_depth / stack_limit`,
  **无调用栈**。
- `interpret_with(&self, frame, &mut Vm)` 经 4 路 `invoke_*`(`run_with_depth` 包裹)递归,
  共享同一 `&mut Vm` → Vm 级 `Vec` 全可达。
- `Interpreter`(`mod.rs:122`):`code/cp/exception_table`,**无类/方法身份**。
- 引导桩(`oops/bootstrap.rs`)含 `Throwable/Error/…/NullPointerException` 等类;
  `native.rs` 的 `fillInStackTrace` 现为空操作返 `this`。
- `ConstantPoolEntry::Utf8(String)`(owned);`cp.get(idx)` 借出 `'a`;`invoke.rs` 已有
  `utf8(cp, idx) -> &'a str` 助手 → 方法名可零分配借得。`LoadedClass::name() -> &'a str`。
- `Reference` = 句柄 id(堆 `Vec<Oop>` 单调分配,不复用)→ 可作 `HashMap` 键。
- `LineNumberTable` 仅以原始 `Attribute` 字节存(`classfile/attributes.rs`),**未解码**
  → 本层不做行号(顺延,见债)。

## 设计

### 1. 调用栈帧(`runtime/call_stack.rs`,新;或并入 `vm.rs`)

```text
pub struct CallFrame { class: String, method: String }   // 内部名 + 方法名(无 desc/行号)
```
`String` 拥有:`push_frame` 取 `&str` 克隆入栈(各处来源生命周期不一,统一 owned 最简)。

### 2. `Vm` 增两个字段 + 访问器(`vm.rs`)

```text
call_stack: Vec<CallFrame>          // 当前活动 Java 栈(逐帧 push/pop)
traces: HashMap<Reference, Vec<CallFrame>>   // 抛出时快照,键=异常句柄
```
- `push_frame(class: &str, method: &str)` / `pop_frame()`。
- `record_trace(exc: Reference)`:克隆当前 `call_stack` 存入 `traces[exc]`(抛出点栈满)。
- `pub fn format_trace(&self, exc: Reference) -> String`:读 `traces[exc]`,无则空串;
  格式 `ExcClass\n\t at Class.method\n\t at …`(调用者→被调用者,栈顶在最下/或最上——
  采用 Java 惯例:**最内(抛出)帧在前**)。

### 3. `Interpreter` 增方法身份(`mod.rs`)

```text
identity: Option<MethodIdentity<'a>>   // None = 匿名(纯算术单测)
struct MethodIdentity<'a> { class: &'a str, name: &'a str }
fn with_identity(self, class: &'a str, name: &'a str) -> Self   // 流式
```
- `interpret_with` 重构为**包裹**:`push_frame`(若 `identity` 有)→ 跑循环(改名 `run`)
  → `pop_frame`(配对,含所有返回/Err 路径)。
- **零分配**:`class`←`target_lc.name()`、`name`←`utf8(cp, name_index)`(均 `'a` 借)。
- **顶层未捕获自动打印**:包裹尾,若结果为 `Err(ThrownException(r))` 且 `frame_depth == 0`
  (顶层帧)→ `eprintln!("{}", vm.format_trace(r))`。`frame_depth==0` 精确刻画"未捕获冒顶"
  (各 `run_with_depth` 包裹的被调用者帧 depth≥1;同帧 `find_handler` 捕获的不会冒顶返回)。

### 4. 抛出点快照(`throw_exception`)

`throw_exception` 末(alloc 异常后)调 `vm.record_trace(reference)`:此刻 `call_stack` 满。
stub 异常不经真 `Throwable.<init>`,故在此直接捕获(等价 fillInStackTrace 语义)。

### 5. `fillInStackTrace` native 接管(前瞻)

`("java/lang/Throwable","fillInStackTrace","()Ljava/lang/Throwable;")`:`record_trace(this)`
→ 返 `this`。为将来真 `Throwable.<init>` 走通预留(届时构造器调它即捕获)。

### 6. native 帧(`native.rs`)

`native::invoke` 入口 `push_frame(class, name)`、出口 `pop_frame()`(guard)。
→ 未登记 native 抛 `UnsatisfiedLinkError` 时,栈轨迹**含该 native 帧**(直接答"缺哪个 native")。

### 7. `invoke.rs` 4 处给被调用者 Interpreter 装身份

```text
Interpreter::new(&code.code, &target_lc.cf.constant_pool)
    .with_exception_table(&code.exception_table)
    .with_identity(target_lc.name(), utf8(&target_lc.cf.constant_pool, target_method.name_index)?)
```
(ACC_NATIVE 路径无 Interpreter,其帧由 `native::invoke` 自推。)

## TDD

- **红** `tests/stack_trace.rs`:javac 编 `class Trace { int deep(){return 1/0;} mid(){return deep();} top(){return mid();} }`,
  跑 `top` → `ThrownException(ArithmeticException)`;断言 `vm.format_trace(r)` 含 `deep`、`mid`、
  `top` 且顺序为 抛出帧→调用者(最内在前)。**修前** `format_trace` 返空 → 红。
- **绿**:实现上述,断言过。

## 闸门 / 不回归

- 全套(234+ 集成)不回归:顶层未捕获自动打印仅 `eprintln`(不改变返回值/不抛)。
- `stack_trace.rs` 过。
- clippy 干净、零 unsafe。

## 债 / 顺延

- **行号**:`LineNumberTable` 解码 + 每帧记 `pc`(抛出点)→ `pc↔line` 映射。当前仅 class.method。
- 真 `Throwable.getStackTrace()` → `StackTraceElement[]`(需载 `StackTraceElement` 类、
  懒转 `backtrace`);现为 `format_trace` 文本输出。
- 异常 `detailMessage`(如 `/ by zero`、`arraycopy: type mismatch …`)——各抛出点带上消息。
- `Vm::traces` 无界增长(异常多时);暂可接受(单 Vm 短命)。
