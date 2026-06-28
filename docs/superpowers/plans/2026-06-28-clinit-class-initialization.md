# Layer 4.9:`<clinit>` 类初始化 — 实现计划

> spec:`2026-06-28-clinit-class-initialization-design.md`。TDD 红先。

## 阶段 A:数据 — `InitState` + `LoadedClass` 状态

1. `src/oops/klass.rs`:
   - 定义 `pub enum InitState { NotStarted, InProgress, Done, Failed }`(`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`)。
   - `LoadedClass` 加字段 `init_state: RefCell<InitState>`。
   - `from_cf` 初始化 `init_state: RefCell::new(InitState::NotStarted)`。
   - 访问器 `pub fn init_state(&self) -> InitState` 与 `pub fn set_init_state(&self, s: InitState)`。
2. `src/oops/mod.rs`:`pub use klass::InitState;`(若 mod 已 re-export klass 项,顺其模式)。
3. 单测(klass.rs tests):`init_state_defaults_not_started`;`set_init_state_round_trips`。

**闸门:** `cargo test --lib oops` 绿。

## 阶段 B:引导桩补充

1. `src/oops/bootstrap.rs::BOOTSTRAP_HIERARCHY` 追加:
   `("java/lang/LinkageError", Some("java/lang/Error"))`、
   `("java/lang/ExceptionInInitializerError", Some("java/lang/LinkageError"))`、
   `("java/lang/NoClassDefFoundError", Some("java/lang/LinkageError"))`。
2. 扩 `install_bootstrap_loads_standard_hierarchy` 断言列表含三者。

**闸门:** `cargo test --lib bootstrap` 绿。

## 阶段 C:`interpreter/clinit.rs` — `ensure_class_initialized` + `run_clinit`

1. 新建 `src/runtime/interpreter/clinit.rs`:
   - `find_clinit(cf: &ClassFile) -> Option<&CodeAttribute>`:按名 `<clinit>` + 描述 `()V` 找方法,取其 `code`(无则 None)。
   - `fn run_clinit(lc: &LoadedClass, vm: &mut Vm<'_>) -> Result<(), VmError>`:无 `<clinit>` → `Ok`;否则建 `Frame`(max_locals/max_stack)、`Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table)`,`run_with_depth(vm, |vm| interp.interpret_with(&mut frame, vm))?;`。
   - `pub(crate) fn ensure_class_initialized(vm: &mut Vm<'_>, class_name: &str) -> Result<(), VmError>`:按 spec §4/§5。
   - `fn is_init_failure_class(vm: &Vm<'_>, exc: Reference) -> bool`:堆读类名 ∈ {EIIE, NCDFO}。
2. `src/runtime/interpreter/mod.rs`:`mod clinit;`。
3. 单测(clinit.rs tests,合成 ClassFile,无需 javac):`clinit_runs_and_sets_static` ——
   CP/fields/methods 合成 `Cls`(static `v:I`、`<clinit>`=`iconst_5;putstatic #fieldref;return`)→
   `ensure_class_initialized(&mut vm, "Cls")` 后 `lc.static_storage.borrow()[0] == Slot::Int(5)`。

**闸门:** `cargo test --lib clinit` 绿(此为 TDD 主红→绿锚点)。

## 阶段 D:触发点接入(4 处)

1. `field::new_instance`:`resolve_class_name` 后、`registry.get` 前,插 `clinit::ensure_class_initialized(vm, &class_name)?;`。
2. `field::get_static` / `field::put_static`:`resolve_fieldref` 得 `class_name` 后、`require_class` 前,插同名调用。
3. `invoke::invoke_static`:`resolve_methodref` 得 `class_name` 后、`vm.registry()` 前,插同名调用。
4. 各文件 `use super::clinit;`(或全路径)。

**闸门:** `cargo build` + `cargo test`(全量)绿。

## 阶段 E:集成闸门(`tests/clinit.rs`,javac)

复用 `string_literals.rs` 的 `compile_and_load`/`find_method`/`run`/`as_int` 模式。
新增 `assert_throws_class`(取 `Err(ThrownException)` → 堆读类名断言)。
源 `ClinitGate`(+ `Base`/`Sub`/`Bad`)覆盖 spec §8 的 6 项。缺 javac 则跳过。

**闸门:** `cargo test --test clinit` 绿;`cargo clippy --all-targets` 零告警;`#![deny(unsafe_code)]` 仍成立。

## 阶段 F:提交 + memory

- 提交 1(docs):spec + plan。
- 提交 2(feat):阶段 A–E 实现 + 闸门。
- 更新 memory:路线图步骤 2 ✅、4.9 ✅ 条目、下一步(步骤 3 native 绑定 / 4 jmod-jar)。
