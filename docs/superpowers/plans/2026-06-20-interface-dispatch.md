# Layer 4.2b 接口分派 + invokespecial 完整语义 + StackOverflowError 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让解释器执行使用接口(含 default 方法)、私有方法、`super.` 调用的真实 Java 程序,并能优雅返回 `StackOverflowError`(不 panic),结果与 JVM 一致。

**Architecture:** invokeinterface/invokevirtual 共享"类链先行 → 接口 default 兜底"的分派解析(`resolve_dispatch`);invokespecial 按 `<init>`/私有/super 三支分派;`Vm` 增可配置帧深度计数 + `run_with_depth` 守卫,递归调用栈仍用 Rust 栈。沿用 4.1/4.2 的 `'a` 借用模式与 id-arena 堆,零 unsafe。

**Tech Stack:** Rust edition 2024,`#![deny(unsafe_code)]`,RefCell 内部可变性,javac 集成闸门。

参考设计:`docs/superpowers/specs/2026-06-20-interface-dispatch-design.md`。

---

## 文件结构

| 文件 | 责任 | 改动 |
|------|------|------|
| `src/runtime/interpreter/mod.rs` | 指令分派 + `VmError` | 增 `AbstractMethodError`/`StackOverflow` 变体 + Display;增 `Invokeinterface` 臂(`pc += 5`) |
| `src/runtime/vm.rs` | 执行上下文 | 增 `frame_depth`/`stack_limit` 字段 + `DEFAULT_STACK_LIMIT` + `with_stack_limit` |
| `src/runtime/interpreter/invoke.rs` | 方法调用 | 增 `run_with_depth`/`run_callee`/`apply_return`;四 invoke 函数统一走它们;新增 `invoke_interface`;`invoke_special` 扩私有/super 分支;`invoke_virtual` 增 default 兜底 |
| `src/oops/klass.rs` | 已加载类 + 注册表 | `LoadedClass::interface_names()`;`ClassRegistry::find_exact_method`/`find_default_method`/`resolve_dispatch` |
| `tests/interface_dispatch.rs`(新) | 集成闸门 | javac 编译接口+default+私有+super 层次,真实执行 |

**关键约束(TDD):** 每个新函数先写失败测试看红,再实现看绿。集成闸门先红(unsupported opcode / 未找到方法)再绿。

---

## Task 1: `VmError` 新变体(`mod.rs`)

**Files:**
- Modify: `src/runtime/interpreter/mod.rs:31-47`(枚举)、`:49-61`(Display)

- [ ] **Step 1: 增枚举变体**

在 `pub enum VmError { ... NullPointer, }` 的 `NullPointer` 后追加两变体:

```rust
    /// NullPointerException:对 null 引用取字段/数组/调用方法。
    NullPointer,
    /// AbstractMethodError:invokeinterface/invokevirtual 命中抽象方法(无 Code)。
    AbstractMethodError,
    /// StackOverflowError:帧嵌套深度超 stack_limit。
    StackOverflow,
```

- [ ] **Step 2: 增 Display 分支**

在 `impl Display` 的 `Self::NullPointer => ...` 后追加:

```rust
            Self::NullPointer => write!(f, "NullPointerException"),
            Self::AbstractMethodError => write!(f, "AbstractMethodError"),
            Self::StackOverflow => write!(f, "StackOverflowError"),
```

- [ ] **Step 3: 编译确认**

Run: `cargo build --lib`
Expected: 编译通过(纯枚举追加)。

- [ ] **Step 4: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "Layer 4.2b:VmError 增 AbstractMethodError/StackOverflow 变体"
```

---

## Task 2: `Vm` 深度计数 + `run_with_depth` 守卫

**Files:**
- Modify: `src/runtime/vm.rs`
- Modify: `src/runtime/interpreter/invoke.rs`(新 `run_with_depth` + 包裹三既有 invoke)
- Test: `src/runtime/interpreter/invoke.rs`(单测)

- [ ] **Step 1: 写失败测试(`invoke.rs` 的 `#[cfg(test)] mod tests`)**

在 `invoke.rs` 测试模块末尾追加(`run_with_depth` 尚不存在 → 编译失败 = 红):

```rust
    #[test]
    fn run_with_depth_counts_symmetrically() {
        // Ok 路径:进入 +1、退出 −1。
        let mut vm = crate::runtime::Vm::default();
        let n = crate::runtime::interpreter::run_with_depth(&mut vm, |vm| {
            // 嵌套两层,验证深度递增
            let d1 = vm.frame_depth;
            let inner = crate::runtime::interpreter::run_with_depth(vm, |vm| {
                Ok(vm.frame_depth)
            });
            let d2 = inner.unwrap();
            assert_eq!(d1, 1);
            assert_eq!(d2, 2);
            Ok(())
        });
        assert!(n.is_ok());
    }

    #[test]
    fn run_with_depth_overflow_returns_stackoverflow() {
        // Err 路径仍对称减计;超限 → StackOverflow。
        let mut vm = crate::runtime::Vm::default().with_stack_limit(2);
        let r = crate::runtime::interpreter::run_with_depth(&mut vm, |vm| {
            crate::runtime::interpreter::run_with_depth(vm, |vm| {
                crate::runtime::interpreter::run_with_depth(vm, |_| Ok(()))
            })
        });
        // 深度 2 已达 limit(2),第三层 → StackOverflow
        assert_eq!(r.unwrap_err(), crate::runtime::interpreter::VmError::StackOverflow);
        assert_eq!(vm.frame_depth, 0); // 异常路径也对称归零
    }
```

注:`run_with_depth` 与 `VmError` 需 `pub(crate)` 可见(见 Step 3)。

- [ ] **Step 2: 运行测试看红**

Run: `cargo test --lib run_with_depth`
Expected: 编译失败(`run_with_depth` 未定义 / `with_stack_limit` / `frame_depth` 不存在)。

- [ ] **Step 3: `vm.rs` 增字段与访问**

改 `src/runtime/vm.rs`。**移除** `#[derive(Default)]`(改手写 Default 以设 `stack_limit`):

```rust
//! 执行上下文:对象堆 + 类注册表 + 帧深度计数。对应 HotSpot `JavaThread`
//! 执行所需的共享状态 + 栈深度检查。
//!
//! 4.1:纯数值路径可不带注册表([`Vm::default`] 空堆 + 无注册表);对象/字段/
//! `invokestatic` 路径需注册表([`Vm::new`])。4.2b:帧深度计数 + 可配置上限
//! ([`Vm::with_stack_limit`])用于 [`VmError::StackOverflow`](crate::runtime::interpreter::VmError)。

use crate::oops::ClassRegistry;
use crate::runtime::heap::Heap;

/// 默认帧深度上限。高于 ackermann(3,3) 的递归深度(~120),正常小测试不会误触;
/// 可经 [`Vm::with_stack_limit`] 调整(SOE 测试用小值快速触发)。
pub const DEFAULT_STACK_LIMIT: u32 = 512;

/// 执行上下文:拥有对象堆,借用类注册表,跟踪帧嵌套深度。
pub struct Vm<'a> {
    heap: Heap,
    registry: Option<&'a ClassRegistry>,
    /// 当前嵌套帧数(进入一帧 +1,退出 −1)。
    pub(crate) frame_depth: u32,
    /// 帧深度上限;`frame_depth >= stack_limit` 时再调用 → StackOverflow。
    pub(crate) stack_limit: u32,
}

impl<'a> Vm<'a> {
    /// 构造带类注册表的 Vm(空堆,默认深度上限)。
    pub fn new(registry: &'a ClassRegistry) -> Self {
        Self {
            heap: Heap::new(),
            registry: Some(registry),
            frame_depth: 0,
            stack_limit: DEFAULT_STACK_LIMIT,
        }
    }

    /// 设置帧深度上限(builder)。SOE 测试用小值快速触发。
    pub fn with_stack_limit(mut self, limit: u32) -> Self {
        self.stack_limit = limit;
        self
    }

    /// 对象堆。
    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    /// 对象堆(可变)。
    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// 类注册表(若启用)。
    ///
    /// 返回的引用与注册表本身同寿命(`'a`),不依赖本次对 `self` 的借用——
    /// 这样取出 `&LoadedClass` 后仍可再借 `&mut self`(如递归 `interpret_with`)。
    pub fn registry(&self) -> Option<&'a ClassRegistry> {
        self.registry
    }
}

impl Default for Vm<'_> {
    fn default() -> Self {
        Self {
            heap: Heap::new(),
            registry: None,
            frame_depth: 0,
            stack_limit: DEFAULT_STACK_LIMIT,
        }
    }
}
```

- [ ] **Step 4: `invoke.rs` 增 `run_with_depth` + 包裹三既有 invoke**

在 `invoke.rs` 顶部 `use` 区后,`enum Arg` 之前,新增守卫(注意 `pub(crate)`,供测试):

```rust
/// 进入一帧:`frame_depth +1`,执行 `f`,返回前 `−1`(Ok/Err 两路对称)。
/// `frame_depth >= stack_limit` 时直接 [`VmError::StackOverflow`],不进入 `f`。
pub(crate) fn run_with_depth<R>(
    vm: &mut Vm<'_>,
    f: impl FnOnce(&mut Vm<'_>) -> Result<R, VmError>,
) -> Result<R, VmError> {
    if vm.frame_depth >= vm.stack_limit {
        return Err(VmError::StackOverflow);
    }
    vm.frame_depth += 1;
    let r = f(vm);
    vm.frame_depth -= 1;
    r
}
```

然后**包裹三既有 invoke 的递归调用**。把每处:
```rust
    let result = callee_interp.interpret_with(&mut callee, vm)?;
```
替换为:
```rust
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm))?;
```
(`invoke_static`、`invoke_special`、`invoke_virtual` 各一处。)`run_with_depth` 在同模块,直接调用。

- [ ] **Step 5: 运行新单测看绿**

Run: `cargo test --lib run_with_depth`
Expected: PASS(2 测试)。

- [ ] **Step 6: 确认既有套件不回归**

Run: `cargo test`
Expected: 全绿(含 ackermann 等既有递归测试,默认上限 512 高于其深度)。

- [ ] **Step 7: 提交**

```bash
git add src/runtime/vm.rs src/runtime/interpreter/invoke.rs
git commit -m "Layer 4.2b:Vm 帧深度计数 + run_with_depth 守卫(StackOverflow)"
```

---

## Task 3: `LoadedClass::interface_names()`

**Files:**
- Modify: `src/oops/klass.rs`(`impl LoadedClass`)
- Test: `src/oops/klass.rs`(单测)

- [ ] **Step 1: 写失败测试**

在 `klass.rs` 的 `#[cfg(test)] mod tests` 顶部(`use super::*;` 之后)增测试辅助 + 用例:

```rust
    use crate::classfile::Reader;
    use crate::classfile::attributes::CodeAttribute;
    use crate::constant_pool::ConstantPool;

    /// 构建常量池:先放 utf8s(索引从 1 起),再放 classes(每个 = 指向某 utf8 索引的 Class 条目)。
    fn mk_cp(utf8s: &[&str], classes: &[u16]) -> ConstantPool {
        let count = (utf8s.len() + classes.len() + 1) as u16;
        let mut b = count.to_be_bytes().to_vec();
        for s in utf8s {
            b.push(0x01);
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        for &name_idx in classes {
            b.push(0x07);
            b.extend_from_slice(&name_idx.to_be_bytes());
        }
        ConstantPool::parse(&mut Reader::new(&b)).unwrap()
    }

    fn mk_cf(
        cp: ConstantPool,
        this: u16,
        super_c: u16,
        interfaces: Vec<u16>,
        methods: Vec<MethodInfo>,
    ) -> ClassFile {
        ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: cp,
            access_flags: crate::metadata::AccessFlags::from_bits(0),
            this_class: this,
            super_class: super_c,
            interfaces,
            fields: Vec::new(),
            methods,
            attributes: Vec::new(),
        }
    }

    #[test]
    fn interface_names_resolves_cp_class_entries() {
        // utf8: [1]="C",[2]="java/lang/Object",[3]="I1",[4]="I2"
        // classes[1,2,3,4] → [5]=Class{1}="C",[6]=Class{2}=Object,[7]=Class{3}="I1",[8]=Class{4}="I2"
        let pool = mk_cp(&["C", "java/lang/Object", "I1", "I2"], &[1, 2, 3, 4]);
        let cf = mk_cf(pool, 5, 6, vec![7, 8], vec![]);
        let lc = LoadedClass::from_cf(cf).unwrap();
        assert_eq!(
            lc.interface_names(),
            vec!["I1".to_string(), "I2".to_string()]
        );
    }
```

- [ ] **Step 2: 运行看红**

Run: `cargo test --lib interface_names`
Expected: 编译失败(`interface_names` 未定义)。

- [ ] **Step 3: 实现 `interface_names()`**

在 `impl LoadedClass`(紧邻 `super_class_name()` 之后)增:

```rust
    /// 直接实现的接口内部名(解析 `cf.interfaces` 的 `Class` 条目)。
    pub fn interface_names(&self) -> Vec<String> {
        let cp = &self.cf.constant_pool;
        self.cf
            .interfaces
            .iter()
            .filter_map(|&idx| match cp.get(idx).ok()? {
                ConstantPoolEntry::Class { name_index } => match cp.get(*name_index).ok()? {
                    ConstantPoolEntry::Utf8(s) => Some(s.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
```

- [ ] **Step 4: 运行看绿**

Run: `cargo test --lib interface_names`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/oops/klass.rs
git commit -m "Layer 4.2b:LoadedClass::interface_names() 解析直接接口"
```

---

## Task 4: `ClassRegistry` 分派查找三方法

**Files:**
- Modify: `src/oops/klass.rs`(顶部 `use` + `impl ClassRegistry`)
- Test: `src/oops/klass.rs`(单测)

- [ ] **Step 1: 顶部增 `use`**

`klass.rs` 顶部 `use std::collections::HashMap;` 改为:

```rust
use std::collections::{HashMap, HashSet, VecDeque};
```

- [ ] **Step 2: 写失败测试**

在 `klass.rs` 测试模块追加辅助与三用例(`mk_method`/`default_code` + 三个测试):

```rust
    fn default_code() -> CodeAttribute {
        CodeAttribute {
            max_stack: 0,
            max_locals: 0,
            code: Vec::new(),
            exception_table: Vec::new(),
            attributes: Vec::new(),
        }
    }

    fn mk_method(name_idx: u16, desc_idx: u16, code: Option<CodeAttribute>) -> MethodInfo {
        use crate::metadata::access_flags::ACC_PUBLIC;
        MethodInfo {
            access_flags: crate::metadata::AccessFlags::from_bits(ACC_PUBLIC),
            name_index: name_idx,
            descriptor_index: desc_idx,
            attributes: Vec::new(),
            code,
        }
    }

    #[test]
    fn find_exact_method_locates_in_named_class() {
        // utf8: [1]="C",[2]="m",[3]="()I"; classes[1] → [4]=Class{1}="C"
        let pool = mk_cp(&["C", "m", "()I"], &[1]);
        let cf = mk_cf(pool, 4, 0, vec![], vec![mk_method(2, 3, Some(default_code()))]);
        let mut reg = ClassRegistry::new();
        reg.load(cf).unwrap();
        let (lc, _m) = reg.find_exact_method("C", "m", "()I").expect("应命中");
        assert_eq!(lc.name(), "C");
        assert!(reg.find_exact_method("C", "nope", "()I").is_none());
    }

    #[test]
    fn find_default_method_finds_interface_default() {
        // 接口 I:[1]="I",[2]=Object,[3]="m",[4]="()I"; classes[1,3] → [5]=Class"I",[6]=Class Object
        let i_pool = mk_cp(&["I", "java/lang/Object", "m", "()I"], &[1, 3]);
        let i_cf = mk_cf(i_pool, 5, 6, vec![], vec![mk_method(3, 4, Some(default_code()))]);
        // 类 C 实现 I,不声明 m:[1]="C",[2]=Object,[3]="I"; classes[1,3,3] → [4]=Class"C",[5]=Class Object,[6]=Class"I"
        let c_pool = mk_cp(&["C", "java/lang/Object", "I"], &[1, 3, 3]);
        let c_cf = mk_cf(c_pool, 4, 5, vec![6], vec![]);
        let mut reg = ClassRegistry::new();
        reg.load(i_cf).unwrap();
        reg.load(c_cf).unwrap();
        let (lc, _m) = reg.find_default_method("C", "m", "()I").expect("应命中接口 default");
        assert_eq!(lc.name(), "I");
        assert!(reg.find_exact_method("C", "m", "()I").is_none()); // C 自身未声明
    }

    #[test]
    fn find_default_method_skips_abstract_finds_superinterface() {
        // I2:default m。[1]="I2",[2]=Object,[3]="m",[4]="()I"; classes[1,3]→[5]=Class"I2",[6]=Class Object
        let i2_pool = mk_cp(&["I2", "java/lang/Object", "m", "()I"], &[1, 3]);
        let i2_cf = mk_cf(i2_pool, 5, 6, vec![], vec![mk_method(3, 4, Some(default_code()))]);
        // I1:抽象 m + 超接口 I2。[1]="I1",[2]=Object,[3]="I2",[4]="m",[5]="()I";
        //    classes[1,2,3] → [6]=Class"I1",[7]=Class Object,[8]=Class"I2"
        let i1_pool = mk_cp(&["I1", "java/lang/Object", "I2", "m", "()I"], &[1, 2, 3]);
        let i1_cf = mk_cf(
            i1_pool,
            6,
            7,
            vec![8],
            vec![mk_method(4, 5, None)], // 抽象 m,无 Code
        );
        // C 实现 I1。[1]="C",[2]=Object,[3]="I1"; classes[1,2,3] → [4]=Class"C",[5]=Class Object,[6]=Class"I1"
        let c_pool = mk_cp(&["C", "java/lang/Object", "I1"], &[1, 2, 3]);
        let c_cf = mk_cf(c_pool, 4, 5, vec![6], vec![]);
        let mut reg = ClassRegistry::new();
        reg.load(i2_cf).unwrap();
        reg.load(i1_cf).unwrap();
        reg.load(c_cf).unwrap();
        let (lc, _m) = reg.find_default_method("C", "m", "()I").expect("应跳过抽象,命中 I2");
        assert_eq!(lc.name(), "I2");
    }

    #[test]
    fn find_default_method_none_when_all_abstract() {
        // I 抽象 m(无超接口)。C 实现 I。
        let i_pool = mk_cp(&["I", "java/lang/Object", "m", "()I"], &[1, 3]);
        let i_cf = mk_cf(i_pool, 5, 6, vec![], vec![mk_method(3, 4, None)]);
        let c_pool = mk_cp(&["C", "java/lang/Object", "I"], &[1, 3, 3]);
        let c_cf = mk_cf(c_pool, 4, 5, vec![6], vec![]);
        let mut reg = ClassRegistry::new();
        reg.load(i_cf).unwrap();
        reg.load(c_cf).unwrap();
        assert!(reg.find_default_method("C", "m", "()I").is_none());
    }
```

- [ ] **Step 3: 运行看红**

Run: `cargo test --lib find_`
Expected: 编译失败(`find_exact_method`/`find_default_method` 未定义)。

- [ ] **Step 4: 实现三方法**

在 `impl ClassRegistry`(紧邻 `find_virtual_method` 之后)追加:

```rust
    /// 在 class_name(单类,非链)内精确查找 (name, desc) 方法 → (类, 方法)。
    /// 用于 invokespecial 的私有精确判定与 super 虚查起点。
    pub fn find_exact_method<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        let lc = self.get(class_name)?;
        lc.cf
            .methods
            .iter()
            .find(|m| method_matches(&lc.cf, m, name, desc))
            .map(|m| (lc, m))
    }

    /// 接口 default 方法查找:沿 class_name 类层次所有传递实现接口 BFS,
    /// 找首个**带 Code** 的 (name, desc) → (声明接口类, 方法)。
    /// 类链已由调用方查过;此仅兜底 default(抽象方法跳过,继续搜索)。
    pub fn find_default_method<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut visited: HashSet<String> = HashSet::new();
        // 种子:class_name 及其超类链上每类的直接接口。
        let mut cur = self.get(class_name);
        while let Some(lc) = cur {
            for iface in lc.interface_names() {
                if visited.insert(iface.clone()) {
                    queue.push_back(iface);
                }
            }
            cur = lc
                .super_class_name()
                .filter(|s| *s != "java/lang/Object")
                .and_then(|s| self.get(s));
        }
        // BFS 接口闭包,跳过抽象,命中带 Code 的 default。
        while let Some(iface_name) = queue.pop_front() {
            if let Some(iface_lc) = self.get(&iface_name) {
                if let Some(m) = iface_lc
                    .cf
                    .methods
                    .iter()
                    .find(|m| method_matches(&iface_lc.cf, m, name, desc) && m.code.is_some())
                {
                    return Some((iface_lc, m));
                }
                for super_iface in iface_lc.interface_names() {
                    if visited.insert(super_iface.clone()) {
                        queue.push_back(super_iface);
                    }
                }
            }
        }
        None
    }

    /// 虚/接口分派解析:类链先行(`find_virtual_method`),落空走接口 default
    /// (`find_default_method`)。命中抽象类方法(无 Code)时仍返回(由调用方判
    /// `AbstractMethodError`);default 路径必带 Code。
    pub fn resolve_dispatch<'a>(
        &'a self,
        class_name: &str,
        name: &str,
        desc: &str,
    ) -> Option<(&'a LoadedClass, &'a MethodInfo)> {
        if let Some(hit) = self.find_virtual_method(class_name, name, desc) {
            return Some(hit);
        }
        self.find_default_method(class_name, name, desc)
    }
```

- [ ] **Step 5: 运行看绿**

Run: `cargo test --lib find_`
Expected: PASS(4 测试)。

- [ ] **Step 6: 提交**

```bash
git add src/oops/klass.rs
git commit -m "Layer 4.2b:ClassRegistry find_exact_method/find_default_method/resolve_dispatch"
```

---

## Task 5: 集成闸门(先红)

**Files:**
- Create: `tests/interface_dispatch.rs`

- [ ] **Step 1: 写集成测试(完整文件)**

创建 `tests/interface_dispatch.rs`:

```rust
//! 集成测试(执行闸门):用 `javac` 编译含**接口 + default 方法 + 私有方法 + super 调用**
//! 的真实 Java 层次,解析其 `.class`,再用 rustj 解释器真正执行,验证 invokeinterface
//! 虚分派 / default 方法 / invokespecial 私有与 super / StackOverflowError 与 JVM 一致。
//!
//! 这是 Layer 4.2b 的"能否跑通真实字节码"判据。需要 PATH 中有 `javac`(无则跳过)。

use std::path::PathBuf;
use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

static COMPILE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load_all(source: &str, public_name: &str) -> ClassRegistry {
    let seq = COMPILE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("rustj-iface-{}-{seq}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let output = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        output.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut registry = ClassRegistry::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("class") {
            let bytes = std::fs::read(&path).unwrap();
            let cf = parse(&bytes).expect("解析应成功");
            registry.load(cf).expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    registry
}

fn utf8(cf: &ClassFile, index: u16) -> String {
    match cf.constant_pool.get(index).unwrap() {
        ConstantPoolEntry::Utf8(s) => s.clone(),
        e => panic!("expected Utf8 at {index}, got {e:?}"),
    }
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| utf8(cf, m.name_index) == name && utf8(cf, m.descriptor_index) == desc)
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

/// 以给定深度上限执行 static 方法,返回结果。
fn run_with_limit(
    registry: &ClassRegistry,
    class_name: &str,
    name: &str,
    desc: &str,
    stack_limit: u32,
) -> Result<Value, VmError> {
    let lc = registry
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let method = find_method(&lc.cf, name, desc);
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(registry).with_stack_limit(stack_limit);
    interp.interpret_with(&mut frame, &mut vm)
}

fn run(registry: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    run_with_limit(registry, class_name, name, desc, rustj::runtime::DEFAULT_STACK_LIMIT)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

const SOURCE: &str = r#"
interface Shape {
    int kind();
    default int tag() { return kind() * 100 + 1; }
}
class Circle implements Shape {
    public int kind() { return 2; }
}
class Square implements Shape {
    public int kind() { return 3; }
    public int ownTag() { return tag(); }
}
class Root {
    int base() { return 10; }
}
class Mid extends Root { }
class Leaf extends Mid {
    int viaSuper() { return super.base(); }
}
public class Vm {
    // invokeinterface 多态:a.kind()=2,b.kind()=3 → 2 + 30 = 32
    public static int ifacePoly() {
        Shape a = new Circle();
        Shape b = new Square();
        return a.kind() + b.kind() * 10;
    }
    // default method:类未覆盖 tag → 落到接口默认(kind()*100+1)= 201
    public static int defaultOnIface() {
        Shape s = new Circle();
        return s.tag();
    }
    // default 经类类型调用:invokevirtual tag → 类链落空 → 接口 default → 301
    public static int defaultViaClass() {
        Square s = new Square();
        return s.ownTag();
    }
    // super 调用继承方法:Mid 不声明 base → invokespecial 虚查到 Root.base = 10
    public static int superInherited() {
        Leaf l = new Leaf();
        return l.viaSuper();
    }
    // 无限递归 → StackOverflowError
    public static int infinite() {
        return infinite();
    }
    // null 引用 invokeinterface → NullPointerException
    public static int nullIface() {
        Shape s = null;
        return s.kind();
    }
}
"#;

#[test]
fn invokeinterface_is_polymorphic() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    assert_eq!(run(&registry, "Vm", "ifacePoly", "()I"), Value::Int(32));
}

#[test]
fn invokeinterface_hits_default_method() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    assert_eq!(run(&registry, "Vm", "defaultOnIface", "()I"), Value::Int(201));
}

#[test]
fn invokevirtual_falls_through_to_default() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    assert_eq!(run(&registry, "Vm", "defaultViaClass", "()I"), Value::Int(301));
}

#[test]
fn invokespecial_super_inherited() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    assert_eq!(run(&registry, "Vm", "superInherited", "()I"), Value::Int(10));
}

#[test]
fn infinite_recursion_is_stackoverflow() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    assert_eq!(
        run_with_limit(&registry, "Vm", "infinite", "()I", 16).unwrap_err(),
        VmError::StackOverflow
    );
}

#[test]
fn invokeinterface_on_null_is_nullpointer() {
    if !javac_available() {
        eprintln!("跳过:未找到 javac");
        return;
    }
    let registry = compile_and_load_all(SOURCE, "Vm");
    assert_eq!(
        run_with_limit(&registry, "Vm", "nullIface", "()I", rustj::runtime::DEFAULT_STACK_LIMIT)
            .unwrap_err(),
        VmError::NullPointer
    );
}
```

注:`rustj::runtime::DEFAULT_STACK_LIMIT` 与 `Vm::with_stack_limit` 由 Task 2 导出。

- [ ] **Step 2: 运行看红**

Run: `cargo test --test interface_dispatch`
Expected: FAIL(多数用例因 `Invokeinterface` 仍未分派 → `UnsupportedOpcode`,或 invokevirtual 未走 default / invokespecial super 未虚查)。

> **说明**:此时集成测试文件应能编译(仅用既有公共 API + Task 2 导出项)。`defaultViaClass`/`superInherited` 失败于逻辑缺口,`ifacePoly`/`defaultOnIface`/`nullIface` 失败于 invokeinterface 未实现,`infinite` 失败于……实际会先触发 Rust 栈溢出或 StackOverflow(取决于 limit)。Task 6 实现后转绿。

- [ ] **Step 3: 暂不提交**(随 Task 6 一起提交)。

---

## Task 6: 实现 invokeinterface + invokespecial 分支 + default 兜底

**Files:**
- Modify: `src/runtime/interpreter/invoke.rs`(重构 + 新增 + 扩展)
- Modify: `src/runtime/interpreter/mod.rs`(`Invokeinterface` 臂)

- [ ] **Step 1: `invoke.rs` 顶部增 import**

`use crate::runtime::{Frame, LocalVars, Reference, Vm};` 之后确认有(若无则加):

```rust
use crate::oops::LoadedClass;
```

- [ ] **Step 2: 增 `run_callee` 与 `apply_return` 辅助(DRY)**

在 `run_with_depth` 之后、`enum Arg` 之前新增:

```rust
/// 构造被调用者帧、按 `setup` 写局部变量、递归执行(带深度守卫),返回被调用者返回值。
fn run_callee(
    vm: &mut Vm<'_>,
    target_lc: &LoadedClass,
    method: &MethodInfo,
    setup: impl FnOnce(&mut LocalVars) -> Result<(), VmError>,
) -> Result<Value, VmError> {
    let code = method
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("目标方法无 Code(抽象/原生)"))?;
    let mut callee = Frame::new(code.max_locals, code.max_stack);
    setup(&mut callee.locals)?;
    let callee_interp = Interpreter::new(&code.code, &target_lc.cf.constant_pool);
    run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm))
}

/// 按返回描述符把被调用者返回值回填调用者操作数栈。
fn apply_return(
    frame: &mut Frame,
    return_type: ReturnDescriptor,
    value: Value,
) -> Result<(), VmError> {
    match (return_type, value) {
        (ReturnDescriptor::Void, Value::Void) => Ok(()),
        (ReturnDescriptor::FieldType(_), Value::Void) => {
            Err(VmError::BadConstant("期望返回值,被调用者返回 void"))
        }
        (ReturnDescriptor::FieldType(_), v) => push_return(frame, v),
        (ReturnDescriptor::Void, _) => Err(VmError::BadConstant("void 方法返回了值")),
    }
}
```

- [ ] **Step 3: 重写 `invoke_static` 走 `run_callee`/`apply_return`**

把 `invoke_static` 中"构造被调用者帧 → 递归 → 回填"整段(从 `let mut callee = Frame::new(...)` 到函数末尾的 match)替换为:

```rust
    let result = {
        let args = args;
        run_callee(vm, target_lc, target_method, move |locals| {
            let mut slot: u16 = 0;
            for a in args {
                let advance = store_arg(locals, slot, a)?;
                slot = slot
                    .checked_add(advance)
                    .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
            }
            Ok(())
        })?
    };
    apply_return(frame, md.return_type, result)
```

(保留前置的 `resolve_methodref` / `find_method` / `pop_arg` 逻辑不变。)

- [ ] **Step 4: 重写 `invoke_special` —— 扩三支 + 走辅助**

把整个 `invoke_special` 函数体替换为:

```rust
pub(super) fn invoke_special(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
) -> Result<(), VmError> {
    let (declared_class, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    let md = parse_method_descriptor(&desc)?;

    let mut args: Vec<Arg> = Vec::with_capacity(md.parameters.len());
    for ft in md.parameters.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    let objref = frame.operands.pop_reference()?;

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokespecial 需要类注册表"))?;

    // 解析目标 (类, 方法):
    //   <init>  → 声明类精确(未加载根类 ()V → 空操作,沿用 4.1);
    //   私有    → 声明类精确(私有不可继承);
    //   其余    → super 虚查(声明类 = 调用者直接超类,上行)。
    let (target_lc, target_method) = if method_name == "<init>" {
        match registry.get(&declared_class) {
            None => {
                if matches!(md.return_type, ReturnDescriptor::Void) {
                    return Ok(());
                }
                return Err(VmError::BadConstant("invokespecial 目标类未加载"));
            }
            Some(lc) => {
                let m = find_method(&lc.cf, &method_name, &desc)?;
                (lc, m)
            }
        }
    } else {
        match registry.find_exact_method(&declared_class, &method_name, &desc) {
            Some((lc, m)) if m.access_flags.is_private() => (lc, m),
            _ => registry
                .find_virtual_method(&declared_class, &method_name, &desc)
                .ok_or(VmError::BadConstant("invokespecial 未找到目标方法"))?,
        }
    };

    let result = run_callee(vm, target_lc, target_method, move |locals| {
        locals.set_reference(0, objref)?;
        let mut slot: u16 = 1;
        for a in args {
            let advance = store_arg(locals, slot, a)?;
            slot = slot
                .checked_add(advance)
                .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
        }
        Ok(())
    })?;
    apply_return(frame, md.return_type, result)
}
```

- [ ] **Step 5: 重写 `invoke_virtual` —— default 兜底 + 走辅助**

把 `invoke_virtual` 中从 `let registry = ...` 到函数末尾替换为(保留前置解析 Methodref / pop args / null 检查 / 取运行时类不变):

```rust
    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokevirtual 需要类注册表"))?;
    let (target_lc, target_method) = registry
        .resolve_dispatch(&runtime_class, &method_name, &desc)
        .ok_or(VmError::AbstractMethodError)?;
    if target_method.code.is_none() {
        return Err(VmError::AbstractMethodError);
    }

    let result = run_callee(vm, target_lc, target_method, move |locals| {
        locals.set_reference(0, objref)?;
        let mut slot: u16 = 1;
        for a in args {
            let advance = store_arg(locals, slot, a)?;
            slot = slot
                .checked_add(advance)
                .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
        }
        Ok(())
    })?;
    apply_return(frame, md.return_type, result)
```

- [ ] **Step 6: 新增 `invoke_interface`**

在 `invoke_virtual` 之后新增:

```rust
/// 执行 `invokeinterface`:按对象运行时实际类分派。语义与 `invokevirtual` 一致
/// (类链先行 → 接口 default 兜底),差别仅在操作数 5 字节(由分派循环处理 `pc += 5`)
/// 与命中抽象方法报 `AbstractMethodError`。Methodref 声明接口不参与分派。
pub(super) fn invoke_interface(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
) -> Result<(), VmError> {
    let (_declared_iface, method_name, desc) = resolve_methodref(interp.cp(), methodref_index)?;
    let md = parse_method_descriptor(&desc)?;

    let mut args: Vec<Arg> = Vec::with_capacity(md.parameters.len());
    for ft in md.parameters.iter().rev() {
        args.push(pop_arg(frame, ft)?);
    }
    let objref = frame.operands.pop_reference()?;
    if objref.is_null() {
        return Err(VmError::NullPointer);
    }

    let runtime_class = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("invokeinterface 引用悬空"))?;
    let runtime_class = match runtime_class {
        Oop::Instance(i) => i.class_name().to_string(),
    };

    let registry = vm
        .registry()
        .ok_or(VmError::BadConstant("invokeinterface 需要类注册表"))?;
    let (target_lc, target_method) = registry
        .resolve_dispatch(&runtime_class, &method_name, &desc)
        .ok_or(VmError::AbstractMethodError)?;
    if target_method.code.is_none() {
        return Err(VmError::AbstractMethodError);
    }

    let result = run_callee(vm, target_lc, target_method, move |locals| {
        locals.set_reference(0, objref)?;
        let mut slot: u16 = 1;
        for a in args {
            let advance = store_arg(locals, slot, a)?;
            slot = slot
                .checked_add(advance)
                .ok_or(VmError::BadConstant("局部变量槽位溢出"))?;
        }
        Ok(())
    })?;
    apply_return(frame, md.return_type, result)
}
```

- [ ] **Step 7: `mod.rs` 增 `Invokeinterface` 分派臂**

在 `mod.rs` 的 `Opcode::Invokevirtual => { ... }` 块之后、`Opcode::Return` 之前,新增:

```rust
                Opcode::Invokeinterface => {
                    let index = self.read_u2(pc + 1)?;
                    // count(pc+3) 与尾 0(pc+4)对运行时冗余,读后随 pc += 5 丢弃。
                    invoke::invoke_interface(self, frame, vm, index)?;
                    pc += 5;
                }
```

- [ ] **Step 8: 运行集成闸门看绿**

Run: `cargo test --test interface_dispatch`
Expected: PASS(6 测试)。若有用例仍失败:
- `defaultViaClass` 失败 → javac 对类类型上的 default 调用可能用了 invokevirtual(已由 Step 5 兜底);确认 `invokevirtual` 走了 `resolve_dispatch`。
- `superInherited` 失败 → 确认 `invoke_special` 非私有分支走了 `find_virtual_method`。

- [ ] **Step 9: 运行全量套件确认无回归**

Run: `cargo test`
Expected: 全绿(含既有 `interpret_method_invocation`、`virtual_dispatch`、`object_fields`;重构由它们守护)。

- [ ] **Step 10: 提交**

```bash
git add src/runtime/interpreter/invoke.rs src/runtime/interpreter/mod.rs tests/interface_dispatch.rs
git commit -m "$(cat <<'EOF'
Layer 4.2b:invokeinterface + invokespecial 完整语义 + default 兜底

invokeinterface(0xb9):运行时类分派,类链先行(find_virtual_method)落空走接口
default(find_default_method BFS);命中抽象 → AbstractMethodError;5 字节操作数。

invokevirtual:同样经 resolve_dispatch 走 default 兜底(JVM 自 Java 8 类类型调用
default 亦走此路);命中抽象 → AbstractMethodError。

invokespecial 三支:<init> 精确(4.1)/ 私有精确(ACC_PRIVATE)/ super 虚查
(find_virtual_method 从声明类=调用者超类上行)。

DRY 重构:抽出 run_callee(构造帧+深度守卫递归)+ apply_return(按返回类型回填),
四 invoke 函数统一走它们。run_with_depth 守卫(深度 +1/−1 对称)覆盖全部 invoke。

集成闸门 tests/interface_dispatch.rs:javac 编译 Shape(接口+default)/Circle/Square/
Root/Mid/Leaf 层次,真实执行接口多态/default/default 经类类型/super 继承调用/
无限递归 StackOverflow/null NPE,与 JVM 一致。

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: clippy + 零 unsafe + 收尾

**Files:** 无(校验)

- [ ] **Step 1: clippy 零告警**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 无告警(Windows 增量编译"拒绝访问"提示非错误,忽略)。
常见修法:let-chain 折叠、未用 import 删除。

- [ ] **Step 2: 零 unsafe 复核**

Run(检查无新 `unsafe`):
```
git grep -n "unsafe" -- src/
```
Expected: 仅 `#![deny(unsafe_code)]` 与文档注释中的 `unsafe` 字样,无实际 unsafe 块。

- [ ] **Step 3: 全量测试**

Run: `cargo test`
Expected: 全绿。

- [ ] **Step 4: 更新项目记忆**

更新 `C:\Users\jinha\.claude\projects\E--rustj\memory\hotspot-rust-migration-project.md`:标记 4.2b 完成(commit 哈希填入),下一增量指向 4.3 数组。

---

## Self-Review(计划自检)

**Spec 覆盖:** §2 范围逐项 → invokeinterface(Task 6 Step 6)/ default method(Task 4 find_default_method + Task 6 Step 5/6)/ invokespecial 三支(Task 6 Step 4)/ SOE 深度计数(Task 2)/ AbstractMethodError(Task 1 + Task 6)。顺延项(itable/ICCE/athrow/显式帧栈/最具体解析)均不做。✓

**占位符:** 无 TBD/TODO;每步含完整代码或确切命令。✓

**类型一致性:** `find_exact_method`/`find_default_method`/`resolve_dispatch` 返回 `Option<(&'a LoadedClass, &'a MethodInfo)>`,与 `find_virtual_method` 一致,被 invoke 各处以解构消费;`run_callee`/`apply_return`/`run_with_depth` 签名贯穿 Task 2/6 一致;`Vm::frame_depth`/`stack_limit`/`with_stack_limit`/`DEFAULT_STACK_LIMIT` 在 Task 2 定义、Task 5 测试引用一致。✓

**已知风险点:**(a) Task 6 重构改三既有 invoke —— 由既有绿测试守护;(b) `defaultViaClass` 依赖 javac 对类类型 default 调用发 invokevirtual(已由 Step 5 兜底覆盖,若 javac 发 invokeinterface 也由 Step 6 覆盖);(c) SOE 测试用 limit=16 快速触发,规避 Rust 栈溢出。✓
