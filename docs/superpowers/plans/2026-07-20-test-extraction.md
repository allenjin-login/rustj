# tests/src 公用提取(`testkit` + `cp_util`)实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `tests/`(64 文件,~51% 重复样板)与 `src/` 的常量池解析真重复,收敛为两处单点定义,并提供守卫宏/断言宏;行为逐位保持。

**Architecture:** (1) `src/runtime/interpreter/cp_util.rs`(pub(crate))收编 `field.rs`/`invoke.rs` 重复的 utf8/class_name/name_and_type 三函数;(2) `src/testkit/`(feature 门控)收编测试基础设施(compile/run/守卫/断言),宏用 `#[macro_export]` + `$crate` 跨 crate;(3) 一次性全量迁移 64 测试文件。Cargo `testkit` feature + dev-deps 自引用使 `cargo test` 无参启用。

**Tech Stack:** Rust edition 2024,零 unsafe(`#![deny(unsafe_code)]`),手写 4 空格(**不跑** `cargo fmt`),无新依赖。

**Spec:** `docs/superpowers/specs/2026-07-20-test-extraction-design.md`

---

## 文件结构

**新建:**
- `src/runtime/interpreter/cp_util.rs` — 常量池解析三函数(pub(crate),VM 用)。
- `src/testkit/mod.rs` — testkit 模块根 + pub use 汇总 + 宏 re-export。
- `src/testkit/env.rs` — `javac_available`/`find_javabase_jmod` + 守卫宏。
- `src/testkit/compile.rs` — compile/compile_dir/compile_and_load/load_dir。
- `src/testkit/runner.rs` — run 系列(高层 + 低层 `_raw`)。
- `src/testkit/lookup.rs` — find_method/utf8(测试 panic 版)。
- `src/testkit/args.rs` — Arg enum + set_args。
- `src/testkit/asserts.rs` — as_int 等 + 断言宏。

**修改:**
- `Cargo.toml` — 加 `testkit` feature + `[dev-dependencies]` 自引用。
- `src/lib.rs` — `#[cfg(any(test, feature="testkit"))] pub mod testkit;`。
- `src/runtime/interpreter/mod.rs` — 加 `pub(crate) mod cp_util;`。
- `src/runtime/interpreter/field.rs` — 删私有三函数,`use super::cp_util::{...}`。
- `src/runtime/interpreter/invoke.rs` — 同上。
- 64 个 `tests/*.rs` — 删私有辅助,`use rustj::testkit::*;`,守卫/断言换宏。

---

## Task 1: feature 机制 + 宏跨 crate 探针验证(关键风险)

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/testkit/mod.rs`(最小骨架)
- Create: `tests/_feature_probe.rs`(探针,验证后留作 smoke)

- [ ] **Step 1: Cargo.toml 加 feature + dev-deps 自引用**

在 `Cargo.toml` 末尾(bimap 依赖之后)追加:
```toml

[features]
# testkit:集成测试公用基础设施(compile/run/守卫宏/断言宏)。dev-deps 自引用使
# `cargo test` 无参启用;`cargo build`/release 不启用 → 不带测试 dead code。
testkit = []

[dev-dependencies]
# 自引用:让 `cargo test` 编译 tests/*.rs 时自动启用 testkit feature
#(cfg(test) 对集成测试无效,故走 feature + dev-deps)。
rustj = { path = ".", features = ["testkit"] }
```

- [ ] **Step 2: lib.rs 挂 testkit(cfg 门控)**

`src/lib.rs` 在 `pub mod runtime;`(line 17)后加:
```rust
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
```

- [ ] **Step 3: testkit/mod.rs 最小骨架(一个 pub fn + 一个宏)**

`src/testkit/mod.rs`:
```rust
//! 集成测试公用基础设施(守 VM 不用的 javac 编译/jmod 探测/run/守卫/断言)。
//!
//! feature 门控:仅 `cargo test`(经 dev-deps 自引用开 `testkit` feature)或
//! `--features testkit` 时编译;`cargo build`/release 不带。VM 运行时不依赖本模块。
//!
//! 用法(`tests/*.rs`):`use rustj::testkit::*;` 引入函数;宏经 `#[macro_export]`
//! 在 crate 根,`use rustj::testkit::*;` 亦经下方 `pub use` 引入。

pub fn probe() -> bool {
    true
}

#[macro_export]
macro_rules! probe_macro {
    () => {
        42
    };
}

pub use crate::probe_macro;
```

- [ ] **Step 4: 探针集成测试**

`tests/_feature_probe.rs`:
```rust
use rustj::testkit::*;

#[test]
fn feature_and_macro_visible_from_integration_test() {
    assert!(probe());
    assert_eq!(probe_macro!(), 42);
}
```

- [ ] **Step 5: 验证 `cargo test` 无参开 feature + 宏跨 crate 可用**

Run: `cargo test --test _feature_probe`
Expected: PASS(编译通过 + 测试绿)。

- [ ] **Step 6: 验证 `cargo build` 不编译 testkit(release 净)**

Run: `cargo build`
Expected: 编译成功(testkit 因 feature 未开而不编译;无警告)。

- [ ] **Step 7: 决策记录**

若 Step 5 PASS 且 Step 6 PASS → feature 机制 + 宏跨 crate **确认可用**,后续 `cargo test` 无参。
若 Step 5 FAIL(feature 未自动开)→ 回退:删 Step 1 的 `[dev-dependencies]` 自引用行,后续所有 `cargo test` 命令改 `cargo test --features testkit`,并在 `CLAUDE.md §4` 末尾注明"集成测试需 `--features testkit`"。
若宏跨 crate FAIL(`probe_macro` 不可见)→ 调整宏导出策略(改用 `#[macro_export]` + 全路径 `rustj::probe_macro!()`,不依赖 glob)。

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/lib.rs src/testkit/mod.rs tests/_feature_probe.rs
git commit -m "feat(testkit): feature 门控骨架 + 跨 crate 探针(Task1)" -m "Cargo testkit feature + dev-deps 自引用使 cargo test 无参启用;#[macro_export] 宏跨 crate 验证通过。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: cp_util.rs(常量池解析三函数,TDD)

**Files:**
- Create: `src/runtime/interpreter/cp_util.rs`
- Modify: `src/runtime/interpreter/mod.rs:17`(加 mod 声明)
- Test: `src/runtime/interpreter/cp_util.rs`(内联 `#[cfg(test)]`)

- [ ] **Step 1: 挂模块**

`src/runtime/interpreter/mod.rs` 在 `mod type_check;`(line 17)后加:
```rust
pub(crate) mod cp_util;
```

- [ ] **Step 2: 写失败测试(先红)**

创建 `src/runtime/interpreter/cp_util.rs`,只放测试骨架:
```rust
//! 常量池条目解析公用工具(VM 内部,跨 field/invoke 共享)。
//!
//! 提取自 `field.rs:48-74`/`invoke.rs:988-1014` 各自私有定义的逐字重复。

#[cfg(test)]
mod tests {
    use crate::classfile::Reader;
    use crate::constant_pool::ConstantPool;

    /// [1]Utf8"Pt" [2]Class{1} [3]Utf8"x" [4]Utf8"I" [5]NameAndType{3,4}
    fn cp_with_names() -> ConstantPool {
        let bytes = [
            0x00, 0x05, // count=5
            0x01, 0x00, 0x02, b'P', b't', // [1] "Pt"
            0x07, 0x00, 0x01, // [2] Class{1}
            0x01, 0x00, 0x01, b'x', // [3] "x"
            0x01, 0x00, 0x01, b'I', // [4] "I"
            0x0C, 0x00, 0x03, 0x00, 0x04, // [5] NameAndType{3,4}
        ];
        ConstantPool::parse(&mut Reader::new(&bytes)).unwrap()
    }

    #[test]
    fn utf8_decodes_string_entry() {
        let cp = cp_with_names();
        assert_eq!(super::utf8(&cp, 1).unwrap(), "Pt");
    }

    #[test]
    fn class_name_decodes_class_entry() {
        let cp = cp_with_names();
        assert_eq!(super::class_name(&cp, 2).unwrap(), "Pt");
    }

    #[test]
    fn name_and_type_decodes_name_and_descriptor() {
        let cp = cp_with_names();
        let (n, d) = super::name_and_type(&cp, 5).unwrap();
        assert_eq!(n, "x");
        assert_eq!(d, "I");
    }
}
```

- [ ] **Step 3: 跑测试验证失败**

Run: `cargo test --lib cp_util`
Expected: FAIL(编译错误:`utf8`/`class_name`/`name_and_type` 未定义)。

- [ ] **Step 4: 写最小实现(逐字搬自 field.rs)**

在 `cp_util.rs` 顶部(`#[cfg(test)]` 之前)加:
```rust
use crate::constant_pool::{ConstantPool, ConstantPoolEntry};

use super::VmError;

/// 取 `Utf8` 条目的字符串(owned)。
pub(crate) fn utf8(cp: &ConstantPool, index: u16) -> Result<String, VmError> {
    match cp.get(index)? {
        ConstantPoolEntry::Utf8(s) => Ok(s.clone()),
        _ => Err(VmError::BadConstant("期望 Utf8 条目")),
    }
}

/// 解析 `Class` 条目 → 类内部名。
pub(crate) fn class_name(cp: &ConstantPool, class_index: u16) -> Result<String, VmError> {
    let ConstantPoolEntry::Class { name_index } = cp.get(class_index)?
    else {
        return Err(VmError::BadConstant("常量池条目须为 Class"));
    };
    utf8(cp, *name_index)
}

/// 解析 `NameAndType` 条目 → `(名字, 描述符)`。
pub(crate) fn name_and_type(cp: &ConstantPool, index: u16) -> Result<(String, String), VmError> {
    let ConstantPoolEntry::NameAndType {
        name_index,
        descriptor_index,
    } = cp.get(index)?
    else {
        return Err(VmError::BadConstant("常量池条目须含 NameAndType"));
    };
    Ok((utf8(cp, *name_index)?, utf8(cp, *descriptor_index)?))
}
```

- [ ] **Step 5: 跑测试验证通过**

Run: `cargo test --lib cp_util`
Expected: 3 tests PASS。

- [ ] **Step 6: 全量回归(确保未破坏现有)**

Run: `cargo test --lib`
Expected: 全绿(cp_util 新增 + 原有 lib 测试不变)。

- [ ] **Step 7: Commit**

```bash
git add src/runtime/interpreter/cp_util.rs src/runtime/interpreter/mod.rs
git commit -m "refactor(interp): 提取 cp_util 常量池解析三函数(Task2)" -m "utf8/class_name/name_and_type 移入 pub(crate) cp_util;TDD 三单元测试绿。field/invoke 尚未改用(后续 Task)。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: field.rs 改用 cp_util

**Files:**
- Modify: `src/runtime/interpreter/field.rs:48-74`(删私有三函数)+ import

- [ ] **Step 1: 删私有三函数,改 use 引入**

`src/runtime/interpreter/field.rs`:
- 删 line 48-74(私有 `class_name`/`name_and_type`/`utf8` 三函数,含注释)。
- 在 import 区(line 19 `use super::{clinit, throw_exception, Interpreter, VmError};`)后加:
```rust
use super::cp_util::{class_name, name_and_type, utf8};
```
(调用处 `class_name(cp, ...)`/`name_and_type(cp, ...)`/`utf8(cp, ...)` 不变 —— use 引入同名,resolve_fieldref/resolve_class_name 内部调用自动指向 cp_util 版本。)

- [ ] **Step 2: 确认无残留引用**

Run: `cargo build`
Expected: 成功(无 "cannot find function" 错误)。

- [ ] **Step 3: 回归**

Run: `cargo test --lib field`
Expected: 2 tests PASS(field.rs 内 resolve_fieldref/resolve_class_name 单元测试)。
Run: `cargo test --lib`
Expected: 全绿。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/field.rs
git commit -m "refactor(interp): field.rs 改用 cp_util(Task3)" -m "删私有 utf8/class_name/name_and_type,use super::cp_util 引入。行为保持。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: invoke.rs 改用 cp_util

**Files:**
- Modify: `src/runtime/interpreter/invoke.rs:987-1014`(删私有三函数)+ import

- [ ] **Step 1: 删私有三函数,改 use 引入**

`src/runtime/interpreter/invoke.rs`:
- 删 line 987-1014(私有 `class_name`/`name_and_type`/`utf8` 三函数,含注释)。
  **保留** line 1017 的 `cp_utf8`(零分配版,栈轨迹用)与 line 935 的 `arg_to_slot`(invoke 专用)—— 这两个不重复,不删。
- 在 invoke.rs 的 import 区加(找到现有 `use super::...` 或 `use crate::constant_pool::...` 处):
```rust
use super::cp_util::{class_name, name_and_type, utf8};
```

- [ ] **Step 2: 确认编译**

Run: `cargo build`
Expected: 成功。
注:若 invoke.rs 内有 `resolve_methodref`(line 967,调 class_name/name_and_type)等调用,use 引入后自动指向 cp_util 版本,无需改调用点。

- [ ] **Step 3: 回归**

Run: `cargo test --lib`
Expected: 全绿。

- [ ] **Step 4: Commit**

```bash
git add src/runtime/interpreter/invoke.rs
git commit -m "refactor(interp): invoke.rs 改用 cp_util(Task4)" -m "删私有 utf8/class_name/name_and_type;保留 cp_utf8/arg_to_slot(invoke 专用,非重复)。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: testkit/env.rs(环境探测 + 守卫宏)

**Files:**
- Create: `src/testkit/env.rs`
- Modify: `src/testkit/mod.rs`(pub use + 宏 re-export)

- [ ] **Step 1: 写 env.rs**

`src/testkit/env.rs`:
```rust
//! 环境探测:`javac` 是否可用 + 本机 `java.base.jmod` 定位;守卫宏。

use std::path::{Path, PathBuf};
use std::process::Command;

/// `javac` 是否在 PATH(集成测试需编译真 Java 源)。
pub fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 找本机首个 `java.base.jmod`;无则 `None`。
pub fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java")
            .join(ver)
            .join("jmods/java.base.jmod");
        if p.exists() {
            return Some(p);
        }
    }
    std::env::var("JAVA_HOME")
        .ok()
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

/// 守卫:无 `javac` 则 `eprintln!` + early-return 跳过当前 `#[test]`。
///
/// 必须**宏**(early-return `return` 仅在宏展开处的 `#[test]` 函数内有效;函数封装做不到)。
#[macro_export]
macro_rules! require_javac {
    () => {
        if !$crate::testkit::env::javac_available() {
            eprintln!("跳过:未找到 javac");
            return;
        }
    };
}

/// 守卫:找 `java.base.jmod` 绑定到 `$var`;无则 `eprintln!` + early-return。
#[macro_export]
macro_rules! require_javabase {
    ($var:ident) => {
        let Some($var) = $crate::testkit::env::find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
    };
}
```

- [ ] **Step 2: mod.rs 加 pub use + 宏 re-export**

`src/testkit/mod.rs` 在 `probe`/`probe_macro` 之后(保留探针,或删探针改为正式内容)替换为:
```rust
//! 集成测试公用基础设施(守 VM 不用的 javac 编译/jmod 探测/run/守卫/断言)。
//!
//! feature 门控:仅 `cargo test`(经 dev-deps 自引用开 `testkit` feature)或
//! `--features testkit` 时编译;`cargo build`/release 不带。VM 运行时不依赖本模块。
//!
//! 用法(`tests/*.rs`):`use rustj::testkit::*;` 引入函数;宏经 `#[macro_export]`
//! 在 crate 根,下方 `pub use` 使 glob 亦引入。

pub mod args;
pub mod asserts;
pub mod compile;
pub mod env;
pub mod lookup;
pub mod runner;

pub use args::{set_args, Arg};
pub use asserts::{as_double, as_float, as_int, as_long};
pub use compile::{compile, compile_and_load, compile_dir, load_dir};
pub use env::{find_javabase_jmod, javac_available};
pub use lookup::{find_method, utf8};
pub use runner::{run, run_err, run_raw_int, run_raw_value, run_result, run_static_in, run_static_int};

// 宏经 #[macro_export] 在 crate 根;此处 re-export 使 `use rustj::testkit::*;` 引入。
pub use crate::{assert_int, assert_is_thrown, assert_long, assert_double, assert_float, assert_throws, probe_macro,
    require_javabase, require_javac};
```
(注:`probe_macro` 与 `probe` 在 Step 1 的 mod.rs 已建为探针;此处替换为正式内容,删 `probe`/`probe_macro` 定义。Task 1 探针 `tests/_feature_probe.rs` 仍可保留作 smoke,或在此 Task 删除 —— 选择保留,改其 import 为正式 API。)

- [ ] **Step 3: 临时注释掉未建子模块**

Step 2 的 `pub use` 引用 args/asserts/compile/lookup/runner(尚未建)。**临时**在 mod.rs 把这些 `pub mod`/`pub use` 行注释掉,只留 env:
```rust
pub mod env;
pub use env::{find_javabase_jmod, javac_available};
pub use crate::{require_javabase, require_javac};
```
(后续 Task 6-9 建子模块时逐个取消注释。)

- [ ] **Step 4: 更新探针测试验证 env + 宏**

`tests/_feature_probe.rs` 改为:
```rust
use rustj::testkit::*;

#[test]
fn env_and_guard_macros_visible() {
    // javac_available 不 panic 即可(本机有无 javac 都行)。
    let _ = javac_available();
    let _ = find_javabase_jmod();
    // 宏可见性:require_javac! 展开(若 javac 在则继续,不在则 return 跳过本测试)。
    require_javac!();
}
```

- [ ] **Step 5: 验证**

Run: `cargo test --test _feature_probe`
Expected: PASS(或因无 javac 跳过,均算通过)。
Run: `cargo build`
Expected: 成功。

- [ ] **Step 6: Commit**

```bash
git add src/testkit/env.rs src/testkit/mod.rs tests/_feature_probe.rs
git commit -m "feat(testkit): env.rs 环境探测 + 守卫宏(Task5)" -m "javac_available/find_javabase_jmod + require_javac!/require_javabase! 宏(#[macro_export] 跨 crate)。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 6: testkit/compile.rs

**Files:**
- Create: `src/testkit/compile.rs`
- Modify: `src/testkit/mod.rs`(取消 compile 注释)

- [ ] **Step 1: 写 compile.rs**

`src/testkit/compile.rs`:
```rust
//! javac 编译辅助 + `.class` 载入 registry。目录名统一 `rustj-test-{name}-{seq}-{pid}`。

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::classfile::parse;
use crate::oops::ClassRegistry;

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 唯一临时目录(`rustj-test-{name}-{seq}-{pid}`)。
fn temp_dir(name: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("rustj-test-{n}-{pid}-{name}", pid = std::process::id()))
}

/// javac 编译 `source` 到唯一目录,返回该目录。`extra` 追加 javac 参数(如 `--add-exports`)。
pub fn compile_dir(source: &str, name: &str, extra: &[&str]) -> PathBuf {
    let dir = temp_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .args(extra)
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// 编单类,返回其 `.class` 路径(最常用)。
pub fn compile(source: &str, name: &str) -> PathBuf {
    let dir = compile_dir(source, name, &[]);
    dir.join(format!("{name}.class"))
}

/// 把 `dir` 下所有 `.class` 解析并载入 `reg`。
pub fn load_dir(reg: &mut ClassRegistry, dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功"))
                .expect("加载应成功");
        }
    }
}

/// 编多类 + 全部 `.class` 载入新 `ClassRegistry`。
pub fn compile_and_load(source: &str, name: &str) -> ClassRegistry {
    let dir = compile_dir(source, name, &[]);
    let mut reg = ClassRegistry::new();
    load_dir(&mut reg, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    reg
}
```

- [ ] **Step 2: mod.rs 取消 compile 注释**

`src/testkit/mod.rs` 取消 `pub mod compile;`、`pub use compile::{compile, compile_and_load, compile_dir, load_dir};` 的注释。

- [ ] **Step 3: 验证编译**

Run: `cargo build`
Expected: 成功(若 `ClassRegistry::new`/`.load` 签名与上不符,按编译错误微调 —— load 接 `ClassFile`,见 `clinit.rs:52` 用法)。

- [ ] **Step 4: Commit**

```bash
git add src/testkit/compile.rs src/testkit/mod.rs
git commit -m "feat(testkit): compile.rs 编译+载入辅助(Task6)" -m "compile/compile_dir/compile_and_load/load_dir;SEQ 内化;统一目录名 rustj-test-{name}-{seq}-{pid}。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 7: testkit/runner.rs(高层 + 低层)

**Files:**
- Create: `src/testkit/runner.rs`
- Modify: `src/testkit/mod.rs`(取消 runner/args/lookup 注释)
- Create: `src/testkit/lookup.rs`、`src/testkit/args.rs`(runner 依赖)

- [ ] **Step 1: 写 lookup.rs**

`src/testkit/lookup.rs`:
```rust
//! ClassFile 查找辅助(测试侧,panic 版;区别于 VM 的 cp_util Result 版)。

use crate::constant_pool::ConstantPoolEntry;
use crate::metadata::{ClassFile, MethodInfo};

/// 按 (名字, 描述符) 找方法;未找到 panic。
pub fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 取 Utf8 条目字符串(panic 版)。
pub fn utf8(cf: &ClassFile, index: u16) -> String {
    match cf.constant_pool.get(index).unwrap() {
        ConstantPoolEntry::Utf8(s) => s.clone(),
        e => panic!("expected Utf8 at {index}, got {e:?}"),
    }
}
```

- [ ] **Step 2: 写 args.rs**

`src/testkit/args.rs`:
```rust
//! 实参按 JVM 槽位约定写入 locals(long/double 占两槽)。

use crate::runtime::Frame;

/// 实参(int/long/float/double)。仅供低层 `run_raw_value` 用。
pub enum Arg {
    I(i32),
    L(i64),
    F(f32),
    D(f64),
}

/// 按 JVM 槽位约定(`I`/`F`=1 槽,`L`/`D`=2 槽)把 `args` 顺序写入 frame locals。
pub fn set_args(frame: &mut Frame, args: &[Arg]) {
    let mut slot: u16 = 0;
    for a in args {
        match a {
            Arg::I(v) => {
                frame.locals.set_int(slot, *v).unwrap();
                slot += 1;
            }
            Arg::L(v) => {
                frame.locals.set_long(slot, *v).unwrap();
                slot += 2;
            }
            Arg::F(v) => {
                frame.locals.set_float(slot, *v).unwrap();
                slot += 1;
            }
            Arg::D(v) => {
                frame.locals.set_double(slot, *v).unwrap();
                slot += 2;
            }
        }
    }
}
```

- [ ] **Step 3: 写 runner.rs**

`src/testkit/runner.rs`:
```rust
//! 执行静态方法辅助。两层:
//! - 高层(经 VmThread + interpret_with,完整 VM 语义):run/run_result/run_err/run_static_in/run_static_int
//! - 低层(不经 VmThread,直接 Frame + Interpreter::interpret,只测纯指令):run_raw_int/run_raw_value

use std::sync::Arc;

use crate::metadata::ClassFile;
use crate::oops::ClassRegistry;
use crate::runtime::{Frame, Interpreter, Value, VmError, VmThread};

use super::args::{set_args, Arg};
use super::lookup::find_method;

// ── 高层(经 VmThread)──────────────────────────────────────────────

/// 运行 `class.name(desc)`,自建 VmThread + 异常表;异常 panic。
pub fn run(reg: &Arc<ClassRegistry>, class: &str, name: &str, desc: &str) -> Value {
    run_result(reg, class, name, desc).0.unwrap_or_else(|e| panic!("{class}.{name}{desc} 执行失败:{e}"))
}

/// 同 [`run`] 但保留 `Result`(含 `Err`)+ 产出 `VmThread`(供读堆上异常)。
pub fn run_result(
    reg: &Arc<ClassRegistry>,
    class: &str,
    name: &str,
    desc: &str,
) -> (Result<Value, VmError>, VmThread) {
    let lc = reg.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let mut vm = VmThread::new(Arc::clone(reg));
    (interp.interpret_with(&mut frame, &mut vm), vm)
}

/// 期望失败:返回 `VmError`。
pub fn run_err(reg: &Arc<ClassRegistry>, class: &str, name: &str, desc: &str) -> VmError {
    run_result(reg, class, name, desc).0.expect_err("期望失败")
}

/// 复用调用方 `VmThread`(守静态字段句柄同堆约束:静态字段值是 Vm 堆句柄,堆随 Vm 析构失效,
/// 故引导与运行须同一 Vm —— 见 real_integer.rs 注释)。
pub fn run_static_in(
    vm: &mut VmThread,
    class: &str,
    name: &str,
    desc: &str,
) -> Result<Value, VmError> {
    let lc = vm
        .registry()
        .unwrap_or_else(|| panic!("类注册表缺失"))
        .get(class)
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    interp.interpret_with(&mut frame, vm)
}

/// 高层便利 int 版(解 Value::Int)。
pub fn run_static_int(vm: &mut VmThread, class: &str, name: &str) -> Result<i32, VmError> {
    match run_static_in(vm, class, name, "()I")? {
        Value::Int(v) => Ok(v),
        other => Err(VmError::BadConstant("run_static_int 期望 int,得 {other:?}")),
    }
}

// ── 低层(不经 VmThread,直接 Interpreter::interpret)──────────────

/// 喂 Frame 给 `Interpreter::interpret`(纯指令,无 `<clinit>`/堆/异常表),解 `Value::Int`。
pub fn run_raw_int(cf: &ClassFile, name: &str, desc: &str, args: &[i32]) -> i32 {
    let method = find_method(cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    for (i, &arg) in args.iter().enumerate() {
        frame.locals.set_int(i as u16, arg).unwrap();
    }
    let interp = Interpreter::new(&code.code, &cf.constant_pool);
    match interp.interpret(&mut frame) {
        Ok(Value::Int(v)) => v,
        Ok(other) => panic!("{name} 返回非 int:{other:?}"),
        Err(e) => panic!("{name} 执行失败:{e}"),
    }
}

/// 按 `Arg` 槽位写 locals,执行,返回 `Value`(低层)。
pub fn run_raw_value(cf: &ClassFile, name: &str, desc: &str, args: &[Arg]) -> Value {
    let method = find_method(cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    set_args(&mut frame, args);
    let interp = Interpreter::new(&code.code, &cf.constant_pool);
    interp.interpret(&mut frame).unwrap_or_else(|e| panic!("{name} 执行失败:{e}"))
}
```

注:`run_static_int` 的 `BadConstant` 不能用 `{other:?}` 内插(`BadConstant(&str)`)—— 改为固定串:
```rust
other => Err(VmError::BadConstant("run_static_int 期望 int 返回")),
```
(Step 3 落地时用此修正版。)

- [ ] **Step 4: mod.rs 取消 runner/args/lookup 注释**

`src/testkit/mod.rs` 取消 `pub mod args; pub mod lookup; pub mod runner;` 与对应 `pub use` 的注释。

- [ ] **Step 5: 验证编译 + 回归**

Run: `cargo build`
Expected: 成功(若 `frame.locals.set_int` 等签名不符,按 clinit.rs/interpret_int_methods.rs 用法微调)。
Run: `cargo test --test _feature_probe`
Expected: PASS。

- [ ] **Step 6: Commit**

```bash
git add src/testkit/runner.rs src/testkit/lookup.rs src/testkit/args.rs src/testkit/mod.rs
git commit -m "feat(testkit): runner/lookup/args(Task7)" -m "高层 run/run_result/run_err/run_static_in/run_static_int + 低层 run_raw_int/run_raw_value;find_method/utf8(panic 版);Arg+set_args。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 8: testkit/asserts.rs(断言宏)

**Files:**
- Create: `src/testkit/asserts.rs`
- Modify: `src/testkit/mod.rs`(取消 asserts 注释)

- [ ] **Step 1: 写 asserts.rs**

`src/testkit/asserts.rs`:
```rust
//! 取值辅助 + 断言宏。

use crate::oops::Oop;
use crate::runtime::{Value, VmError};

/// 解 `Value::Int`;非 int panic。
pub fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

/// 解 `Value::Long`。
pub fn as_long(v: Value) -> i64 {
    match v {
        Value::Long(x) => x,
        other => panic!("期望 long,得 {other:?}"),
    }
}

/// 解 `Value::Double`。
pub fn as_double(v: Value) -> f64 {
    match v {
        Value::Double(x) => x,
        other => panic!("期望 double,得 {other:?}"),
    }
}

/// 解 `Value::Float`。
pub fn as_float(v: Value) -> f32 {
    match v {
        Value::Float(x) => x,
        other => panic!("期望 float,得 {other:?}"),
    }
}

/// 断言 `Value::Int(x)`,且 `x == $expected`。
#[macro_export]
macro_rules! assert_int {
    ($v:expr, $expected:expr) => {
        match $v {
            $crate::runtime::Value::Int(x) => assert_eq!(x, $expected),
            other => panic!("期望 Value::Int,得 {other:?}"),
        }
    };
}

/// 断言 `Value::Long(x)`,且 `x == $expected`。
#[macro_export]
macro_rules! assert_long {
    ($v:expr, $expected:expr) => {
        match $v {
            $crate::runtime::Value::Long(x) => assert_eq!(x, $expected),
            other => panic!("期望 Value::Long,得 {other:?}"),
        }
    };
}

/// 断言 `Value::Double(x)`,且 `|x - $expected| < 1e-9`。
#[macro_export]
macro_rules! assert_double {
    ($v:expr, $expected:expr) => {
        match $v {
            $crate::runtime::Value::Double(x) => assert!((x - $expected).abs() < 1e-9),
            other => panic!("期望 Value::Double,得 {other:?}"),
        }
    };
}

/// 断言 `Value::Float(x)`,且 `|x - $expected| < 1e-6`。
#[macro_export]
macro_rules! assert_float {
    ($v:expr, $expected:expr) => {
        match $v {
            $crate::runtime::Value::Float(x) => assert!((x - $expected).abs() < 1e-6),
            other => panic!("期望 Value::Float,得 {other:?}"),
        }
    };
}

/// 断言 `result` 为 `Err(VmError::ThrownException(r))`,且堆对象 `class_name() == $expected`。
#[macro_export]
macro_rules! assert_throws {
    ($result:expr, $vm:expr, $expected:literal) => {
        let err = $result.unwrap_err();
        let $crate::runtime::VmError::ThrownException(exc) = err else {
            panic!("期望 ThrownException({}), 得 {:?}", $expected, err);
        };
        let Some($crate::oops::Oop::Instance(i)) = $vm.heap().get(exc) else {
            panic!("异常应为 Instance,引用 {exc:?}");
        };
        assert_eq!(i.class_name(), $expected, "异常类名不符");
    };
}

/// 断言 `VmError` 为 `ThrownException` 变体。
#[macro_export]
macro_rules! assert_is_thrown {
    ($err:expr) => {
        assert!(matches!($err, $crate::runtime::VmError::ThrownException(_)))
    };
}
```

注:`asserts.rs` 顶部 `use crate::oops::Oop; use crate::runtime::{Value, VmError};` 仅 `as_*` 函数用 `Value`;宏内用全路径(不依赖 use)。若编译警告 `Oop`/`VmError` 未用于非宏,则只 import `Value`:
```rust
use crate::runtime::Value;
```
(Step 1 落地时按编译警告裁剪 import。)

- [ ] **Step 2: mod.rs 取消 asserts 注释**

`src/testkit/mod.rs` 取消 `pub mod asserts;`、`pub use asserts::{as_double, as_float, as_int, as_long};` 注释。

- [ ] **Step 3: 验证**

Run: `cargo build`
Expected: 成功。
Run: `cargo test --test _feature_probe`
Expected: PASS。

- [ ] **Step 4: Commit**

```bash
git add src/testkit/asserts.rs src/testkit/mod.rs
git commit -m "feat(testkit): asserts.rs 取值+断言宏(Task8)" -m "as_int/as_long/as_double/as_float + assert_int!/assert_long!/assert_double!/assert_float!/assert_throws!/assert_is_thrown!。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 9: 迁移代表文件 clinit.rs + throw.rs(验证 API,Commit 1 收尾)

**Files:**
- Modify: `tests/clinit.rs`
- Modify: `tests/throw.rs`

- [ ] **Step 1: 迁移 clinit.rs**

对 `tests/clinit.rs` 做以下变换:
1. 删 `fn javac_available`(line 16-22)、`static SEQ`(line 24)、`fn compile_and_load`(line 27-58)、`fn find_method`(line 60-75)、`fn run`(line 79-82)、`fn run_result`(line 85-106)、`fn as_int`(line 108-113)、`fn assert_throws_class`(line 116-125)。
2. import 区改为(删 `use std::process::Command;` 与不再需要的 metadata/constant_pool import,保留 rustj 类型):
```rust
use rustj::oops::{ClassRegistry, Oop};
use rustj::runtime::{Value, VmError, VmThread};
use rustj::testkit::*;
```
3. 测试体内每个 `#[test]` 开头 `if !javac_available() { eprintln!("跳过:未找到 javac"); return; }` → `require_javac!();`。
4. `let reg = compile_and_load(SOURCE, "ClinitGate"); let reg = Arc::new(reg);` 不变(testkit::compile_and_load 同名同签名)。
5. `as_int(run(...))` → `as_int(run(...))`(不变,as_int 由 testkit glob 引入)。
6. `assert_throws_class(r, &vm, "java/lang/ExceptionInInitializerError")` → `assert_throws!(r, &vm, "java/lang/ExceptionInInitializerError")`。

- [ ] **Step 2: 验证 clinit.rs**

Run: `cargo test --test clinit`
Expected: 5 tests PASS(或无 javac 全跳过 —— 均算通过;若有 javac 须全绿)。

- [ ] **Step 3: 迁移 throw.rs**

对 `tests/throw.rs` 做同类变换:
1. 删 `fn javac_available`、`static SEQ`、`fn compile_and_load`、`fn find_method`、`fn run`、`fn run_err`、`fn as_int`、`fn is_thrown`。
2. import 改:
```rust
use rustj::oops::ClassRegistry;
use rustj::runtime::{Value, VmError, VmThread};
use rustj::testkit::*;
```
3. 守卫 → `require_javac!();`。
4. `is_thrown(run_err(...))` → 断言改写:`assert!(matches!(run_err(...), VmError::ThrownException(_)))`,或用宏 `assert_is_thrown!(run_err(...))`(后者更简洁,需作 statement:`let e = run_err(...); assert_is_thrown!(e);` —— 宏内 `assert!` 可作表达式 statement)。
   具体:`assert!(is_thrown(run_err(&reg, "ThrowGate", "uncaught", "()I")));` → `assert_is_thrown!(run_err(&reg, "ThrowGate", "uncaught", "()I"));`

- [ ] **Step 4: 验证 throw.rs**

Run: `cargo test --test throw`
Expected: 12 tests PASS(或无 javac 跳过)。

- [ ] **Step 5: 全量回归 + clippy**

Run: `cargo test`
Expected: 全绿(clinit/throw 已迁移 + 探针 + 其余 62 文件未动仍绿)。
Run: `cargo clippy --all-targets -- -D warnings`
Expected: 净(若 clinit/throw 有未用 import 警告,裁剪)。

- [ ] **Step 6: Commit(Commit 1 收尾)**

```bash
git add tests/clinit.rs tests/throw.rs
git commit -m "refactor(tests): clinit/throw 迁移上 testkit(Task9,Commit1 收尾)" -m "删私有辅助,use rustj::testkit::*;守卫 require_javac!,断言 assert_throws!/assert_is_thrown!。验证 API 可用;全测试绿、clippy 净。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 10: 全量迁移剩余 62 文件(Commit 2 主体)

**Files:** `tests/` 下除 clinit.rs/throw.rs/_feature_probe.rs 外的全部(见下方分组)。

**迁移变换规则(通用,每个文件套用):**

| 旧 | 新 |
|---|---|
| 文件内 `fn javac_available` | 删;`use rustj::testkit::*;` 已含 |
| 文件内 `fn find_javabase_jmod` | 删 |
| 文件内 `static SEQ`/`COMPILE_SEQ` | 删(SEQ 内化于 testkit::compile) |
| 文件内 `fn compile`/`compile_and_load`/`compile_dir`/`compile_and_load_all` | 删;改调 testkit 同名(注意:旧 `compile_and_load_all` → `compile_and_load`;旧 `compile_dir` 带 extra 签名已匹配) |
| 文件内 load `.class` 循环 | 删;用 `compile_and_load`(返回 registry)或 `load_dir(&mut reg, &dir)` |
| 文件内 `fn find_method`/`utf8` | 删;testkit 已含 |
| 文件内 `fn run`/`run_result`/`run_err`/`run_static_in`/`run_static_int` | 删;testkit 已含(注意签名须匹配 —— 见特例) |
| 文件内 `fn as_int`/`as_long`/`as_double`/`as_float` | 删;testkit 已含 |
| 文件内 `enum Arg` + set 逻辑 | 删;用 testkit::Arg + run_raw_value(低层)或直接(高层) |
| `use std::process::Command;` | 删(javac_available 已移走) |
| `if !javac_available() { eprintln!("跳过..."); return; }` | `require_javac!();` |
| `let Some(jmod) = find_javabase_jmod() else { eprintln!("跳过..."); return; };` | `require_javabase!(jmod);` |
| `assert_throws_class(r, &vm, "x")`(自定义) | `assert_throws!(r, &vm, "x")` |
| `is_thrown(err)` 谓词 | `assert_is_thrown!(err);`(statement)或 `matches!(err, VmError::ThrownException(_))` |
| 自定义 `assert_*` helper | 改用 testkit 宏;无对应则保留 |

import 统一模板(按文件实际用到的类型裁剪):
```rust
use rustj::testkit::*;
// 按需(原文件用了哪些 rustj 类型就留哪些):
use rustj::classfile::parse;            // 若仍直接 parse
use rustj::oops::ClassRegistry;         // 若声明 reg 变量类型
use rustj::runtime::{Value, VmError, VmThread};  // 按需
```

**特例文件(需额外注意):**

1. `tests/real_integer.rs`:
   - 用 `run_static_in`(同堆约束)—— testkit 同名,签名匹配。
   - 文件内还**内联**了 find_method(找 IntegerGate.run)—— 改用 `testkit::find_method`。
   - `compile_dir(SOURCE, name, &[])` / `compile_dir(BOOTSTRAP_SRC, ..., &["--add-exports", ...])` —— testkit::compile_dir 签名 `(src, name, extra: &[&str])` 匹配。
   - 保留 `BOOTSTRAP_SRC`/`SOURCE` 常量与引导逻辑(run_static_in 跑 RustjBootstrap.init)。

2. `tests/interpret_int_methods.rs`:
   - **低层**(不经 VmThread):`run_static_int(cf, name, desc, &[i32])` → `run_raw_int(cf, name, desc, &[i32])`;`run_static_value(cf, name, desc, &[Arg])` → `run_raw_value(cf, name, desc, &[Arg])`。
   - 删文件内 `enum Arg`、`fn run_static_int`、`fn run_static_value`、`fn find_method`、`fn utf8`、`fn compile`(返回 PathBuf 版 —— 注意此文件 `compile` 返回 .class 路径,与 testkit::compile 一致)、`fn javac_available`。

3. `tests/interpret_method_invocation.rs` / `tests/object_fields.rs`:
   - 同 interpret_int_methods(低层 + Arg)。

4. 用 `run_static_int(vm, class, name)`(**高层**,经 VmThread,如 reflection_*/filesystem_*):
   - testkit::run_static_int 签名 `(vm: &mut VmThread, class, name) -> Result<i32, VmError>` 匹配。

5. `tests/parse_real_class.rs` / `tests/module_info_parse.rs`:
   - 若只用 `parse`(不 compile/run)—— 仅删 javac_available/守卫,加 `use rustj::testkit::*;`(或只引 `require_javac!`)。

**分组(逐组迁移,每组跑测试验证再下一组):**

- **组 A(纯解析/无 VM):** parse_real_class, module_info_parse
- **组 B(低层纯指令):** interpret_int_methods, interpret_method_invocation, control_flow, dup_stack_ops, areturn, arrays, multianewarray, multi_arg_invoke, int_string_round_trip, object_fields
- **组 C(异常/栈迹):** throwable_message, stack_trace, stack_trace_elements, array_store_ase
- **组 D(真 java.base 集合/字符串/indy):** arraylist_end_to_end, hashmap_end_to_end, real_integer, real_string_ops, string_builder_append, string_concat, string_literals, lambda_end_to_end, indy_concat, method_ref_end_to_end, method_handle_lf_invoke, native_real_object
- **组 E(反射):** g0_reflection_classfile_probe, reflection_declared, reflection_field_getset, reflection_field_invoke, reflection_field_unreflect, reflection_forname, reflection_invoke, reflection_methods
- **组 F(线程):** thread_constructor, thread_interrupt, thread_mirror, thread_start_join, thread_uncaught, object_wait_notify
- **组 G(模块/类加载):** classloader_load_class, module_get_module, module_layer_boot, system_classloader
- **组 H(杂项):** system_arraycopy, unsafe_offset_reads, synchronized_block, virtual_dispatch, interface_dispatch, checkcast, class_mirror, class_real_bytecode, static_property_encodings, vm_system_bootstrap, filesystem_attributes, filesystem_canonicalize, stream_pipeline_probe, bmh_clinit_probe, javabase_full_load, stress_real_project

(共 62 文件;若实际数与分组有出入,以 `tests/` 目录实际未迁移文件为准。)

- [ ] **Step 1..N: 逐组迁移 + 验证**

对每组:
1. 按变换规则迁移组内每个文件(删私有辅助、改 import、守卫换宏、断言换宏、特例按上处理)。
2. Run: `cargo test --test <file>`(组内每文件)。
   Expected: 全 PASS(或无 javac/jmod 跳过)。
3. 组内全绿后进入下一组。

- [ ] **Step last: 全量回归**

Run: `cargo test`
Expected: 全绿(所有 64 文件 + lib 单元测试)。

- [ ] **Step: Commit(Commit 2)**

```bash
git add tests/
git commit -m "refactor(tests): 全量迁移 62 文件上 testkit(Task10,Commit2)" -m "删各文件私有 javac_available/compile*/run*/find_method/utf8/as_int/find_javabase_jmod/SEQ/Arg,统一 use rustj::testkit::*;守卫 require_javac!/require_javabase!,断言 assert_* 宏。低层 _raw、高层 run_static_in 同堆约束均保留。" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 11: 最终闸门 + 收尾

**Files:** 无新改(验证);可选删 `tests/_feature_probe.rs`。

- [ ] **Step 1: clippy 全净**

Run: `cargo clippy --all-targets --features testkit -- -D warnings`
Expected: 净(无警告)。若 Task 1 决策为无参可用,则 `cargo clippy --all-targets -- -D warnings`。

- [ ] **Step 2: 全测试绿**

Run: `cargo test`
Expected: 全绿。

- [ ] **Step 3: 零 unsafe 保持**

Run: `cargo build` (release,不带 testkit feature)
Expected: 成功(`#![deny(unsafe_code)]` 仍通过;testkit/cp_util 不引入 unsafe)。

- [ ] **Step 4: 重复消除核验**

Run(grep 确认各辅助仅 testkit/cp_util 一处):
```
grep -rl "fn javac_available" tests/    # 期望:空
grep -rl "fn find_javabase_jmod" tests/ # 期望:空
grep -rl "fn compile_and_load" tests/   # 期望:空
grep -rl "static SEQ" tests/            # 期望:空
grep -rl "fn class_name" src/runtime/interpreter/field.rs src/runtime/interpreter/invoke.rs  # 期望:空
```
Expected: tests/ 内无私有辅助定义;field.rs/invoke.rs 无私有 class_name。

- [ ] **Step 5: 处理探针 + 收尾 commit**

决定 `tests/_feature_probe.rs`:保留(作 feature/宏 smoke)或删除(其职责已被各迁移文件覆盖)。若删:
```bash
git rm tests/_feature_probe.rs
```
若 Task 1 Step 7 决策为"回退 --features testkit",确认 CLAUDE.md §4 已注明。

最终收尾 commit(若 Step 5 有改动):
```bash
git commit -m "chore(testkit): 收尾(删探针/CLAUDE 注明)" -m "Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review(写计划后自检)

**1. Spec 覆盖:**
- §3.1 cp_util 三函数 → Task 2/3/4 ✓
- §3.2 testkit feature 门控 → Task 1 ✓
- §4.1 env + 守卫宏 → Task 5 ✓
- §4.2 compile 分层 → Task 6 ✓
- §4.3 runner 高层+低层 → Task 7 ✓
- §4.4 lookup → Task 7 ✓
- §4.5 args → Task 7 ✓
- §4.6 asserts + 断言宏 → Task 8 ✓
- §5 Commit 1(基建+代表)→ Task 1-9 ✓
- §5 Commit 2(全量)→ Task 10 ✓
- §7 闸门 → Task 11 ✓

**2. 占位符扫描:** 无 TBD/TODO;Task 10 的"按变换规则套用"是机械迁移的规则描述(非占位符),特例文件给了精确处理。

**3. 类型一致:** run_static_int 在 Task 7(高层,`(vm, class, name)`)与 Task 10 特例 4 一致;run_raw_int 在 Task 7 与 Task 10 特例 2 一致;compile_dir 签名 Task 6 与 Task 10 特例 1 一致。

**4. 已知风险点(实现时关注):**
- Task 1 feature 机制(若自引用 dev-deps 不生效 → 回退序明确)。
- Task 7 run_static_int 的 BadConstant 串(Step 3 已标注修正)。
- Task 8 asserts.rs import 裁剪(Step 1 已标注)。
- Task 10 特例文件(real_integer 内联 find_method、低层 _raw、同堆约束)。
