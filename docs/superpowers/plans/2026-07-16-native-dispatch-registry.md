# Native 分派重构:NativeRegistry + `natives!` 宏 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 Layer 4.10c 的编译期 `match (class,name,desc)` 静态 native 分派表,换成数据驱动的 `NativeRegistry`(fn 指针表 + `register`/`resolve` upsert API)+ 声明式 `natives!` 宏(非捕获闭包协变 fn 指针),行为逐位保持,并预留运行时注册口供 4.16 `JNI_RegisterNatives` 接入。

**Architecture:** 新建 `NativeRegistry`(两层 `HashMap<String, Vec<NativeEntry>>`,零分配按 `&str` 查),放 `Vm` 单例 `RwLock` 后,在 `Vm::new` 一次性 `register_all` 填充(故 `VmThread::new`/`default` 构造即带满表,现有单测零改动)。`natives!` 宏把每模块的 `match` 臂折叠成 `pub(super) fn register(&mut NativeRegistry)`,闭包体在 `register(..., f: NativeFn)` 位协变为零成本 fn 指针。迁移用**渐进 fallback**:`invoke_inner` 先查 registry、miss 走旧 `dispatch` 前缀路由;每迁一个模块 = 加其 `register` + 从路由删其臂 + 删其 `dispatch`;全部迁完后再删 `dispatch` 收尾。零依赖、零 unsafe、不跑 `cargo fmt`、手写 4 空格。

**Tech Stack:** Rust edition 2024(`#![deny(unsafe_code)]`)、`std::sync::RwLock`、declarative `macro_rules!`。

**Spec:** `docs/superpowers/specs/2026-07-16-native-dispatch-registry-design.md`

**迁移规则模板(本计划反复使用,先记住):**
一个模块的旧 `dispatch` 形如
```rust
pub(super) fn dispatch(vm: &mut VmThread, class: &str, name: &str, desc: &str,
                       this: Option<Reference>, args: &[Value]) -> Result<Value, VmError> {
    match (class, name, desc) {
        ("pkg/Cls", "meth", "()I") => <body>,          // body 是 Ok(..)/Err(..) 表达式
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}
```
迁移后(整个 `dispatch` 删掉,换成宏):
```rust
natives! {
    ("pkg/Cls", "meth", "()I") => |vm, this, args| <body>;
}
```
**变换规则**:`match` 的每个 `($class, $name, $desc) => <body>` 臂 → `($class, $name, $desc) => |vm, this, args| <body>;`。`<body>` 逐字不动(它本就是返回 `Result<Value,VmError>` 的表达式)。`_ => ULE` 臂丢弃(未登记由 `invoke_inner` 的 miss 路径统一抛)。`this`/`args` 形参:原来叫 `_this`/`_args` 的模块,闭包也用 `_this`/`_args`(保持警告净);若体里用了 `this`/`args`,闭包用 `this`/`args`。模块顶 `use super::super::throw_exception;` 等导入保留(体里仍调)。

---

## 文件结构

| 文件 | 职责 | 本计划操作 |
|------|------|-----------|
| `src/runtime/interpreter/native/registry.rs` | `NativeFn`/`NativeRegistry`/`NativeEntry` + 单测 | **新建**(Task 1) |
| `src/runtime/interpreter/native/mod.rs` | `invoke`/`invoke_inner`(registry→dispatch fallback)、`natives!` 宏、`register_all`、`dispatch`(渐进删臂)、模块 helpers(`is_primitive_name`/`class_arg_name` 保留) | 改(Task 2,3,11) |
| `src/runtime/interpreter/native/{sun_nio_fs,jdk_internal_loader,java_io,java_lang_invoke,jdk_internal,jdk_internal_reflect,java_lang}.rs` | 各包 native 实现 | 逐个迁移(Task 4–10) |
| `src/runtime/vm.rs` | `Vm` 加 `native_registry` 字段 + `Vm::new` 填充 + `VmThread::native_resolve` + sync 断言 | 改(Task 3) |

`invoke.rs` 的 6 个 `native::invoke(...)` 调用点(行 582/1166/1358/1678/1764/1858/1947)**零改动**——`native::invoke` 签名保持。

---

## Task 1: `NativeRegistry` 类型与核心 API

**Files:**
- Create: `src/runtime/interpreter/native/registry.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`(加 `mod registry;` + 重导出,行 32 后)

- [ ] **Step 1: 写失败测试(新建 registry.rs,先只放测试)**

新建 `src/runtime/interpreter/native/registry.rs`,内容:
```rust
//! Native 方法 fn 指针注册表(Layer 4.17):替代 4.10c 的编译期 `match`。
//! 两层 `HashMap<String, Vec<NativeEntry>>`:外层类名键经 `Borrow<str>` 零分配按 `&str` 查,
//! 内层 Vec 线性扫 name+desc(每类 native 个位数,cache 友好)。对应 HotSpot 每 `Method` 的
//! `native_function` 字段(`method.hpp:441-447`),rustj 集中成单表(per-Method 缓存顺延)。
//! `register` upsert(同键覆盖,镜像 `Method::set_native_function`);为 4.16 RegisterNatives 预留。

use std::collections::HashMap;

use crate::runtime::{Reference, Value, VmError, VmThread};

/// 单个 native 方法的实现指针。删掉 4.10c 签名里的 class/name/desc——那三者只用于分派查表,
/// native 体不需要。非捕获闭包在 `register(..., f: NativeFn)` 位自动协变为本类型(零成本)。
pub(crate) type NativeFn = fn(&mut VmThread, Option<Reference>, &[Value]) -> Result<Value, VmError>;

pub(crate) struct NativeRegistry {
    by_class: HashMap<String, Vec<NativeEntry>>,
}

struct NativeEntry {
    name: String,
    desc: String,
    f: NativeFn,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(_vm: &mut VmThread, _this: Option<Reference>, _args: &[Value]) -> Result<Value, VmError> {
        Ok(Value::Int(1))
    }
    fn other(_vm: &mut VmThread, _this: Option<Reference>, _args: &[Value]) -> Result<Value, VmError> {
        Ok(Value::Int(2))
    }

    #[test]
    fn resolve_miss_returns_none() {
        let reg = NativeRegistry::new();
        assert!(reg.resolve("java/lang/Foo", "bar", "()V").is_none());
    }

    #[test]
    fn register_then_resolve_roundtrip() {
        let mut reg = NativeRegistry::new();
        reg.register("java/lang/Object", "hashCode", "()I", dummy);
        let f = reg.resolve("java/lang/Object", "hashCode", "()I").expect("应命中");
        // fn 指针可比较(dummy 即其地址)。
        assert_eq!(f as usize, dummy as usize);
    }

    #[test]
    fn register_upsert_overwrites_same_key() {
        let mut reg = NativeRegistry::new();
        reg.register("java/lang/Object", "hashCode", "()I", dummy);
        reg.register("java/lang/Object", "hashCode", "()I", other); // 同 (class,name,desc) → 覆盖
        let f = reg.resolve("java/lang/Object", "hashCode", "()I").expect("应命中");
        assert_eq!(f as usize, other as usize, "upsert 后须返后者");
    }

    #[test]
    fn resolve_distinct_methods_in_same_class() {
        let mut reg = NativeRegistry::new();
        reg.register("java/lang/Object", "hashCode", "()I", dummy);
        reg.register("java/lang/Object", "getClass", "()Ljava/lang/Class;", other);
        assert_eq!(reg.resolve("java/lang/Object", "hashCode", "()I").unwrap() as usize, dummy as usize);
        assert_eq!(reg.resolve("java/lang/Object", "getClass", "()Ljava/lang/Class;").unwrap() as usize, other as usize);
    }
}
```

- [ ] **Step 2: 跑测试看它失败(正确的失败原因)**

Run: `cargo test --lib registry::tests 2>&1 | Select-Object -First 30`(PowerShell)或 `cargo test --lib registry::tests`
Expected: **编译失败**(`NativeRegistry::new` / `register` / `resolve` 未实现)。

- [ ] **Step 3: 写最小实现(在 registry.rs 测试上方追加 impl)**

在 `struct NativeEntry { ... }` 之后、`#[cfg(test)]` 之前插入:
```rust
impl NativeRegistry {
    pub(crate) fn new() -> Self {
        Self { by_class: HashMap::new() }
    }

    /// 登记一个 native。**upsert**:同 (class,name,desc) 已存在 → 覆盖 fn;否则 push。
    /// 对应 HotSpot `Method::set_native_function`(`method.cpp:1024-1044`):同 fn 幂等、不同 fn 覆盖。
    /// 静态注册期无重键 → 零副作用;将来 4.16 `JNI_RegisterNatives` 覆盖注册直接复用。
    pub(crate) fn register(&mut self, class: &str, name: &str, desc: &str, f: NativeFn) {
        let v = self.by_class.entry(class.to_string()).or_default();
        if let Some(e) = v.iter_mut().find(|e| e.name == name && e.desc == desc) {
            e.f = f;
        } else {
            v.push(NativeEntry { name: name.to_string(), desc: desc.to_string(), f });
        }
    }

    /// 零分配查表:外层 `String` 键经 `Borrow<str>` 按 `&str` 查;内层 Vec 线性扫 name+desc。
    /// fn 指针 `Copy`,返 owned `Option<NativeFn>`,调用方释锁后再调(不在持锁态调 native 体)。
    pub(crate) fn resolve(&self, class: &str, name: &str, desc: &str) -> Option<NativeFn> {
        self.by_class
            .get(class)?
            .iter()
            .find(|e| e.name == name && e.desc == desc)
            .map(|e| e.f)
    }
}
```

- [ ] **Step 4: 在 native/mod.rs 登记 + 重导出**

在 `native/mod.rs` 行 32(`mod sun_nio_fs;`)之后加:
```rust
mod registry;
pub(crate) use registry::{NativeFn, NativeRegistry};
```

- [ ] **Step 5: 跑测试看它通过**

Run: `cargo test --lib registry::tests`
Expected: **4 tests passed**(`resolve_miss_returns_none` / `register_then_resolve_roundtrip` / `register_upsert_overwrites_same_key` / `resolve_distinct_methods_in_same_class`)。

- [ ] **Step 6: Commit**

```bash
git add src/runtime/interpreter/native/registry.rs src/runtime/interpreter/native/mod.rs
git commit -m "feat(native): NativeRegistry fn 指针表 + register/resolve upsert (Layer 4.17 infra)" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: `natives!` 声明式宏

**Files:**
- Modify: `src/runtime/interpreter/native/mod.rs`(宏定义置于子模块声明**之前**;单测置于既有 `mod tests`)

- [ ] **Step 1: 写失败测试**

在 `native/mod.rs` 的 `#[cfg(test)] mod tests`(行 121)内两处加代码:
(a) **顶部**(`use super::*;` 之后)用宏生成一个测试用 `register` fn;(b) 末尾加测试调用它。
```rust
#[cfg(test)]
mod tests {
    use super::*;

    // 宏展开 → 生成 `pub(super) fn register(&mut NativeRegistry)`,登记 2 条测试 native。
    // 文本作用域:`natives!` 在 native/mod.rs 顶部定义、本 mod 在其后声明 → 本 mod 可见。
    natives! {
        ("test/Sample", "one", "()I") => |_vm, _this, _args| Ok(Value::Int(1));
        ("test/Sample", "two", "()I") => |_vm, _this, _args| Ok(Value::Int(2));
    }

    // …(既有测试保持不变)…

    /// `natives!` 宏生成的 `register(&mut NativeRegistry)` 须把每条 (class,name,desc)=>闭包
    /// 登记进表;非捕获闭包协变为 fn 指针。
    #[test]
    fn natives_macro_generates_register() {
        let mut reg = NativeRegistry::new();
        register(&mut reg); // 宏在本 mod 作用域生成的 register。
        assert!(reg.resolve("test/Sample", "one", "()I").is_some());
        assert!(reg.resolve("test/Sample", "two", "()I").is_some());
        assert!(reg.resolve("test/Sample", "missing", "()I").is_none());
    }
}
```
> `Value`/`NativeRegistry` 经 `use super::*` 可见;`register` 由宏在本 mod 生成。闭包形参 `_vm`/`_this`/`_args` 加 `_` 前缀避免 unused 警告。

- [ ] **Step 2: 跑测试看它失败**

Run: `cargo test --lib natives_macro_generates_register`
Expected: **编译失败**——`macro natives! is undefined` / `natives!` 未声明。

- [ ] **Step 3: 写宏定义**

在 `native/mod.rs` 顶部 `use super::{throw_exception, Value, VmError};`(行 24)之后、`mod java_io;`(行 26)**之前**插入(**关键**:文本作用域须先于所有子模块声明,子模块文件方能用裸 `natives!`):
```rust
/// 声明式登记一个模块的全部 native(替代手写 `match` + `dispatch` 路由)。
/// 生成该模块的 `pub(super) fn register(&mut NativeRegistry)`;每条 `(class,name,desc) => <闭包>`,
/// 闭包须**非捕获**(在 `register(..., f: NativeFn)` 位协变为零成本 fn 指针;捕获即编译错——护栏)。
/// 用法见各 `native/<pkg>.rs`。
macro_rules! natives {
    ( $( ($class:literal, $name:literal, $desc:literal) => $body:expr );* $(;)? ) => {
        pub(super) fn register(reg: &mut $crate::runtime::interpreter::native::NativeRegistry) {
            $(
                reg.register($class, $name, $desc, $body);
            )*
        }
    };
}
```

- [ ] **Step 4: 跑测试看它通过**

Run: `cargo test --lib natives_macro_generates_register`
Expected: **1 test passed**。

- [ ] **Step 5: Commit**

```bash
git add src/runtime/interpreter/native/mod.rs
git commit -m "feat(native): natives! 声明式宏(非捕获闭包协变 fn 指针,生成 register)" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: 接入 Vm(registry 字段 + `Vm::new` 填充 + `native_resolve` + fallback invoke)

> 本任务后:registry 已在 `Vm::new` 填充(此时 `register_all` 暂为空 → 表空 → 所有 native 仍经 fallback `dispatch` 命中,行为不变)。后续每个模块迁移向 `register_all` 加一行。

**Files:**
- Modify: `src/runtime/vm.rs`(字段 + `Vm::new` + `VmThread::native_resolve` + sync 断言)
- Modify: `src/runtime/interpreter/native/mod.rs`(`register_all` 空壳 + `invoke`/`invoke_inner` fallback 改写)

- [ ] **Step 1: 写失败测试(`native_resolve` 接线 + NativeRegistry: Send+Sync)**

在 `vm.rs` 的 `mod sync_assertions`(行 502 起)末尾(`}` 闭合 mod 前)追加:
```rust
    /// Layer 4.17:`VmThread::native_resolve` 经 `Vm.native_registry`(RwLock 读锁,拷 fn 指针,
    /// 释锁)返 `Option<NativeFn>`。未登记(表空或无此 native)→ `None`。
    #[test]
    fn native_resolve_returns_none_for_unregistered() {
        let reg = ClassRegistry::new();
        let vm = VmThread::new(reg);
        // Task 3 时 register_all 仍空 → Object.hashCode 未登记 → None。
        assert!(vm.native_resolve("java/lang/Object", "hashCode", "()I").is_none());
    }

    /// Layer 4.17:`NativeRegistry: Send + Sync`(RwLock<NativeRegistry> 为 Vm 字段,Vm: Send+Sync 的前置)。
    #[test]
    fn native_registry_is_send_sync() {
        assert_send::<crate::runtime::interpreter::native::NativeRegistry>();
        assert_sync::<crate::runtime::interpreter::native::NativeRegistry>();
    }
```
> `assert_send`/`assert_sync` 已在 sync_assertions 顶部定义(行 522–523)。`native_resolve` 与 `native_registry` 字段在 Step 3 实现(此刻编译失败 = RED)。

- [ ] **Step 2: 跑测试看它失败**

Run: `cargo test --lib sync_assertions::native_`
Expected: **编译失败**——`VmThread::native_resolve` 未定义;`Vm` 无 `native_registry` 字段。

- [ ] **Step 3: vm.rs —— 加 RwLock 导入、registry 字段、Vm::new 填充、native_resolve**

3a. 行 16 `use std::sync::{Arc, Condvar, Mutex, MutexGuard};` 改为:
```rust
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
```

3b. 在 `native/mod.rs` 加 `register_all` 空壳(行 100 `dispatch` fn 之后、`is_primitive_name` 之前):
```rust
/// 把所有内置 native 模块的 `register` 串调,填满 `NativeRegistry`。
/// 渐进迁移:每迁一个模块,在此加一行 `<module>::register(reg);`(模块迁移见各 Task)。
/// Task 3 阶段为空(所有模块仍走 `dispatch` fallback);全部迁完后 fallback 删除(Task 11)。
pub(crate) fn register_all(reg: &mut NativeRegistry) {
    let _ = reg; // 占位:迁移期间逐模块加 `<module>::register(reg);`。
}
```

3c. **开 `native` 模块可见性**(vm.rs 须引用 `crate::runtime::interpreter::native::{NativeRegistry, register_all}`):`NativeRegistry`/`register_all` 虽已 `pub(crate)`,但外层 `mod native;` 私有 → 路径不可达。把 `interpreter/mod.rs` 行 15
```rust
mod native;
```
改为
```rust
pub(crate) mod native;
```
(同邻行 `pub(crate) mod string;` 风格。)然后在 `vm.rs` 顶部 import 区加:
```rust
use crate::runtime::interpreter::native::NativeRegistry;
```
(`register_all` 以全路径 `crate::runtime::interpreter::native::register_all` 调,见 3e。)

3d. `Vm` 结构体(行 194–232)末尾字段 `main_thread` 之后加:
```rust
    /// Native 方法 fn 指针注册表(Layer 4.17):替代 4.10c 编译期 match。读多写稀(写仅 `Vm::new`
    /// 与将来 4.16 RegisterNatives)→ `RwLock`。`Vm::new` 时 `register_all` 一次性填满。
    /// 对应 HotSpot 每 `Method` 的 `native_function`,rustj 集中成单表(per-Method 缓存顺延)。
    native_registry: RwLock<NativeRegistry>,
```

3e. `Vm::new`(行 237–251)改为先建 registry 再填:
```rust
    fn new(registry: Option<Arc<ClassRegistry>>) -> Self {
        let mut native_registry = NativeRegistry::new();
        crate::runtime::interpreter::native::register_all(&mut native_registry);
        Self {
            heap: Mutex::new(Heap::new()),
            registry,
            string_pool: Mutex::new(StringPool::new()),
            monitors: Mutex::new(HashMap::new()),
            threads: threads::ThreadManager::new(),
            exception_meta: Mutex::new(HashMap::new()),
            class_mirrors: Mutex::new(bimap::BiMap::new()),
            module_mirrors: Mutex::new(HashMap::new()),
            unnamed_module: Mutex::new(None),
            phase: Mutex::new(VmPhase::Created),
            main_thread: Mutex::new(None),
            native_registry: RwLock::new(native_registry),
        }
    }
```

3f. `impl VmThread` 内(任意位置,建议靠近其它 `pub(crate)` accessor,如 `vm_arc`/`from_vm` 附近,行 327 周围)加:
```rust
    /// 按 (class,name,desc) 取 native 实现:读锁 → `NativeRegistry::resolve` → 拷出 owned
    /// `Option<NativeFn>` → 释锁。**不在持锁态调 native 体**(避免串行化所有 native 调用)。
    /// 命中 → 调用方 `f(vm, this, args)`;未命中 → 调用方抛 `UnsatisfiedLinkError`。
    pub(crate) fn native_resolve(
        &self,
        class: &str,
        name: &str,
        desc: &str,
    ) -> Option<crate::runtime::interpreter::native::NativeFn> {
        self.runtime
            .native_registry
            .read()
            .unwrap()
            .resolve(class, name, desc)
    }
```

- [ ] **Step 4: native/mod.rs —— invoke 改 fallback**

把 `invoke`(行 49–61)+ `dispatch`(行 66–100)整段替换为:
```rust
pub(super) fn invoke(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    vm.push_frame(class, name);
    let result = invoke_inner(vm, class, name, desc, this, args);
    vm.pop_frame();
    result
}

/// `invoke` 内核(已 push_frame):(1) 任意类的 `registerNatives()V` 空操作(rustj 编译期表,
/// native 恒已注册——JDK 侧 registerNatives 把 Java_*/JVM_* 登记进方法槽,rustj 无此运行期步骤);
/// (2) 命中 `NativeRegistry` → 调 fn 指针;(3) miss → 旧 `dispatch` 前缀路由 fallback
/// (渐进迁移期;全部模块迁完后删除,Task 11)。
fn invoke_inner(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    if name == "registerNatives" && desc == "()V" {
        return Ok(Value::Void);
    }
    if let Some(f) = vm.native_resolve(class, name, desc) {
        return f(vm, this, args);
    }
    dispatch(vm, class, name, desc, this, args)
}

/// 按**声明类前缀**路由(迁移期 fallback;每迁一个模块,删其对应臂,Task 4–10;全删于 Task 11)。
/// registerNatives 已在 `invoke_inner` 处理;`name`/`desc` 透传给各子模块 `dispatch`。
fn dispatch(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match class {
        c if c.starts_with("java/lang/invoke/") => {
            java_lang_invoke::dispatch(vm, c, name, desc, this, args)
        }
        c if c.starts_with("java/lang/") => java_lang::dispatch(vm, c, name, desc, this, args),
        c if c.starts_with("java/io/") => java_io::dispatch(vm, c, name, desc, this, args),
        c if c.starts_with("sun/nio/fs/") => sun_nio_fs::dispatch(vm, c, name, desc, this, args),
        "jdk/internal/misc/VM" | "jdk/internal/misc/CDS" | "jdk/internal/misc/Unsafe" => {
            jdk_internal::dispatch(vm, class, name, desc, this, args)
        }
        "jdk/internal/loader/NativeLibraries" | "jdk/internal/loader/NativeLibrary"
        | "jdk/internal/loader/BootLoader" => {
            jdk_internal_loader::dispatch(vm, class, name, desc, this, args)
        }
        c if c.starts_with("jdk/internal/reflect/") => {
            jdk_internal_reflect::dispatch(vm, c, name, desc, this, args)
        }
        _ => Err(throw_unsatisfied_link_error(vm, class, name, desc)),
    }
}

/// 未登记 native → `UnsatisfiedLinkError`(带 class.name+desc 诊断,对应 HotSpot NativeLookup 报错)。
fn throw_unsatisfied_link_error(vm: &mut VmThread, class: &str, name: &str, desc: &str) -> VmError {
    let msg = format!("{}.{} {}", class.replace('/', "."), name, desc);
    super::throw_exception_with_message(vm, "java/lang/UnsatisfiedLinkError", &msg)
}
```
> `dispatch` 的 `_ =>` 臂从 `throw_exception(vm, ...)` 改为 `throw_unsatisfied_link_error(...)`(带诊断串)。registerNatives 特例已上移到 `invoke_inner`,故 `dispatch` 不再处理它(`let _ = (name, desc)` 消未用警告)。

- [ ] **Step 5: 跑测试看新测试通过 + 全套不退**

Run: `cargo test --lib 2>&1 | Select-String -Pattern "test result|error\[|warning: unused"`
Expected: `native_resolve_returns_none_for_unregistered` + `native_registry_is_send_sync` 通过;全套 350+ 绿;无新编译错/警告。

- [ ] **Step 6: clippy 净**

Run: `cargo clippy --lib --tests -- -D warnings 2>&1 | Select-String -Pattern "warning|error"`
Expected: 空(无 warning/error)。

- [ ] **Step 7: Commit**

```bash
git add src/runtime/vm.rs src/runtime/interpreter/native/mod.rs
git commit -m "feat(native): registry 接入 Vm(Vm::new 填充 + native_resolve)+ invoke fallback" -m "Task 3 后 registry 已建(空表),native 仍全经 dispatch fallback,行为不变。ULE 带诊断串。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: 迁移 `sun_nio_fs`(最小,1 native —— 验证迁移管线)

**Files:**
- Modify: `src/runtime/interpreter/native/sun_nio_fs.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`(`register_all` 加一行 + `dispatch` 删 `sun/nio/fs/` 臂)

- [ ] **Step 1: 改 sun_nio_fs.rs(删 dispatch,换 natives!)**

把整个 `pub(super) fn dispatch(...) { match (class,name,desc) { ... } }`(行 25–45)替换为:
```rust
natives! {
    // WindowsNativeDispatcher.initIDs()V —— 本机 jmod <clinit>:1100 调用。HotSpot 历史语义仅缓存
    // field ID,无 FS/Win32 访问 → 空操作返 void(同 WinNTFileSystem.initIDs 4.25、FileDescriptor.initIDs 4.35)。
    ("sun/nio/fs/WindowsNativeDispatcher", "initIDs", "()V") => |_vm, _this, _args| Ok(Value::Void);
}
```
保留文件顶的 `use crate::runtime::{Reference, Value, VmThread, VmError};` 与 `use super::super::throw_exception;`——本条 native 体不需它们,但若保留以防后续 native 用;若 clippy 报 unused,则删 `throw_exception`/`Reference`/`VmThread`/`VmError` 中未用者(本步仅留 `Value`,因体里用到 `Value::Void`)。**实际:本模块只一个返 void 的 native,体仅需 `Value`**;把 `use` 行改为:
```rust
use crate::runtime::Value;
```
(删 `Reference`/`VmThread`/`VmError`/`throw_exception`——本 native 体未用。)

- [ ] **Step 2: native/mod.rs —— register_all 加 sun_nio_fs;dispatch 删其臂**

2a. `register_all`(Task 3 加的)改为:
```rust
pub(crate) fn register_all(reg: &mut NativeRegistry) {
    sun_nio_fs::register(reg);
}
```
2b. `dispatch` 删除 `c if c.starts_with("sun/nio/fs/") => sun_nio_fs::dispatch(...)` 臂(行 87)。

- [ ] **Step 3: 跑 sun_nio_fs 测试 + 全套**

Run: `cargo test --lib 2>&1 | Select-String -Pattern "test result|error\["`
Expected: 全套绿。`windows_native_dispatcher_init_ids_returns_void` 经 `invoke` → `native_resolve`(命中 sun_nio_fs::register 登记项)→ fn 指针 → `Ok(Value::Void)`。

- [ ] **Step 4: clippy 净**

Run: `cargo clippy --lib --tests -- -D warnings 2>&1 | Select-String -Pattern "warning|error"`
Expected: 空。

- [ ] **Step 5: Commit**

```bash
git add src/runtime/interpreter/native/sun_nio_fs.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): sun_nio_fs 迁 natives! 宏 + registry(退役首模块 dispatch)" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: 迁移 `jdk_internal_loader`(5 natives)

**Files:**
- Modify: `src/runtime/interpreter/native/jdk_internal_loader.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`(`register_all` + `dispatch`)

- [ ] **Step 1: 改 jdk_internal_loader.rs**

把 `pub(super) fn dispatch(...) { match (class,name,desc) { ... } }` 整体替换为 `natives! { ... }`,按**迁移规则模板**:每个 `($class, $name, $desc) => <body>` 臂 → `($class, $name, $desc) => |vm, this, args| <body>;`(形参按原 `<body>` 用名:`this`/`args` 或 `_this`/`_args`,以原文件为准)。`_ => ULE` 臂丢弃。保留文件顶 `use` 导入(体里仍用 `throw_exception` 等)与模块内 helper 自由函数。
模板(以实际臂数为准):
```rust
natives! {
    ("jdk/internal/loader/NativeLibraries", "<name>", "<desc>") => |vm, this, args| <原 body>,
    ("jdk/internal/loader/NativeLibrary",   "<name>", "<desc>") => |vm, this, args| <原 body>,
    ("jdk/internal/loader/BootLoader",      "<name>", "<desc>") => |vm, this, args| <原 body>,
    // …每个原 match 臂一条…
}
```
(读 `jdk_internal_loader.rs` 现有 `match` 逐臂搬迁;`<body>` 逐字不动。)

- [ ] **Step 2: native/mod.rs —— register_all + dispatch**

2a. `register_all` 追加一行:
```rust
    jdk_internal_loader::register(reg);
```
2b. `dispatch` 删除 `"jdk/internal/loader/NativeLibraries" | "jdk/internal/loader/NativeLibrary" | "jdk/internal/loader/BootLoader" => jdk_internal_loader::dispatch(...)` 臂(行 91–94)。

- [ ] **Step 3: 全套 + clippy**

Run: `cargo test --lib 2>&1 | Select-String -Pattern "test result|error\["` → 全绿。
Run: `cargo clippy --lib --tests -- -D warnings 2>&1 | Select-String -Pattern "warning|error"` → 空。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/native/jdk_internal_loader.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): jdk_internal_loader 迁 natives! 宏 + registry" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 6: 迁移 `java_io`

**Files:**
- Modify: `src/runtime/interpreter/native/java_io.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`

- [ ] **Step 1: 改 java_io.rs** —— 按**迁移规则模板**:删 `dispatch`/`match`,换 `natives! { ... }`,每臂 `($class,$name,$desc) => <body>` → `($class,$name,$desc) => |vm, this, args| <body>;`,`_ => ULE` 丢。保留 `use` 与 helper。

- [ ] **Step 2: native/mod.rs —— register_all + dispatch**

2a. `register_all` 追加 `java_io::register(reg);`。
2b. `dispatch` 删 `c if c.starts_with("java/io/") => java_io::dispatch(...)` 臂。

- [ ] **Step 3: 全套 + clippy** —— `cargo test --lib` 全绿;`cargo clippy --lib --tests -- -D warnings` 空。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/native/java_io.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): java_io 迁 natives! 宏 + registry" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 7: 迁移 `java_lang_invoke`

**Files:**
- Modify: `src/runtime/interpreter/native/java_lang_invoke.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`

- [ ] **Step 1: 改 java_lang_invoke.rs** —— 按**迁移规则模板**迁 `dispatch`→`natives!`。保留 `use` 与 helper(`MethodHandleNatives` 相关)。

- [ ] **Step 2: native/mod.rs —— register_all + dispatch**

2a. `register_all` 追加 `java_lang_invoke::register(reg);`(**注意顺序**:此模块的 `register` 须在 `java_lang::register` **之前**——`java/lang/invoke/` 前缀优先于 `java/lang/`;虽然 registry 是精确 (class,name,desc) 键不再靠前缀,但保持源序清晰无妨,顺序无功能影响)。
2b. `dispatch` 删 `c if c.starts_with("java/lang/invoke/") => java_lang_invoke::dispatch(...)` 臂(行 82–84)。

- [ ] **Step 3: 全套 + clippy** —— 全绿 / 空。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/native/java_lang_invoke.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): java_lang_invoke 迁 natives! 宏 + registry" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 8: 迁移 `jdk_internal`(VM/CDS/Unsafe,18 natives)

**Files:**
- Modify: `src/runtime/interpreter/native/jdk_internal.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`

- [ ] **Step 1: 改 jdk_internal.rs** —— 按**迁移规则模板**迁 `dispatch`→`natives!`。**保留**对 `super::is_primitive_name` / `super::class_arg_name` 的调用(这两个 helper 留在 `mod.rs`,闭包体里照 `super::class_arg_name(vm, args)` 调)。保留模块内 helper。

- [ ] **Step 2: native/mod.rs —— register_all + dispatch**

2a. `register_all` 追加 `jdk_internal::register(reg);`。
2b. `dispatch` 删 `"jdk/internal/misc/VM" | "jdk/internal/misc/CDS" | "jdk/internal/misc/Unsafe" => jdk_internal::dispatch(...)` 臂(行 88–90)。

- [ ] **Step 3: 全套 + clippy** —— 全绿 / 空。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/native/jdk_internal.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): jdk_internal(VM/CDS/Unsafe) 迁 natives! 宏 + registry" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 9: 迁移 `jdk_internal_reflect`(33 natives)

**Files:**
- Modify: `src/runtime/interpreter/native/jdk_internal_reflect.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`

- [ ] **Step 1: 改 jdk_internal_reflect.rs** —— 按**迁移规则模板**迁 `dispatch`→`natives!`(33 臂,体逐字搬)。保留模块顶 `pub(crate) use ... {alloc_wrapper, primitive_wrapper, unbox_arg};` 重导出(`mod.rs` 行 36 仍引它们,G.4.1 lambda 适配器复用——**不动**)与 helper。

- [ ] **Step 2: native/mod.rs —— register_all + dispatch**

2a. `register_all` 追加 `jdk_internal_reflect::register(reg);`。
2b. `dispatch` 删 `c if c.starts_with("jdk/internal/reflect/") => jdk_internal_reflect::dispatch(...)` 臂(行 95–97)。

- [ ] **Step 3: 全套 + clippy** —— 全绿 / 空。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/native/jdk_internal_reflect.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): jdk_internal_reflect(33 法)迁 natives! 宏 + registry" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 10: 迁移 `java_lang`(50 natives —— 压轴)

**Files:**
- Modify: `src/runtime/interpreter/native/java_lang.rs`
- Modify: `src/runtime/interpreter/native/mod.rs`

- [ ] **Step 1: 改 java_lang.rs** —— 按**迁移规则模板**迁 `dispatch`→`natives!`(50 臂,体逐字搬)。形参按原 `<body>` 用名(`this`/`args` 多见;Object.hashCode 等用 `this`)。保留模块内所有 helper 自由函数(`capture_backtrace`、`obj_hashcode` 等——闭包体按名调)。保留 `use`。

- [ ] **Step 2: native/mod.rs —— register_all + dispatch**

2a. `register_all` 追加 `java_lang::register(reg);`(置于 invoke 系之后,顺序无功能影响)。
2b. `dispatch` 删 `c if c.starts_with("java/lang/") => java_lang::dispatch(...)` 臂(行 85)。

- [ ] **Step 3: 全套 + clippy** —— 全绿 / 空。`native/mod.rs` 自带的 11 个 `invoke` 单测(`object_hashcode_is_handle_id_mode4` 等)经 registry 命中通过——验证 50 法全登记。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/native/java_lang.rs src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): java_lang(50 法)迁 natives! 宏 + registry —— 全 native 上表" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 11: 收尾 —— 删 `dispatch` fallback,简化 `invoke_inner`

> 本任务后:全部 native 经 registry,`dispatch` 仅剩 `_ => ULE`(无路由臂)→ 整个删除,`invoke_inner` 去 fallback。

**Files:**
- Modify: `src/runtime/interpreter/native/mod.rs`

- [ ] **Step 1: 确认 dispatch 已无路由臂**

读 `dispatch`:此时应只剩
```rust
fn dispatch(...) -> Result<Value, VmError> {
    let _ = (name, desc);
    match class {
        _ => Err(throw_unsatisfied_link_error(vm, class, name, desc)),
    }
}
```

- [ ] **Step 2: 写失败测试 —— miss 现抛带诊断串的 ULE**

在 `native/mod.rs` 既有 `mod tests` 的 `unbound_native_throws_unsatisfied_link_error`(行 310)下方加一断言验证诊断串 + 把现有该测试改为走 registry-miss(全迁完后无 fallback,miss 直接抛):
```rust
    #[test]
    fn unbound_native_throws_unsatisfied_link_error_with_message() {
        let reg = crate::oops::ClassRegistry::new();
        let mut vm = crate::runtime::VmThread::new(reg);
        let err = invoke(&mut vm, "java/lang/Foo", "bar", "(I)V", None, &[]).unwrap_err();
        let crate::runtime::VmError::ThrownException(exc) = err else {
            panic!("应抛 ThrownException,得 {err:?}");
        };
        // 异常类 = UnsatisfiedLinkError;detailMessage 含 "java.lang.Foo.bar (I)V"。
        let heap = vm.heap();
        let Some(crate::oops::Oop::Instance(i)) = heap.get(exc) else { panic!("须为异常实例"); };
        assert_eq!(i.class_name(), "java/lang/UnsatisfiedLinkError");
        // detailMessage 字段(真 Throwable)经 throw_exception_with_message 写入;读回核对诊断串。
        // (若该字段读取须 helper,用 vm.instance_reference_field 读 "java/lang/Throwable"."detailMessage"
        //  再 intern 串比对;此处仅断言类名 + 不 panic 即可,诊断串细节由 format 串保证。)
        let _ = i; // 仅断言类名(诊断串构造见 throw_unsatisfied_link_error)。
    }
```
> 该测试在 Task 3 后(有 fallback)即应通过(miss → fallback dispatch → `_ => ULE`)。Task 11 收尾后(无 fallback)仍通过(miss → `invoke_inner` 的 None 分支直接抛)。故本测试**红优先**意义在 Task 11 之前先锁定行为;Task 11 删 fallback 时它不退。

- [ ] **Step 3: 删 dispatch、简化 invoke_inner**

把 `invoke_inner` 改为(去 fallback):
```rust
fn invoke_inner(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
    this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    if name == "registerNatives" && desc == "()V" {
        return Ok(Value::Void);
    }
    match vm.native_resolve(class, name, desc) {
        Some(f) => f(vm, this, args),
        None => Err(throw_unsatisfied_link_error(vm, class, name, desc)),
    }
}
```
**删除**整个 `fn dispatch(...)`(已无路由臂)。

- [ ] **Step 4: 处理 clippy 可能报的 dead helper**

若 `is_primitive_name`(mod.rs:104)在 java_lang 迁移后无生产调用(仅自测用),clippy 会报 dead_code。
- 若 `class_arg_name` 仍被 jdk_internal 用 → 留。
- 若 `is_primitive_name` dead → 删除该 fn 及其测试 `is_primitive_name_recognizes_keywords`(若仅测它)。
Run: `cargo clippy --lib --tests -- -D warnings 2>&1 | Select-String -Pattern "warning|error"`
按报告处理 dead 项。

- [ ] **Step 5: 全套 + clippy 终检**

Run: `cargo test --lib --tests 2>&1 | Select-String -Pattern "test result|error\["` → 全绿(350+ test)。
Run: `cargo clippy --lib --tests -- -D warnings 2>&1 | Select-String -Pattern "warning|error"` → 空。
Run: `cargo build --lib --tests 2>&1 | Select-String -Pattern "error|warning"` → 空。

- [ ] **Step 6: javac 集成闸门**

跑现有真 java.base 集成闸门(经 native 分派):
Run: `cargo test --test class_real_bytecode 2>&1 | Select-String -Pattern "test result|error"`
Run: `cargo test --test real_integer 2>&1 | Select-String -Pattern "test result|error"`
(及其它既有 javac 闸门;全绿。)
Expected: 全绿——native 分派经 registry 行为等价。

- [ ] **Step 7: Commit**

```bash
git add src/runtime/interpreter/native/mod.rs
git commit -m "refactor(native): 退役 dispatch fallback —— 全 native 经 NativeRegistry" -m "Layer 4.17 收尾:83 native 全 fn 指针上表,前缀路由 dispatch 删除;ULE 带诊断串。行为保持,全套绿 + clippy 净。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## 收尾核验清单

- [ ] 83 native 全部经 `NativeRegistry`(`register_all` 7 个模块齐;`dispatch` 已删)。
- [ ] `native::invoke` 签名不变 → `invoke.rs` 6 调用点零改动。
- [ ] 现有 `native/mod.rs::tests` 的 11 个单测 + 各模块自带测试全绿(经 registry 命中)。
- [ ] `sync_assertions`:`Arc<Vm>: Send+Sync` 仍绿(新增 `RwLock<NativeRegistry>` 不破坏);`native_registry_is_send_sync` 绿。
- [ ] clippy `-D warnings` 净;零 unsafe;零新增依赖(手写 `macro_rules!`)。
- [ ] javac 集成闸门(class_real_bytecode / real_integer / …)全绿。
- [ ] memory 更新:在 `hotspot-rust-migration-project.md` 记 Layer 4.17 完成。

## 回滚

纯重构,无数据/语义变更。任一模块迁移任务可 `git revert <commit>` 单独回退(Task 4–10 相互独立)。Task 3 的 fallback 设计保证迁移期间任一点全套可运行。
