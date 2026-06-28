# Layer 4.8 计划:`ldc String` + 字符串 intern 池

> 日期:2026-06-28
> 设计:[2026-06-28-ldc-string-pool-design.md](../specs/2026-06-28-ldc-string-pool-design.md)
> 节奏:RED(看红)→ GREEN(看绿)→ javac 闸门 → 提交(待用户确认)
> 全程:`#![deny(unsafe_code)]`,手写 4 空格缩进(勿 `cargo fmt`),`Co-Authored-By: Claude Opus 4.8` 收尾

## 阶段 A —— 数据层(RED → GREEN)

**A1. `src/oops/string.rs`**(新)+ `src/oops/oop.rs`(加变体)+ `src/oops/mod.rs`(pub)
- RED:`StringOop::new` / `text()` 单测;`Oop::String(StringOop::new("hi".into()))` 构造 + 取回。
- GREEN:`StringOop{ text: String }` + `new`/`text()` + `derive(Debug,Clone,PartialEq)`;`Oop` 增 `String(StringOop)`。
- ⚠ 加变体立即触发下游 `match Oop` 编译错误 —— 这是预期的;阶段 C 逐一补臂。

**A2. `src/runtime/string_pool.rs`**(新)+ `src/runtime/mod.rs`(pub)
- RED:`intern` 首次分配(id 递增)、二次同引用(`==`)、不同文本不同引用(`!=`)。
- GREEN:`StringPool{ table: HashMap<String,Reference> }` + `intern(&mut self, heap: &mut Heap, text: &str) -> Reference`。
- 借 `Heap` 与 `Reference`(同 crate);不碰 `oops` 之外的环。

## 阶段 B —— Vm 接入

**B1. `src/runtime/vm.rs`**
- `Vm` 增 `string_pool: StringPool` 字段;`new`/`default` 初始化 `StringPool::new()`。
- `pub fn intern_string(&mut self, text: &str) -> Reference { self.string_pool.intern(&mut self.heap, text) }`(split-borrow 不相交字段)。
- 无独立单测(被 ldc 臂间接覆盖);保证 `cargo build` 过。

## 阶段 C —— 解释器 ldc 臂 + 穷尽 match 补臂

**C1. `src/runtime/interpreter/mod.rs`**
- 抽私有 `load_constant(&self, frame, vm, index) -> Result<(), VmError>`:`Integer`→push_int / `Float`→push_float / `String{string_index}`→解析 Utf8→`vm.intern_string`→push_reference / 其余→`BadConstant`。
- 改 `Ldc` 臂调 `load_constant`,`pc += 2`;**新增 `LdcW` 臂**调 `load_constant`,`pc += 3`。
- RED:`ldc String` 压引用(查堆文本);`ldc_w String`;`ldc_w Integer`(补缺)。
- 需 `vm: &mut Vm` 进 `load_constant` —— `dispatch` 已有 `vm`,OK。

**C2. 各 `match Oop` 补 `String` 臂**(编译器驱动,逐文件):
- `type_check.rs::object_type` → `(false, Some("java/lang/String".into()))`。
- `invoke.rs` `invoke_virtual`/`invoke_interface` runtime_class → `Err(BadConstant("invoke 目标为 String(顺延)"))`。
- `array.rs` `array_load`/`array_store` → 并入既有"非数组"错误臂(`BadConstant`)。
- `field.rs` `get_field`/`put_field` → `Err(BadConstant("String 非实例字段目标"))`。
- `heap.rs` 测试 → `panic!("期望实例/数组")`。
- 其余编译器报出者同处理。

**GREEN 判据**:`cargo build` 干净;阶段 A/C 的 RED 测试转绿。

## 阶段 D —— javac 集成闸门

**D1. `tests/string_literals.rs`**(新)
- 复用既有 `compile_and_load`/`run` 模式(参考 `throw.rs`)。
- javac 编 `StringGate`:
  1. `return "hello"` → 查堆 `Oop::String` 文本 == `"hello"`。
  2. `return "x" == "x"` → `Int(1)`(if_acmpeq 经 intern 身份)。
  3. `String a="x",b="x"; return a==b?1:0` → `Int(1)`。
  4. `return "a"=="b"` → `Int(0)`。
- helper:`run_ref` 返回 `(Value, &Vm)` 或运行后查堆读文本。
- 无 javac 则跳过(沿用 `javac_available()`)。

## 阶段 E —— 收尾

- `cargo test`(全绿)+ `cargo clippy --all-targets -- -D warnings`(零告警)。
- 更新 memory 路线图(`hotspot-rust-migration-project.md`):4.8 ✅ 条目 + stdlib 前置 1 勾除。
- **提交前待用户确认**(既定闸门)。建议两提交:① docs(spec+plan);② feat(test) 实现+闸门。或合一——届时问。

## 文件清单

新增:`src/oops/string.rs`、`src/runtime/string_pool.rs`、`tests/string_literals.rs`、
`docs/superpowers/specs/2026-06-28-ldc-string-pool-design.md`、
`docs/superpowers/plans/2026-06-28-ldc-string-pool.md`。
改动:`src/oops/{oop,mod}.rs`、`src/runtime/{mod,vm}.rs`、
`src/runtime/interpreter/{mod,type_check,invoke,array,field}.rs`、`src/runtime/heap.rs`(测试)。

## 风险与回退

- 加 `Oop::String` 变体是单向改动;补臂均为"并入既有错误路径",不改既有行为。
- intern 表放 Vm(非注册表)—— 注册表以不可变借用持,运行时无法 `&mut` 追加,intern 需随堆可变 → Vm 是正确归属。
- 若 javac 对 `"x"=="x"` 生成非常量折叠(直接 `iconst_1`):用局部变量承载(`a`/`b`)的用例 #3 保证走 `ldc`+`if_acmpeq`,锁定 intern 语义。
