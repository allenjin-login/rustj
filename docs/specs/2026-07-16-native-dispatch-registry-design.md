# Native 分派重构:NativeRegistry + `natives!` 宏(Layer 4.17)

> **状态**:设计(已与用户确认,待 spec 审阅 → 写实现计划)
> **日期**:2026-07-16
> **范围**:行为保持型分派重构。把 Layer 4.10c 的编译期 `match (class,name,desc)` 静态表,
> 换成数据驱动的 `NativeRegistry`(fn 指针表 + `register`/`resolve` API)+ 声明式 `natives!` 宏。
> 预留运行时注册口,使 4.16 的 `JNI_RegisterNatives` / `NativeLookup` 动态解析将来能直接接入。
> **北极星对照**:HotSpot 的 native fn 指针存在每个 `Method` 的 `native_function` 字段
> (`method.hpp:441-447`),由 `JNI_RegisterNatives`(`jni.cpp:2649-2714`)/ `NativeLookup::lookup`
> (`nativeLookup.cpp:409-423`)写入;dispatch 是 per-Method 字段查(O(1)),"表"只承载**注册**机制。

---

## 1. 背景与动机

### 现状(4.10c)
- 83 个 native 方法,经编译期 `match (class,name,desc)` 臂绑定,分 7 个 package module:
  `java_lang.rs`(50)、`jdk_internal_reflect.rs`(33)、`jdk_internal.rs`、`jdk_internal_loader.rs`(5)、
  `java_lang_invoke.rs`、`java_io.rs`、`sun_nio_fs.rs`(1)。
- 两级分派:`native/mod.rs::invoke` → 按 class 前缀路由到 `module::dispatch` → 每 module 一个
  巨型 `match (class,name,desc)`。
- 签名:`fn(&mut VmThread, class:&str, name:&str, desc:&str, this:Option<Reference>, args:&[Value])
  -> Result<Value,VmError>`。

### Smell(用户原话:"完全不是可持续发展的")
1. 巨型 match 臂(单 `java_lang.rs` 50 条),视觉噪声高、难导航。
2. 每 native 重复写完整 `(class,name,desc)` 三元组,冗余。
3. 描述符串手写、易错、无中心校验。
4. 注册分散:新增 native 要同时改 module 的 match **和**确信前缀路由命中。
5. **无法支持运行时 `RegisterNatives`**——静态 match 根本不可变,堵死 4.16 动态库加载。
6. miss 仅抛裸 `UnsatisfiedLinkError`,无 class/name/desc 诊断信息。

### HotSpot 模型(源码优先,§3)
- fn 指针在 `Method` 上(`method.hpp:441-447` `native_function()`),inline 紧随结构体;
  `set_native_function`(`method.cpp:1024-1044`)语义:**同 fn 幂等、不同 fn 覆盖**。
- 三种填充:(1) 默认桩抛 ULE;(2) `JNI_RegisterNatives` 走 `JNINativeMethod{name,signature,fnPtr}`
  → `Method::register_native`;(3) 首调懒解析 `NativeLookup::lookup` 经 `os::dll_lookup` 取符号并缓存。
- `JVM_*` 是普通 C 函数(`JVM_ENTRY`/`JVM_LEAF` 宏),**不是表**。

### 目标
- 杀掉巨型 match + 前缀路由;native 声明与分派解耦。
- 数据驱动注册:fn 指针入 `NativeRegistry`,新增 native = 加一行声明。
- 预留运行时注册 API(`register`/`resolve` `pub(crate)`),4.16 直接接入不返工。
- 零依赖、零 unsafe、行为逐位保持。

### 非目标(本层不做)
- 不在 `Method`/`LoadedClass` 上缓存 fn 指针(per-Method O(1) 优化,顺延)。
- 不实现 `JNI_RegisterNatives` / `NativeLookup` 动态解析(4.16)。
- 不改任何 native **语义**。
- 不引依赖(`natives!` 是手写 declarative macro)。

---

## 2. 架构与数据流

```
invoke.rs  ACC_NATIVE 分支
   │  (现签名不变)
   ▼
native::invoke(vm, class, name, desc, this, args)
   │
   ▼
vm.native_resolve(class, name, desc) ──►  RwLock 读锁
   │                                         │  by_class.get(class)
   │  Option<NativeFn>  ◄───────────────────┘  .iter().find(name,desc).map(f).copied()
   │  (fn 指针 Copy,锁内拷出即释锁;不在持锁态调 native 体)
   ├─ Some(f) ──► push_frame(class,name); r = f(vm, this, args); pop_frame(); r
   └─ None    ──► Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError",
                                       "{class}.{name} {desc}"))
```

前缀路由**整层移除**——单次 `native_resolve` 查表。module 文件仅作代码组织,各 expose 一个
`register()`(宏生成)在 bootstrap 一次性填表。

---

## 3. 类型与数据结构

新文件 `src/runtime/interpreter/native/registry.rs`:

```rust
use std::collections::HashMap;
use crate::runtime::{VmThread, Reference};
use crate::runtime::interpreter::Value;
use crate::runtime::error::VmError;

/// 单个 native 方法的实现指针。删掉了 4.10c 签名里的 class/name/desc 串——
/// 那三者只用于分派查表(`native_resolve`),native 体本身不需要;个别要自报类名的就在体内写字面量。
pub(crate) type NativeFn =
    fn(&mut VmThread, Option<Reference>, &[Value]) -> Result<Value, VmError>;

pub(crate) struct NativeRegistry {
    /// 类内部名 → 该类 native 列表。两层(非 `HashMap<(String,String,String), _>`)的原因见 §8 决策 #1。
    by_class: HashMap<String, Vec<NativeEntry>>,
}

struct NativeEntry {
    name: String,
    desc: String,
    f: NativeFn,
}

impl NativeRegistry {
    pub(crate) fn new() -> Self {
        Self { by_class: HashMap::new() }
    }

    /// 登记一个 native。**upsert 语义**(对应 HotSpot `Method::set_native_function`:
    /// 同 (class,name,desc) 已存在 → 替换 fn;否则 push)。静态注册期无重键 → 零副作用;
    /// 将来 `JNI_RegisterNatives` 覆盖注册直接复用此法。
    pub(crate) fn register(&mut self, class: &str, name: &str, desc: &str, f: NativeFn) {
        let v = self.by_class.entry(class.to_string()).or_default();
        if let Some(e) = v.iter_mut().find(|e| e.name == name && e.desc == desc) {
            e.f = f;                                  // upsert:同键覆盖
        } else {
            v.push(NativeEntry { name: name.to_string(), desc: desc.to_string(), f });
        }
    }

    /// 零分配查表:外层 `String` 键经 `Borrow<str>` 按 `&str` 查;内层 Vec 线性扫 name+desc。
    /// fn 指针是 `Copy`,返 owned `Option<NativeFn>`,调用方释锁后再调。
    pub(crate) fn resolve(&self, class: &str, name: &str, desc: &str) -> Option<NativeFn> {
        self.by_class.get(class)?
            .iter()
            .find(|e| e.name == name && e.desc == desc)
            .map(|e| e.f)
    }
}
```

`native/mod.rs` 顶部重导出:`pub(crate) use self::registry::{NativeRegistry, NativeFn};`

**Send+Sync**:`fn` 指针 + `HashMap<String, Vec<...>>` 均 `Send+Sync`;`NativeRegistry` 天然满足,
新 `sync_assertions` 显式断言守住。

---

## 4. `natives!` 宏

### 关键 Rust 事实
非捕获闭包字面量在期望 `fn(...)` 的位置会**自动协变**为 fn 指针。故宏**不必生成命名 fn**:
直接把闭包字面量传给 `register(..., f: NativeFn)`,编译期协变,零成本。闭包若捕获 → 编译错
("closures can only be coerced to fns if they do not capture any variables"),这正是护栏。

### 定义(置 `native/mod.rs`,`mod java_lang;` 等子模块声明**之前**,文本作用域可达子模块)

```rust
/// 声明式登记一个模块的全部 native。生成该模块的 `pub(super) fn register(&mut NativeRegistry)`。
/// 每条 `(class, name, desc) => |vm, this, args| { ... body }`;闭包须非捕获(协变为 fn 指针)。
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

### 用法(`java_lang.rs` 顶部)

```rust
natives! {
    ("java/lang/Object", "hashCode", "()I") => |vm, this, _args| {
        match this {
            Some(r) => Ok(Value::Int(obj_hashcode(vm, r))),
            None => Err(throw_exception(vm, "java/lang/NullPointerException")),
        }
    };
    ("java/lang/System", "currentTimeMillis", "()J") => |vm, _this, _args| {
        Ok(Value::Long(crate::time::current_millis()))
    };
    ("java/lang/Throwable", "fillInStackTrace", "(I)Ljava/lang/Throwable;") => |vm, this, _args| {
        match this {
            Some(r) => { capture_backtrace(vm, r); Ok(Value::Reference(r)) }
            None => Err(throw_exception(vm, "java/lang/NullPointerException")),
        }
    };
}

// 共享 helper 仍为模块内自由函数,闭包体按名调,不捕获。
fn obj_hashcode(vm: &mut VmThread, r: Reference) -> i32 { /* ... */ }
fn capture_backtrace(vm: &mut VmThread, r: Reference) { /* ... */ }
```

类名/描述符/实现紧挨一行 → 新增 native = 加一行,不碰分派路由。

---

## 5. 分派路径

### `native/mod.rs::invoke`(签名不变,体改)

```rust
pub(super) fn invoke(
    vm: &mut VmThread,
    class: &str, name: &str, desc: &str,
    this: Option<Reference>, args: &[Value],
) -> Result<Value, VmError> {
    let Some(f) = vm.native_resolve(class, name, desc) else {
        return Err(throw_unsatisfied_link_error(vm, class, name, desc));
    };
    vm.push_frame(class, name);
    let r = f(vm, this, args);          // fn 指针调用,锁已释
    vm.pop_frame();
    r
}
```

`invoke.rs` 的 `ACC_NATIVE` 分支(`invoke.rs:568-570` 现场调 `native::invoke`)**零改动**。

### `VmThread::native_resolve`(新,薄封装 RwLock)

```rust
/// 取读锁 → resolve → 拷出 owned fn 指针 → 释锁。不在持锁态调 native 体(避免串行化)。
pub(crate) fn native_resolve(&self, class: &str, name: &str, desc: &str) -> Option<NativeFn> {
    self.runtime.native_registry.read().unwrap().resolve(class, name, desc)
}
```

### `throw_unsatisfied_link_error`(新 helper,诊断串)

```rust
fn throw_unsatisfied_link_error(vm: &mut VmThread, class: &str, name: &str, desc: &str) -> VmError {
    // 内部名 → 二进制名(java/lang/Foo → java.lang.Foo)拼消息,对应 HotSpot NativeLookup 报错风格。
    let msg = format!("{}.{} {}", class.replace('/', "."), name, desc);
    throw_exception_with_message(vm, "java/lang/UnsatisfiedLinkError", &msg)
}
```
复用现有 `throw_exception_with_message(vm, class_name, message)`(`interpreter/mod.rs:74`,
带 detailMessage 的异常构造——已置真 Throwable.detailMessage 字段,供 getMessage() 读回)。

---

## 6. 模块迁移(机械,逐 module,7 个)

每 module 文件保留,操作:
1. 删 `pub(super) fn dispatch(...)` 的 `match` 外壳。
2. 每个 `match` 臂的**体**(已是 `Ok(...)`/`Err(...)` 表达式)逐字搬进 `natives! { ... }` 的闭包体。
3. 共享 helper(`capture_backtrace`、`obj_hashcode` 等)保留为模块内自由函数。
4. 宏生成新的 `pub(super) fn register(&mut NativeRegistry)`。

### `native/mod.rs`
- 删 `dispatch`(前缀路由整体移除)。
- 加 `pub(crate) use self::registry::{NativeRegistry, NativeFn};`。
- 加 `macro_rules! natives`(见 §4)。
- 加 `register_all`:
  ```rust
  pub(crate) fn register_all(reg: &mut NativeRegistry) {
      java_lang::register(reg);
      java_lang_invoke::register(reg);
      java_io::register(reg);
      sun_nio_fs::register(reg);
      jdk_internal::register(reg);
      jdk_internal_loader::register(reg);
      jdk_internal_reflect::register(reg);
  }
  ```
  (子模块声明次序与 `register_all` 调用次序对齐;`natives!` 宏须在所有子 `mod` 声明之前定义。)

### 迁移顺序(先小后大,每步全套绿才进下一步)
1. `sun_nio_fs.rs`(1)——验证宏/管线端到端。
2. `jdk_internal_loader.rs`(5)。
3. `java_io.rs`、`java_lang_invoke.rs`。
4. `jdk_internal.rs`。
5. `jdk_internal_reflect.rs`(33)。
6. `java_lang.rs`(50)——最大,压轴。

---

## 7. 引导 wiring

### `Vm` 字段(`runtime/vm.rs`)
```rust
/// Native 方法 fn 指针注册表(Layer 4.17):替代 4.10c 的编译期 match。读多写稀(写仅 bootstrap
/// 与将来 4.16 RegisterNatives),故 RwLock。对应 HotSpot 每 Method 的 native_function,但
/// rustj 集中成单表(per-Method 缓存顺延)。fn 指针 Copy,resolve 锁内拷出即释锁。
native_registry: RwLock<NativeRegistry>,
```
`Default`:`native_registry: RwLock::new(NativeRegistry::new()),`

### `Vm::bootstrap()`(最早一步)
注册须在**任何会触发 native 的 `interpret()` 之前**(引导 savedProps 阶段即会调 native):
```rust
{
    let mut reg = NativeRegistry::new();
    native::register_all(&mut reg);
    *self.native_registry.write().unwrap() = reg;
}
// …其后才进入 Phase1/2/3 解释执行
```

---

## 8. 决策记录

### #1 为什么 `HashMap<String, Vec<NativeEntry>>` 而非 `HashMap<(String,String,String), NativeFn>`?
Rust tuple 无跨元素 `Borrow` 实现(`(String,String,String): Borrow<(&str,&str,&str)>` 不成立),
故按 `&str` 查三元组键只能每次 `to_string()` 造键(每次 native 调用 3 次分配)或自造 key 类型。
两层方案:外层 `String` 键经 `Borrow<str>` 零分配查 `&str`;内层 Vec 线性扫 name+desc。每类 native
个位数(Object ~6、System ~5),线性扫 cache 友好且**全程零分配**。亦贴合 HotSpot"native 隶属于类"心智。

### #2 为什么 `fn` 指针而非 `Box<dyn Fn>`?
83 个 native 全经 `&mut VmThread` 取 VM 状态,**无捕获需求**。`fn` 指针:零堆分配、零虚调用、
`Copy`(锁内拷出即释锁)、`Send+Sync`。`Box<dyn Fn>` 的堆分配 + 虚调用换不到东西,且类型签名锁死
将来捕获约束。非捕获闭包**字面量**仍可作为声明语法(自动协变 fn 指针)——"闭包形式"与"零成本"
两者兼得(用户选项 C)。

### #3 为什么宏生成 `register()` 而非每 native 命名 fn?
宏把"类名/描述符/实现"紧挨成一行,杀掉大 match 视觉噪声,新增 native = 加一行;且**不必生成命名 fn**
——闭包字面量在 `register(..., f: NativeFn)` 位置直接协变 fn 指针,免去唯一命名问题。宏是手写
declarative macro,不引依赖(守 bimap 唯一破例)。

### #4 为什么 `register` 取 upsert 而非 push?
HotSpot `Method::set_native_function`(`method.cpp:1024-1044`)即"同 fn 幂等、不同 fn 覆盖"语义。
静态注册期无重键,upsert 零副作用;将来 4.16 `JNI_RegisterNatives` 覆盖注册直接复用,无需新加方法。

---

## 9. 测试计划(TDD 红优先;§4 节奏)

行为保持型重构 → 主安全网 = **现有全套绿**(83 native + 各 javac 集成闸门)。新增单元测试逐个红→绿:

| # | 测试 | 红(失败原因) | 绿(实现) |
|---|------|--------------|----------|
| 1 | `registry_resolve_miss`:`new()` → resolve 任一 key → `None` | 编译错(无 `NativeRegistry`) | `registry.rs` 基本类型 + new + resolve |
| 2 | `registry_roundtrip`:register 一条 → resolve 命中同 fn | resolve 未实现或返错 | register + resolve |
| 3 | `registry_upsert`:同 (class,name,desc) register 两次不同 fn → resolve 返后者 | register 是 push 非 upsert | upsert 分支 |
| 4 | `natives_macro_populates`:test module 用 `natives!` 登记 2 条 → 调 register → resolve 双中 | 无 `natives!` 宏 | `macro_rules! natives` |
| 5 | `sync_assertions` 扩 `NativeRegistry: Send+Sync` | 断言缺类型 | 类型已 Send+Sync(天然) |

迁移闸门:每迁完一个 module(§6 顺序),跑 `cargo test --lib --tests` 全绿 + `cargo clippy -D warnings` 净才进下一个。最终 83 全迁、全套绿、clippy 净、零 unsafe。

集成闸门复用:`tests/class_real_bytecode.rs`、ArrayList/HashMap 端到端、真 Integer/String 等已有
javac 闸门不变(它们经 native 分派,即回归覆盖)。

---

## 10. 风险与回滚

- **引导时序**:`bootstrap()` 须在第一次可能调 native 的解释执行前 `register_all`。→ 测试:引导后
  立即调一个已知 native(如 `System.currentTimeMillis`)断言不抛 ULE。
- **宏作用域**:`macro_rules! natives` 须在 `native/mod.rs` 内所有子 `mod` 声明**之前**,否则子模块
  文本作用域不可见。→ 迁移 sun_nio_fs(首例)即验证。
- **协变护栏**:闭包误捕获 → 编译错(明确),非静默。无运行时风险。
- **回滚**:纯重构、git 可逐 module 回退;无数据/语义变更。

---

## 11. 文件清单

| 操作 | 文件 |
|------|------|
| 新建 | `src/runtime/interpreter/native/registry.rs` |
| 改 | `src/runtime/interpreter/native/mod.rs`(宏 + register_all + invoke 体改 + 重导出 + 删 dispatch) |
| 改 | `src/runtime/interpreter/native/java_lang.rs`(及另外 6 个 module:match→natives!) |
| 改 | `src/runtime/vm.rs`(加 `native_registry` 字段 + Default) |
| 改 | `runtime/vm` 的 bootstrap 调用点(`register_all` wiring)——计划阶段定位确切文件 |
| 改 | `src/runtime/vm/native.rs` 或等价 VmThread impl(加 `native_resolve`)——计划阶段定位 |
| 改 | 现有 sync_assertions 测试(加 NativeRegistry) |
| 加 | `tests/native_registry.rs`(或并入既有测试模块)— §9 单元测试 |

(确切行号/文件边界在 writing-plans 阶段据当前代码核定。)
