# Layer 4.2b 设计:接口分派 + invokespecial 完整语义 + StackOverflowError

> 2026-06-20 · 对应 HotSpot `LinkResolver::resolve_interface` / `resolve_special` /
> `interpreter/zero/bytecodeInterpreter` 的 `CASE(_invokeinterface)` /
> `CASE(_invokespecial)` 与线程栈深度检查。承接 [4.2 虚分派](2026-06-20-virtual-dispatch-design.md)。

## 1. 目标

执行使用**接口 + default 方法 + 私有方法 + super 调用**的真实 Java 程序,且能**优雅检测无限递归**
(返回 `StackOverflowError`,不 panic),结果与 JVM 一致:

```java
interface Shape { int kind(); default String tag() { return "shape:" + kind(); } }
class Circle implements Shape { public int kind() { return 1; } }   // 接口多态 + default method
class Base { int v() { return 1; } private int p() { return 9; } }
class Sub extends Base { int v() { return super.v() + 10; } int peek() { return p(); } }
// new Circle().tag()        ← invokeinterface: 虚派到 Circle.kind,tag 落到接口 default
// new Sub().v() == 11       ← invokespecial super.v(): 落到 Base.v 而非 Sub.v
// new Sub().peek() == 9     ← invokespecial private: 精确命中 Base.p
// f() { return f(); }       ← StackOverflowError(深度计数,非 panic)
```

## 2. 范围

**包含(本增量):**
- `invokeinterface`(0xb9)搜索式分派:按对象运行时类沿超类链查找;类链落空时沿**传递实现的接口**
  BFS 找带 `Code` 的 **default 方法**;操作数 5 字节(`index, count, 0`),`count` 冗余丢弃。
- `invokespecial`(0xb7)完整三支:`<init>` 精确(4.1 已有)/ 私有方法精确 / `super.m()` 虚查。
- `StackOverflowError`:`Vm` 深度计数,每个 invoke 进入 +1 / 退出 −1,超限 → `VmError::StackOverflow`。
  递归调用栈仍用 Rust 栈(不做迭代帧循环重构)。
- `VmError::AbstractMethodError`:invokeinterface 命中抽象方法(code=None)时报。

**不含(后续增量):**
- 真实 itable / vtable 表(O(1) 分派优化,留待 JIT 层;本增量用线性查找,与 4.2 invokevirtual 一致)。
- 显式迭代帧循环(本增量用 Rust 调用栈;深度计数取得**可观测的 SOE**,迭代化是内部重构,独立增量)。
- `IncompatibleClassChangeError`(对象非接口实例的链接期校验)。
- `athrow` / 异常表处理(独立异常层)。
- `ACC_SUPER` 未置位的历史遗留分支(所有现代类均置位,不做)。
- default 方法的**最具体解析**(JLS §5.4.3.3 菱形冲突选最具体)—— 见 §4.3 已知简化。

## 3. 核心不变量:三类分派的判定

| 指令 | objref | 分派依据 |
|---|---|---|
| `invokevirtual` | 运行时类 | `find_virtual_method(runtime_class, …)`(4.2 既有,沿超类链) |
| `invokeinterface` | 运行时类 | 类链先行(复用 `find_virtual_method`);落空 → `find_default_method`(接口闭包 BFS) |
| `invokespecial` `<init>` | 精确 | Methodref 声明类内 `find_method`(4.1 既有) |
| `invokespecial` 私有 | 精确 | Methodref 声明类内 `find_method`(私有不可继承,无需虚查) |
| `invokespecial` super | 声明类 | `find_virtual_method(declared_class, …)`(声明类 = 调用者直接超类,上行虚查) |

**关键洞见**:`invokeinterface` 的**类链先行**复用 4.2 的 `find_virtual_method`——对象以接口类型调用时,
实现在类链上即可命中;仅当类链落空(类未覆盖、仅接口有 default)才进入接口搜索。

## 4. 表示变更

### 4.1 `LoadedClass`(`oops/klass.rs`)

新增访问器(无新字段——接口名已在 `cf.interfaces`,仅需解析 CP 索引):

```rust
/// 直接实现的接口内部名(由 cf.interfaces 的 Class 条目解析)。
fn interface_names(&self) -> Vec<String>
```

`cf.interfaces` 是 `Vec<u16>`(CP → `Class{name_index}` → `Utf8`),解析同 `this_class_name`。

### 4.2 `ClassRegistry` 新增方法

```rust
/// 接口 default 方法查找:沿 class_name 类层次的所有传递实现接口 BFS,
/// 找首个带 Code 的 (name, desc) → (声明接口类, 方法)。类链已查过,此仅兜底 default。
fn find_default_method(&self, class_name, name, desc) -> Option<(&LoadedClass, &MethodInfo)>

/// invokespecial 非分支判定辅助:在 declared_class 内精确查找 (name, desc) 方法 → (类, 方法)。
/// 用于判定"私有精确"分支与提供 super 虚查的起点。
fn find_exact_method(&self, class_name, name, desc) -> Option<(&LoadedClass, &MethodInfo)>
```

`find_virtual_method`(4.2 既有)不改,被 invokeinterface 类链先行与 invokespecial super 复用。

### 4.3 default 方法解析算法(`find_default_method`)

> **设计决策**:采用**实用 BFS**(选 A),不实现 JLS 最具体解析。

1. 收集 `class_name` 及其整条超类链上每类的直接接口名 → 初始队列(去重)。
2. BFS:取队首接口 `I`,查注册表:
   - 命中带 `Code` 的 (name, desc) 方法 → 返回 `(I_lc, method)`。
   - 否则把 `I` 的超接口(`I.interface_names()`)入队(去重)。
   - 抽象方法(code=None)不返回,继续 BFS。
3. 队列空 → `None`(调用方报 `AbstractMethodError`)。

**已知简化**(文档注明,非 bug):菱形继承下多个接口提供同名 default 时,本算法取 BFS 首个,
不保证 JLS "最具体";真实代码单层 default 占绝大多数,不影响常见正确性。

### 4.4 `Vm`(`runtime/vm.rs`)深度计数

```
Vm<'a> {
    heap: Heap,
    registry: Option<&'a ClassRegistry>,
    frame_depth: u32,   // 新增:当前嵌套帧数
}
```

- `pub const STACK_DEPTH_LIMIT: u32 = 1024;`(典型 -Xss 量级;正常小测试不会误触)。
- 访问器 `frame_depth()` / `frame_depth_mut()`。
- `Default` 仍可用(`frame_depth: 0`);`Vm::new` 不变。
- 守卫:
  ```rust
  /// 进入一帧(深度 +1,闭包返回后 −1);超限 → StackOverflow。
  fn with_stack_depth<R>(&mut self, f: impl FnOnce(&mut Self) -> Result<R, VmError>) -> Result<R, VmError> {
      if self.frame_depth >= STACK_DEPTH_LIMIT { return Err(VmError::StackOverflow); }
      self.frame_depth += 1;
      let r = f(self);
      self.frame_depth -= 1;
      r
  }
  ```
  关键:`f(self)` 先于 `?` 执行完毕,故 Ok/Err 两路均 −1。

### 4.5 `VmError`(`runtime/interpreter/mod.rs`)新变体

```rust
/// AbstractMethodError:invokeinterface 命中抽象方法(无 Code)。
AbstractMethodError,
/// StackOverflowError:帧嵌套深度超 STACK_DEPTH_LIMIT。
StackOverflow,
```
+ `Display` 两支。

## 5. 指令语义

### `invokeinterface`(0xb9)
1. 解析 Methodref → `(declared_iface, name, desc)`。`declared_iface` 仅校验/报错,**不参与分派**。
2. 按描述符逆序弹 args,再弹 objref;objref null → `NullPointer`。
3. 取运行时类(owned `String`)。
4. `find_virtual_method(runtime_class, name, desc)`
   → 未命中则 `find_default_method(runtime_class, name, desc)`
   → 仍无 → `AbstractMethodError`。
5. 命中方法 `code` 为 None(抽象)→ `AbstractMethodError`。
6. 构造被调用者帧:`local[0] = objref`,args 其后(`Arg`/`store_arg`)。
7. `vm.with_stack_depth(|vm| callee_interp.interpret_with(&mut callee, vm))?` → 按返回类型回填。
8. **`pc += 5`**(5 字节:`index(2) + count(1) + 0(1)`;count 丢弃)。

### `invokevirtual`/`invokestatic`(改动:深度守卫)
在各自递归 `interpret_with` 外包 `vm.with_stack_depth(...)`,其余语义不变,`pc += 3`。

### `invokespecial`(0xb7,扩三支)
1. 解析 Methodref → `(declared_class, name, desc)`,弹 args、弹 objref(null → `NullPointer`)。
2. 分支:
   - `name == "<init>"`:声明类已加载 → `find_exact_method(declared_class, name, desc)` 运行;
     未加载根类(Object)且 `()V` → 空操作(4.1 既有,保留)。
   - 否则 `find_exact_method(declared_class, name, desc)`:
     - 命中且 `ACC_PRIVATE` → 精确运行(私有不可继承)。
     - 否则(super 调用)→ `find_virtual_method(declared_class, name, desc)`(声明类=调用者超类,上行虚查)。
3. `vm.with_stack_depth(...)` 递归 → 回填;`pc += 3`。

## 6. 借用要点

沿用 4.1/4.2 的 `'a` 模式:`Vm::registry()` 返回 `Option<&'a ClassRegistry>`(与 `&self` 借用解耦)。
取运行时类时 `heap.get()` 的不可变借用取出 `class_name`(owned `String`)即释放,随后 `&mut vm`
递归 + 深度守卫无冲突。`with_stack_depth` 取 `&mut self`,在 `registry()`(返回 `'a`)之外,无重叠。

## 7. 模块布局

- `oops/klass.rs`:`LoadedClass::interface_names()`;`ClassRegistry::find_default_method` /
  `find_exact_method`。
- `runtime/vm.rs`:`frame_depth` 字段 + `STACK_DEPTH_LIMIT` + `with_stack_depth`。
- `runtime/interpreter/invoke.rs`:`invoke_interface`(新);`invoke_special` 扩三支;四 invoke 函数包
  `with_stack_depth`。
- `runtime/interpreter/mod.rs`:`Opcode::Invokeinterface` 分派臂(`pc += 5`);`VmError::AbstractMethodError` /
  `StackOverflow` + Display。
- `tests/interface_dispatch.rs`(新):javac 编译接口 + default + 私有 + super 层次,真实执行。

## 8. 测试策略

**单元**(klass.rs / vm.rs):
- `interface_names_resolves_cp_class_entries`:手构常量池 + interfaces,验证解析为接口内部名。
- `find_default_method_finds_interface_default`:接口带 default,类未覆盖 → 命中接口。
- `find_default_method_skips_abstract_keeps_searching`:接口抽象、超接口 default → BFS 命中超接口。
- `find_default_method_returns_none_when_all_abstract`:全抽象 → None。
- `with_stack_depth_increments_and_decrements`:对称(进入 +1、退出 −1,Ok/Err 两路)。
- `with_stack_depth_overflow_at_limit`:逼近 LIMIT 再调用 → `StackOverflow`。

**集成**(执行闸门 `tests/interface_dispatch.rs`):javac 编译 `Shape` 接口(`kind()` 抽象 + `tag()` default)
+ `Circle`/`Square` 实现 + `Base`/`Sub`(`v()` + 私有 `p()` + `super.v()`),真实执行:
- 接口多态:对象以接口类型 invokeinterface → 虚派到各自 `kind()`。
- default method:类未覆盖 `tag()` → 落到接口默认实现(其内 invokeinterface `kind()` 虚派到实现)。
- 私有 invokespecial:精确命中声明类私有方法,不被子类同名干扰。
- `super.v()`:落到超类实现而非子类重写。
- 深递归无限方法 `rec()`(`return rec();`)→ `VmError::StackOverflow`(不 panic)。
- 接口方法对象为 null → `NullPointer`。
结果与 JVM 一致。

## 9. HotSpot 对照

| rustj(4.2b) | HotSpot |
|---|---|
| `find_virtual_method` 类链先行 | `LinkResolver::resolve_interface` → `InstanceKlass::method_at_itable`(我们退化为类链线性 + 接口 BFS,不做 itable 表) |
| `find_default_method` 接口闭包 BFS | itable 查找 + DefaultMethods 解析(我们用线性 BFS,最具体简化) |
| `invokevirtual` on private 亦可 | 现代字节码私有方法可能 invokevirtual,虚查因不可重写等效 |
| `frame_depth` 计数 + `STACK_DEPTH_LIMIT` | `JavaThread` 栈基址 + `_stack_overflow_limit` 检查(我们用计数,不做 guard page) |
| `invokeinterface` `count` 操作数丢弃 | HotSpot 亦仅在 `-Xcheck:jni` 校验 count,运行时不依赖 |

## 10. 构建序(TDD)

1. `vm.rs`:`frame_depth` + `STACK_DEPTH_LIMIT` + `with_stack_depth`,单元红→绿。
2. `klass.rs`:`interface_names()` + `find_default_method` + `find_exact_method`,单元红→绿。
3. `invoke.rs`:`invoke_interface`;`invoke_special` 扩三支;四函数包 `with_stack_depth`。
   `mod.rs`:`Invokeinterface` 臂 + 两个 `VmError` 变体 + Display。
4. `tests/interface_dispatch.rs` 集成闸门(红→绿)。
5. clippy `--all-targets -- -D warnings`、零 unsafe(`#![deny(unsafe_code)]` 不开窗)、全测试绿;提交。
