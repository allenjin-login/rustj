# 提取 tests/src 公用方法、宏与特例(`testkit` + `cp_util`)

> **状态**:设计(已与用户确认 5 项决策,待 spec 审阅 → 写实现计划)
> **日期**:2026-07-20
> **范围**:行为保持型工程债清理(**非** HotSpot 源码移植层)。把 `tests/`(64 文件,~11,590 行,
> 约 51%/~5,906 行是重复样板)与 `src/` 内 `field.rs`/`invoke.rs`/`jdk_internal_reflect.rs` 的
> 真重复,收敛为两处单点定义:`src/runtime/interpreter/cp_util.rs`(VM 工具,pub(crate))+
> `src/testkit/`(测试基础设施,feature 门控)。提供守卫宏 / 断言宏。
> **性质**:纯提取 + 全量迁移;不改任何 VM 运行时语义,零 unsafe 保持,手写 4 空格(不跑 cargo fmt)。

---

## 1. 背景与动机

### 现状(全量扫描结论)
`tests/` 64 个集成测试文件中,以下辅助以 copy-paste 形式各自重复(出现文件数 / 估算重复行):

| 重复簇 | 文件数 | 行数 | 变体 |
|---|---|---|---|
| `compile`/`compile_and_load`/`compile_dir` | 61 | ~1,830 | 5(返回 PathBuf vs ClassRegistry、带/不带 `extra` javac 参数、24 种目录名前缀) |
| `run`/`run_result`/`run_err`/`run_static_in`/`run_static_int` | 59 | ~1,770 | 6(返回 Value vs Result vs i32、自建 vs 复用调用方 VmThread) |
| `find_javabase_jmod` | 46 | ~828 | 0(完全一致) |
| load `.class` 进 registry 的循环 | 36 | ~360 | 0 |
| 跳过守卫 `if !javac_available(){…; return;}` | 125 个 #[test] | ~366 | 2(javac / jmod) |
| `javac_available` | 61 | ~183 | 0 |
| `find_method` | 16 | ~144 | 3(utf8 辅助 / matches! 宏 / 内联 match) |
| `utf8` | 10 | ~60 | — |
| `static SEQ: AtomicU64` | 26 | ~52 | 2(SEQ / COMPILE_SEQ) |
| `as_int` 等、`Arg`+`run_static_value` | 少量 | ~110 | — |

`src/` 真重复(经源码核实):
- `src/runtime/interpreter/field.rs`(`field.rs:48-74`)与 `invoke.rs`(`invoke.rs:988-1014`)各自私有定义
  `utf8`/`class_name`/`name_and_type`,实现几乎逐字相同(仅 `BadConstant` 消息文案略异)。**本层唯一提取项。**
- `value_to_slot`/`slot_to_value`/`unbox_arg` 经核实非跨模块重复(见 §3.1 更正),不提取。

### Smell
1. 64 文件各自 copy-paste 同样的 javac/compile/run 守卫;新增测试要重新粘一大坨。
2. 目录名前缀 24 种(`rustj-cl-`/`rustj-th-`/`rustj-integer-`…),纯历史随意性。
3. `find_method` 三种写法并存,语义相同却各写一遍。
4. `field.rs`/`invoke.rs` 常量池解析三函数逐字重复,改一处要同步两处。
5. 守卫 `if !javac_available(){return;}` 是 early-return,函数封装不了(`return` 只退出函数自身),正适合宏。

### 目标
- tests/ 重复辅助收敛到 `src/testkit/` 单点;新测试 `use rustj::testkit::*;` 即用。
- src/ 常量池解析三函数 + `Value↔Slot` 收敛到 `cp_util.rs`(pub(crate)),源码侧单点维护。
- early-return 守卫与断言提供宏。
- 一次性全量迁移 64 测试文件 + 3 个 src 文件。

### 非目标(本层不做)
- **不动**字节码分派 arm(`iconst`/`iload` 系列)——与 JVM 规范一一对应,是"忠实映射"非重复。
- **不动** `type_check.rs` 的 `match Value`、`Oop::Instance`/`Array` 分派——语义各异,合并降可读性。
- **不重构**已有良好封装(`natives!` 宏、`string::intern`、`intern_class_mirror`、`operand_stack.rs`)。
- **不改**任何 VM 运行时行为(纯提取,零语义变化)。

---

## 2. 设计决策(5 条,均经用户确认)

| 维度 | 决策 | 理由 |
|---|---|---|
| 范围 | tests/ + src/ | 用户指定 |
| 变体统一 | 分层保留常用变体(3-4 个清晰函数) | Rust 惯用法,调用点自然;不用重 builder |
| 宏 | 守卫宏 + 断言宏(tests) | 守卫需 early-return(函数做不到);断言符合 Rust assert 惯例 |
| 迁移 | 一次性全量 | 用户指定 |
| 落点 | 全 src + feature 门控 | 用户指定;VM 工具源码要用,测试基础设施"万一要用" |

---

## 3. 架构:两块落点

公用代码性质不同 → 两块,命运不同:

### 3.1 VM 工具 → `src/runtime/interpreter/cp_util.rs`(pub(crate),**无** feature 门控)

源码本就用,VM 运行时依赖,**不受** feature 门控。**唯一真重复**:`field.rs` 与 `invoke.rs` 各自私有定义的
常量池解析三函数(utf8/class_name/name_and_type),逐字相同(仅 `BadConstant` 消息文案因上下文略异)。
经源码核实(`invoke.rs:988-1014`、`field.rs:48-74`),提取这三函数:

```rust
pub(crate) fn utf8(cp: &ConstantPool, idx: u16) -> Result<String, VmError>;
pub(crate) fn class_name(cp: &ConstantPool, idx: u16) -> Result<String, VmError>;
pub(crate) fn name_and_type(cp: &ConstantPool, idx: u16) -> Result<(String, String), VmError>;
```

挂点:`src/runtime/interpreter/mod.rs` 加 `pub(crate) mod cp_util;`(line 17 `type_check` 后)。
`field.rs`/`invoke.rs` 删各自私有定义,改 `use super::cp_util::*`。

**不提取**(经源码核实,**非**跨模块重复——早期扫描报告误判,此处更正):
- `invoke.rs:695` `value_to_slot` / `invoke.rs:923` `slot_to_value`——定义与**所有调用**均在 `invoke.rs` 内部,
  非 DRY 重复;留原处,避免无收益的跨文件搬家(YAGNI)。
- `jdk_internal_reflect.rs:188` `unbox_arg`——独立语义(装箱 `Reference`→`Slot` 拆箱,非 Value↔Slot 转换),
  单点使用,不动。
- `invoke.rs:1017` `cp_utf8`(零分配 `&str` 版,栈轨迹用)、`invoke.rs:935` `arg_to_slot`——invoke 专用,不重复。

### 3.2 测试基础设施 → `src/testkit/`(feature 门控)

VM 运行时不用(不编译 Java 源、不调 javac),仅 tests 用。整模块 `#[cfg(any(test, feature = "testkit"))]`。

`src/lib.rs`:
```rust
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
```

`Cargo.toml`:
```toml
[features]
testkit = []

[dev-dependencies]
rustj = { path = ".", features = ["testkit"] }
```

**feature 机制说明**:`#[cfg(test)]` 对 `tests/` 集成测试**无效**(它只对 src 内单元测试 + `cargo test --lib`
激活;集成测试是独立 crate,链接 lib 的普通构建)。故用 feature + dev-dependencies **自引用**:
`cargo test` 编译 `tests/*.rs` 时,Cargo 启用 `[dev-dependencies]` 声明的 features → testkit feature
开 → `#[cfg(any(test, feature="testkit"))]` 命中 → 模块可见;`cargo build`/release 不启用 dev-deps
feature → testkit 不编译(release 净)。

**实现第一步必须验证此机制**(edition 2024 + 当前 cargo 版本)。若 dev-deps 自引用不生效,回退序:
1. `cargo test --features testkit`(TDD 命令带参),并在 CLAUDE.md §4 注明;或
2. 退而求其次:testkit 无条件 `pub mod`(release 带 dead code,但 VM 不调用,功能无害)。

---

## 4. `src/testkit/` 模块与 API

```
src/testkit/
├── mod.rs     pub use 子模块(*)
├── env.rs     javac_available()/find_javabase_jmod() + require_javac!()/require_javabase!()
├── compile.rs compile()/compile_dir()/compile_and_load()/load_dir()
├── runner.rs  run()/run_result()/run_err()/run_static_in()/run_static_int()
├── lookup.rs  find_method()/utf8()
├── args.rs    Arg + set_args()
└── asserts.rs as_int/as_long/as_double + assert_int!.../assert_throws!/assert_is_thrown!
```

### 4.1 env.rs(环境探测 + 守卫宏)
- `pub fn javac_available() -> bool` — 原样(`Command::new("javac").arg("-version")…`)。
- `pub fn find_javabase_jmod() -> Option<PathBuf>` — 原样(扫 `jdk-25.0.2/24/21/17/11.0.30` + `JAVA_HOME`)。
- 宏 **`require_javac!()`** → `if !javac_available() { eprintln!("跳过:未找到 javac"); return; }`
  (early-return,**函数做不到**)。
- 宏 **`require_javabase!($var)`** → `let Some($var) = find_javabase_jmod() else { eprintln!("跳过:无 java.base.jmod"); return; };`

### 4.2 compile.rs(分层变体;目录名统一 `rustj-test-{name}-{seq}-{pid}`)
- `pub fn compile(src: &str, name: &str) -> PathBuf` — 编单类,返回 `.class` 路径。
- `pub fn compile_dir(src: &str, name: &str, extra: &[&str]) -> PathBuf` — 编到唯一目录,支持
  `--add-exports`,返回目录(供多 `.class` / 带 `extra` 场景)。
- `pub fn compile_and_load(src: &str, name: &str) -> ClassRegistry` — 编多类 + 全部 `.class` 载入 registry。
- `pub fn load_dir(reg: &mut ClassRegistry, dir: &Path)` — 把目录所有 `.class` 载入(供 `compile_dir` 后组合)。
- `static SEQ: AtomicU64` **内化**在 compile.rs(消除 26 处各文件 SEQ / COMPILE_SEQ)。
- 编译失败 `assert!(... status.success(), "javac 失败:\n{}", stderr)`。

### 4.3 runner.rs(分层变体;入口 Interpreter 统一带 `.with_exception_table`)

**两层**(语义不同,命名区分,**不可混用**):
- **高层**(经 `VmThread` + `interpret_with`,主流 59 文件;走完整 VM 语义——`<clinit>`/异常表/堆):
  - `pub fn run(reg: &Arc<ClassRegistry>, class, name, desc) -> Value` — 自建 VmThread,异常 panic。
  - `pub fn run_result(reg, class, name, desc) -> (Result<Value, VmError>, VmThread)` — 保留结果 + vm(供读堆上异常)。
  - `pub fn run_err(reg, class, name, desc) -> VmError` — 期望失败。
  - `pub fn run_static_in(vm: &mut VmThread, class, name, desc) -> Result<Value, VmError>` — **复用调用方 VmThread**
    (守静态字段句柄同堆约束——见 `real_integer.rs` 注释:静态字段值是 Vm 堆句柄,堆随 Vm 析构失效)。
  - `pub fn run_static_int(vm: &mut VmThread, class, name) -> Result<i32, VmError>` — 高层便利 int 版(解 `Value::Int`)。
- **低层**(不经 VmThread,直接 `Frame` + `Interpreter::interpret`,3 文件 `interpret_int_methods`/
  `interpret_method_invocation`/`object_fields`;只测纯指令算术,无 `<clinit>`/堆/异常表):
  - `pub fn run_raw_int(cf: &ClassFile, name, desc, args: &[i32]) -> i32` — 喂 Frame 给 `Interpreter::interpret`,解 `Value::Int`。
  - `pub fn run_raw_value(cf: &ClassFile, name, desc, args: &[Arg]) -> Value` — 按 `Arg` 槽位写 locals(见 args.rs)。
  - **命名约定**:`_raw` 后缀 = 不经 VmThread(区别于高层);迁移时 `interpret_int_methods.rs` 原
    `run_static_int`(低层)→ `run_raw_int`、`run_static_value` → `run_raw_value`。

### 4.4 lookup.rs
- `pub fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo` — 统一用 `matches!` 变体。
- `pub fn utf8(cf: &ClassFile, idx: u16) -> String` — 测试侧 **panic 版**(区别于 `cp_util::utf8` 的 `Result` 版;
  测试断言失败直接 panic 合理)。

### 4.5 args.rs
- `pub enum Arg { I(i32), L(i64), F(f32), D(f64) }`。
- `pub fn set_args(frame: &mut Frame, args: &[Arg])` — 按 JVM 槽位约定(`I`/`F`=1 槽,`L`/`D`=2 槽)写 locals。
- 仅供**低层** `run_raw_value` 用(高层经 VmThread 的调用不走此路)。

### 4.6 asserts.rs
- `pub fn as_int/as_long/as_double/as_float(v: Value) -> T` — panic 版取值。
- 宏 **`assert_int!(v, n)`** / `assert_long!` / `assert_double!` / `assert_float!` — 断言 Value 类型 + 值
  (`float`/`double` 用 `abs < eps`)。
- 宏 **`assert_throws!(result, vm, "cls/name")`** — 断言 `Err(VmError::ThrownException(r))` + 堆对象 `class_name()` 匹配。
- 宏 **`assert_is_thrown!(err)`** — 断言 `VmError` 是 `ThrownException` 变体。

---

## 5. 迁移计划(一次性全量,两 commit)

### Commit 1 — 基建层(先验证 API)
1. `Cargo.toml` 加 `testkit` feature + `[dev-dependencies] rustj = { path=".", features=["testkit"] }`;
   **验证** `cargo test` 是否自动开 testkit(决定 §3.2 走无参 vs `--features testkit`)。
2. 新建 `src/runtime/interpreter/cp_util.rs`,挂 `interpreter/mod.rs`;`field.rs`/`invoke.rs`/
   `jdk_internal_reflect.rs` 删私有定义改用 `cp_util`。
3. 新建 `src/testkit/{mod,env,compile,runner,lookup,args,asserts}.rs`,挂 `lib.rs`
   (`#[cfg(any(test, feature="testkit"))] pub mod testkit;`)。
4. 迁移 2 个代表文件(`clinit.rs` + `throw.rs`)验证 API 可用。
5. 闸门:`cargo test`(或 `--features testkit`)全绿 + `cargo clippy` 净。

### Commit 2 — 全量迁移层
6. 其余 62 个 `tests/*.rs` 全量迁移:删私有辅助 → `use rustj::testkit::*;`;守卫 →
   `require_javac!()` / `require_javabase!()`;断言 → 宏。
7. 按主题分组逐组跑测试(字节码 / 异常 / 集合 / 反射 / 线程 / 模块 / 文件系统 …),每组绿再下一组。
8. 闸门:`cargo test`(全绿)+ `cargo clippy --all-targets -- -D warnings` 净 + 零 unsafe 保持 + 手写 4 空格(不跑 cargo fmt)。

---

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| dev-deps 自引用 feature 不生效(edition 2024 / 新 cargo) | Commit 1 第 1 步验证;回退 `--features testkit` 或无条件 `pub mod` |
| 一次性迁移 62 文件易漏 / 误改 | Commit 1 先验证 API;Commit 2 按主题分组逐组跑测试 |
| `run_static_in` 同堆约束被破坏 | runner.rs 文档注明约束;迁移涉及类(`real_integer`/`arraylist`/`hashmap` 等)重点测 |
| `cp_util` 提取改变 `field.rs`/`invoke.rs` 语义 | 提取 = 原样搬运,不改逻辑;跑 clinit/throw/invoke 相关测试验证 |
| 宏 panic 消息丢失诊断信息 | 宏消息保留上下文(类名 / 方法名 / 期望值 / 实际值) |
| 测试侧 `utf8`(panic)与 `cp_util::utf8`(Result)混淆 | 文档注明分工:测试 panic 版 vs VM Result 版;命名/路径区分 |
| 低层 `_raw` 与高层(run*)API 混用 | `_raw` 后缀命名区分;迁移时按原文件用法对号(纯指令→低层、带 VM→高层) |

---

## 7. 验证闸门(完成判据)

- `cargo test --features testkit`(或无参,视 §3.2 验证)全绿。
- `cargo clippy --all-targets -- -D warnings` 净。
- `#![deny(unsafe_code)]` 保持(`testkit`/`cp_util` 零 unsafe)。
- 手写 4 空格缩进,**不跑** `cargo fmt`。
- 重复样板消除:`javac_available`/`compile*`/`run*`/`find_method`/`utf8`/`as_int`/`find_javabase_jmod`/
  `SEQ`/`Arg` 各仅 `testkit` 一处定义;`utf8`/`class_name`/`name_and_type`/`value_to_slot`/`slot_to_value`
  各仅 `cp_util` 一处定义。
- 行为逐位保持:所有原有 `#[test]` 仍绿(语义零变化)。
