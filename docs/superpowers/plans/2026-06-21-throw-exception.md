# Layer 4.7 `athrow` + 异常表 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `athrow` + 异常表,让 rustj 执行用户异常的抛出与 `try/catch` 捕获(含跨帧传播)。

**Architecture:** 抛出异常以 `VmError::ThrownException(Reference)` 沿 Rust 调用栈上传;
`Interpreter` 增 `exception_table` 字段;`exception.rs::find_handler` 在 `[start,end)` 内
按 `catch_type`(0=catch-all,否则 `is_instance`)找处理者;invoke 经 `InvokeFlow` +
`finish_invoke` 统一返回值回填与异常捕获。

**依据:** `docs/superpowers/specs/2026-06-21-throw-exception-design.md`。
节奏:写失败测试 → 看红 → 最小实现 → 看绿 → 提交。命令在 `E:\rustj`。
路径:`ExceptionTableEntry` = `crate::classfile::attributes::ExceptionTableEntry`;
`ReturnDescriptor::Void`/`FieldType(_)`、`resolve_class_name`(field.rs,`pub(super)`)、
`ClassRegistry::is_instance`(4.6)已就绪。

---

### Task 1: `VmError::ThrownException` + `OperandStack::clear`

**Files:** Modify `src/runtime/interpreter/mod.rs`(VmError + 测试)、
`src/runtime/operand_stack.rs`

- [ ] **Step 1: 写失败测试**(operand_stack.rs tests 追加)

```rust
    #[test]
    fn clear_empties_a_populated_stack() {
        let mut s = OperandStack::new(4);
        s.push_int(1).unwrap();
        s.push_int(2).unwrap();
        assert_eq!(s.depth(), 2);
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.depth(), 0);
        // clear 后仍可正常压栈(容量不变)
        s.push_int(9).unwrap();
        assert_eq!(s.depth(), 1);
    }
```

> operand_stack.rs 已有 `use super::slot::{Reference, Slot};` 等;`OperandStack` 在
> 当前文件,测试用 `super::*` 或 `OperandStack` 直接。确认 tests 模块 `use` 形态后对齐。

- [ ] **Step 2: 看红**

Run: `cargo test --lib clear_empties_a_populated_stack`
Expected: 编译错误(`clear` 未定义)。

- [ ] **Step 3: 实现 `clear`**(operand_stack.rs,`is_empty` 旁)

```rust
    /// 清空操作数栈(异常处理者进入前调用;容量不变)。
    pub fn clear(&mut self) {
        self.slots.clear();
    }
```

- [ ] **Step 4: 加 `ThrownException` 变体**(mod.rs `VmError`,`ClassCastException` 旁)

```rust
    /// 用户 athrow 抛出的异常(沿调用栈传播,直至被异常表处理者捕获)。
    ThrownException(crate::runtime::Reference),
```

并在 `impl Display for VmError` 加臂:

```rust
    VmError::ThrownException(_) => write!(f, "ThrownException"),
```

- [ ] **Step 5: 看绿**

Run: `cargo test --lib clear_empties_a_populated_stack`
Expected: PASS。`cargo build --tests` 编译通过(ThrownException 变体加好)。

- [ ] **Step 6: 提交**

```bash
git add src/runtime/operand_stack.rs src/runtime/interpreter/mod.rs
git commit -m "feat(runtime): OperandStack::clear + VmError::ThrownException"
```

---

### Task 2: `Interpreter.exception_table` 字段

**Files:** Modify `src/runtime/interpreter/mod.rs`

- [ ] **Step 1: 加字段 + 构造器**(mod.rs,`use` 区加 `use crate::classfile::attributes::ExceptionTableEntry;`)

`Interpreter` 结构体增字段:

```rust
pub struct Interpreter<'a> {
    code: &'a [u8],
    cp: &'a ConstantPool,
    exception_table: &'a [ExceptionTableEntry],
}
```

`new` 默认空表(既有 ~67 处调用零改动),新增表感知构造器:

```rust
    pub fn new(code: &'a [u8], cp: &'a ConstantPool) -> Self {
        Self { code, cp, exception_table: &[] }
    }

    /// 带 `Code` 属性的异常表构造(4 处 invoke 被调用者、集成闸门入口用之)。
    pub fn new_with_exception_table(
        code: &'a [u8],
        cp: &'a ConstantPool,
        exception_table: &'a [ExceptionTableEntry],
    ) -> Self {
        Self { code, cp, exception_table }
    }
```

- [ ] **Step 2: 看绿**

Run: `cargo test --lib`
Expected: 全绿(无行为变化;既有测试用 `new`→空表)。
Run: `cargo clippy --all-targets -- -D warnings` → 零告警。

- [ ] **Step 3: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "feat(interp): Interpreter 增 exception_table 字段"
```

---

### Task 3: `exception.rs` 子模块 + `find_handler`(TDD)

**Files:** Create `src/runtime/interpreter/exception.rs`;Modify `mod.rs`(声明子模块)

- [ ] **Step 1: 声明子模块**(mod.rs,`mod type_check;` 旁)

```rust
mod exception;
```

- [ ] **Step 2: 写 `exception.rs` 含失败测试**(整文件,先只放函数签名 + 测试)

```rust
//! 异常分派:`athrow` 抛出对象的异常表扫描(`find_handler`)。
//!
//! 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_athrow)` 异常表查找。
//! `[start_pc, end_pc)` 覆盖抛出点 pc 且 `catch_type`(0=catch-all,否则
//! `is_instance`)匹配 → 返回 `handler_pc`;首条匹配胜出。

use super::field::resolve_class_name;
use super::{Interpreter, VmError};
use crate::classfile::attributes::ExceptionTableEntry;
use crate::runtime::{Reference, Vm};

/// 取异常对象(非 null)的运行时类名(own String 避免借用纠缠)。
/// athrow 对象必为实例(数组不能 throw);悬空 → `BadConstant`。
fn runtime_class_name(vm: &Vm<'_>, exc: Reference) -> Result<String, VmError> {
    use crate::oops::Oop;
    let obj = vm
        .heap()
        .get(exc)
        .ok_or(VmError::BadConstant("athrow/异常分派 引用悬空"))?;
    match obj {
        Oop::Instance(i) => Ok(i.class_name().to_string()),
        Oop::Array(_) => Err(VmError::BadConstant("athrow 对象为数组(非法)")),
    }
}

/// 在 `table` 找覆盖 `pc` 且匹配 `exc` 运行时类的处理者;返回 `handler_pc`。
/// catch_type == 0 → catch-all;否则 `is_instance(运行时类, 目标类)`。
pub(super) fn find_handler(
    interp: &Interpreter<'_>,
    vm: &Vm<'_>,
    table: &[ExceptionTableEntry],
    pc: usize,
    exc: Reference,
) -> Result<Option<usize>, VmError> {
    for e in table {
        if (e.start_pc as usize) <= pc && pc < (e.end_pc as usize) {
            let hit = if e.catch_type == 0 {
                true
            } else {
                let target = resolve_class_name(interp.cp(), e.catch_type)?;
                let exc_class = runtime_class_name(vm, exc)?;
                let reg = vm
                    .registry()
                    .ok_or(VmError::BadConstant("异常分派需类注册表"))?;
                reg.is_instance(&exc_class, &target)
            };
            if hit {
                return Ok(Some(e.handler_pc as usize));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classfile::Reader;
    use crate::classfile::attributes::ExceptionTableEntry;
    use crate::constant_pool::ConstantPool;
    use crate::metadata::{AccessFlags, ClassFile};
    use crate::oops::{ClassRegistry, Oop};
    use crate::runtime::{Reference, Vm};

    /// utf8: 1=Throwable 2=BaseExc 3=SubExc 4=OtherExc ; class: 5=Throwable 6=BaseExc 7=SubExc 8=OtherExc。
    fn cp_bytes() -> Vec<u8> {
        let mut b = vec![0x00, 0x09]; // count = 8 entries + 1
        for s in ["java/lang/Throwable", "BaseExc", "SubExc", "OtherExc"] {
            b.push(0x01);
            b.extend_from_slice(&(s.len() as u16).to_be_bytes());
            b.extend_from_slice(s.as_bytes());
        }
        for idx in [1u16, 2, 3, 4] {
            b.push(0x07);
            b.extend_from_slice(&idx.to_be_bytes());
        }
        b
    }

    /// 注册表:BaseExc←SubExc,OtherExc(均 extends Throwable,Throwable 不加载)。
    fn build() -> (ClassRegistry, ConstantPool) {
        let bytes = cp_bytes();
        let mk_cp = || ConstantPool::parse(&mut Reader::new(&bytes)).unwrap();
        let mk_cf = |this: u16, super_c: u16| ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: mk_cp(),
            access_flags: AccessFlags::from_bits(0),
            this_class: this,
            super_class: super_c,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(),
        };
        let mut reg = ClassRegistry::new();
        reg.load(mk_cf(6, 5)).unwrap(); // BaseExc extends Throwable(#5)
        reg.load(mk_cf(7, 6)).unwrap(); // SubExc extends BaseExc
        reg.load(mk_cf(8, 5)).unwrap(); // OtherExc extends Throwable
        (reg, mk_cp())
    }

    fn sub_instance(reg: &ClassRegistry, vm: &mut Vm<'_>) -> Reference {
        let lc = reg.get("SubExc").unwrap();
        vm.heap_mut().alloc(Oop::Instance(reg.new_instance(lc)))
    }

    fn entry(start: u16, end: u16, handler: u16, catch_type: u16) -> ExceptionTableEntry {
        ExceptionTableEntry { start_pc: start, end_pc: end, handler_pc: handler, catch_type }
    }

    fn empty_interp(cp: &ConstantPool) -> Interpreter<'_> {
        Interpreter::new(&[], cp)
    }

    #[test]
    fn find_exact_type_matches() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 7)]; // catch SubExc(#7)
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), Some(2));
    }

    #[test]
    fn find_out_of_range_no_match() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 7)];
        assert_eq!(find_handler(&interp, &vm, &table, 5, exc).unwrap(), None);
    }

    #[test]
    fn find_supertype_matches() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm); // SubExc 实例
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 6)]; // catch BaseExc(#6)
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), Some(2));
    }

    #[test]
    fn find_unrelated_no_match() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 8)]; // catch OtherExc(#8)
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), None);
    }

    #[test]
    fn find_catch_all_matches_anything() {
        let (reg, cp) = build();
        let mut vm = Vm::new(&reg);
        let exc = sub_instance(&reg, &mut vm);
        let interp = empty_interp(&cp);
        let table = [entry(0, 2, 2, 0)]; // catch_type 0 = catch-all
        assert_eq!(find_handler(&interp, &vm, &table, 1, exc).unwrap(), Some(2));
    }
}
```

> `Vm::new(&reg)`、`reg.get`、`reg.new_instance`、`vm.heap_mut().alloc`、
> `AccessFlags::from_bits(0)`、`ConstantPool::parse(&mut Reader::new(&b))` 均与
> type_check.rs 同用法。`Interpreter::new(&[], cp)` 用空字节码(只借 cp)。

- [ ] **Step 3: 看绿**(find_handler 实现已在文件内,5 测试应直接绿;若签名/导入有误按错误修)

Run: `cargo test --lib exception::tests`
Expected: 5 PASS。

Run: `cargo clippy --all-targets -- -D warnings` → 零告警(若有未用导入按提示删)。

- [ ] **Step 4: 提交**

```bash
git add src/runtime/interpreter/exception.rs src/runtime/interpreter/mod.rs
git commit -m "feat(interp): exception::find_handler 异常表扫描"
```

---

### Task 4: `athrow` 分派臂(TDD)

**Files:** Modify `src/runtime/interpreter/mod.rs`(分派臂 + 测试)

- [ ] **Step 1: 写失败测试**(mod.rs tests 追加;`#[derive(Debug, Clone, Copy, PartialEq)]`
  的 `Opcode`、`cp_with_class` 之外需异常类层次)

```rust
    // ===== Layer 4.7:athrow =====

    /// 构 BaseExc←SubExc 注册表 + SubExc 实例 + 含异常类的 cp。
    /// cp: utf8 1=Throwable 2=BaseExc 3=SubExc ; class 4=Throwable 5=BaseExc 6=SubExc。
    fn athrow_setup() -> (
        crate::oops::ClassRegistry,
        ConstantPool,
        crate::runtime::Reference,
    ) {
        use crate::classfile::Reader;
        use crate::metadata::{AccessFlags, ClassFile};
        let cp_bytes = {
            let mut b = vec![0x00, 0x07]; // count = 6 entries + 1
            for s in ["java/lang/Throwable", "BaseExc", "SubExc"] {
                b.push(0x01);
                b.extend_from_slice(&(s.len() as u16).to_be_bytes());
                b.extend_from_slice(s.as_bytes());
            }
            for idx in [1u16, 2, 3] {
                b.push(0x07);
                b.extend_from_slice(&idx.to_be_bytes());
            }
            b
        };
        let mk_cp = || ConstantPool::parse(&mut Reader::new(&cp_bytes)).unwrap();
        let mk_cf = |this: u16, super_c: u16| ClassFile {
            minor_version: 0,
            major_version: 52,
            constant_pool: mk_cp(),
            access_flags: AccessFlags::from_bits(0),
            this_class: this,
            super_class: super_c,
            interfaces: Vec::new(),
            fields: Vec::new(),
            methods: Vec::new(),
            attributes: Vec::new(),
        };
        let mut reg = crate::oops::ClassRegistry::new();
        reg.load(mk_cf(5, 4)).unwrap(); // BaseExc(#5) extends Throwable(#4)
        reg.load(mk_cf(6, 5)).unwrap(); // SubExc(#6) extends BaseExc
        let cp = mk_cp();
        let mut vm = crate::runtime::Vm::new(&reg);
        let lc = reg.get("SubExc").unwrap();
        let inst = vm.heap_mut().alloc(crate::oops::Oop::Instance(reg.new_instance(lc)));
        (reg, cp, inst)
    }
```

> 注意:`vm` 在 setup 内 drop,实例引用是堆 u32 句柄,凭句柄在新 `Vm` 里仍有效
> (堆是值语义 Oop;但 `Vm::new(&reg)` 新建空堆 → 句柄失效!)。**故 setup 不可返回
> 跨 Vm 的引用**。改为:测试各自建 Vm + 实例(见各测试),setup 只返 `(reg, cp)`。

**修正:setup 签名改为 `(reg, cp)`,实例在各测试内建:**

```rust
    fn athrow_setup() -> (crate::oops::ClassRegistry, ConstantPool) {
        // ...(同上构造,但删除 vm/inst,直接返回 (reg, cp))
        let mut reg = crate::oops::ClassRegistry::new();
        reg.load(mk_cf(5, 4)).unwrap();
        reg.load(mk_cf(6, 5)).unwrap();
        (reg, mk_cp())
    }
```

测试(3 个):

```rust
    #[test]
    fn athrow_caught_jumps_to_handler() {
        use crate::classfile::attributes::ExceptionTableEntry;
        let (reg, cp) = athrow_setup();
        let mut vm = crate::runtime::Vm::new(&reg);
        let lc = reg.get("SubExc").unwrap();
        let inst = vm.heap_mut().alloc(crate::oops::Oop::Instance(reg.new_instance(lc)));
        // [aload0(0) athrow(1) iconst1(2) ireturn(3)] ; try [0,2) handler=2 catch SubExc(#6)
        let code = [Opcode::Aload0 as u8, Opcode::Athrow as u8, Opcode::Iconst1 as u8, Opcode::Ireturn as u8];
        let table = [ExceptionTableEntry { start_pc: 0, end_pc: 2, handler_pc: 2, catch_type: 6 }];
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new_with_exception_table(&code, &cp, &table);
        assert_eq!(interp.interpret_with(&mut frame, &mut vm).unwrap(), Value::Int(1));
    }

    #[test]
    fn athrow_uncaught_propagates() {
        use crate::classfile::attributes::ExceptionTableEntry;
        let (reg, cp) = athrow_setup();
        let mut vm = crate::runtime::Vm::new(&reg);
        let lc = reg.get("SubExc").unwrap();
        let inst = vm.heap_mut().alloc(crate::oops::Oop::Instance(reg.new_instance(lc)));
        let code = [Opcode::Aload0 as u8, Opcode::Athrow as u8];
        let table = []; // 无处理者
        let mut frame = Frame::new(1, 2);
        frame.locals.set_reference(0, inst).unwrap();
        let interp = Interpreter::new_with_exception_table(&code, &cp, &table);
        match interp.interpret_with(&mut frame, &mut vm).unwrap_err() {
            VmError::ThrownException(r) => assert_eq!(r, inst),
            other => panic!("期望 ThrownException,得 {other:?}"),
        }
    }

    #[test]
    fn athrow_null_throws_nullpointer() {
        let (reg, cp) = athrow_setup();
        let mut vm = crate::runtime::Vm::new(&reg);
        let code = [Opcode::AconstNull as u8, Opcode::Athrow as u8];
        let mut frame = Frame::new(0, 2);
        let interp = Interpreter::new_with_exception_table(&code, &cp, &[]);
        assert_eq!(interp.interpret_with(&mut frame, &mut vm).unwrap_err(), VmError::NullPointer);
    }
```

> `Value::Int`、`Frame::new`、`Aload0`/`AconstNull`/`Iconst1`/`Ireturn`/`Athrow` 均既有。
> `Opcode::Athrow as u8` = 0xbf。

- [ ] **Step 2: 看红**

Run: `cargo test --lib athrow_caught_jumps_to_handler athrow_uncaught_propagates athrow_null_throws_nullpointer`
Expected: 失败(`Athrow` 分派臂未实现 → `UnsupportedOpcode(0xbf)`)。

- [ ] **Step 3: 实现分派臂**(mod.rs,`Areturn` 臂附近;`use crate::runtime::Reference` 已在)

```rust
                Opcode::Athrow => {
                    let exc = frame.operands.pop_reference()?;
                    if exc.is_null() {
                        return Err(VmError::NullPointer);
                    }
                    match exception::find_handler(self, vm, self.exception_table, pc, exc)? {
                        Some(h) => {
                            frame.operands.clear();
                            frame.operands.push_reference(exc)?;
                            pc = h;
                        }
                        None => return Err(VmError::ThrownException(exc)),
                    }
                }
```

- [ ] **Step 4: 看绿**

Run: `cargo test --lib athrow_caught_jumps_to_handler athrow_uncaught_propagates athrow_null_throws_nullpointer`
Expected: 3 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "feat(interp): athrow 分派臂(异常表捕获/上传)"
```

---

### Task 5: `InvokeFlow` + `finish_invoke` 统一 invoke 返回/异常处理

**Files:** Modify `src/runtime/interpreter/invoke.rs`(枚举 + helper + 4 函数签名/尾部)、
`mod.rs`(4 分派臂 + 4 invoke 被调用者构造器换 `new_with_exception_table`)

- [ ] **Step 1: 加 `InvokeFlow` 枚举**(invoke.rs 顶部)

```rust
/// invoke 后调用者分派循环的流向。
pub(super) enum InvokeFlow {
    /// 正常返回(含 void);调用方推进 pc(+3 / +5)。
    Fallthrough,
    /// 捕获被调用者抛出的异常并设好处理帧;调用方跳 handler_pc(不推进)。
    Jump(usize),
}
```

- [ ] **Step 2: 加 `finish_invoke` helper**(invoke.rs,`push_return` 旁;引入 exception 模块)

invoke.rs 顶部 use 增 `use super::exception;`,并:

```rust
/// 统一被调用者结果:正常则按返回类型回填(`Fallthrough`);抛异常则在调用者帧
/// 异常表(`caller_table` @ `caller_pc`)找处理者——命中清栈压异常(`Jump(h)`),
/// 未命中原样 `Err(ThrownException)` 上传。
fn finish_invoke(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    caller_table: &[crate::classfile::attributes::ExceptionTableEntry],
    caller_pc: usize,
    result: Result<Value, VmError>,
    return_type: ReturnDescriptor,
) -> Result<InvokeFlow, VmError> {
    match result {
        Ok(v) => {
            match (return_type, v) {
                (ReturnDescriptor::Void, Value::Void) => {}
                (ReturnDescriptor::FieldType(_), Value::Void) => {
                    return Err(VmError::BadConstant("invoke 期望返回值,被调用者返回 void"));
                }
                (ReturnDescriptor::FieldType(_), val) => push_return(frame, val)?,
                (ReturnDescriptor::Void, _) => {
                    return Err(VmError::BadConstant("invoke void 方法返回了值"));
                }
            }
            Ok(InvokeFlow::Fallthrough)
        }
        Err(VmError::ThrownException(exc)) => match exception::find_handler(
            interp, vm, caller_table, caller_pc, exc,
        )? {
            Some(h) => {
                frame.operands.clear();
                frame.operands.push_reference(exc)?;
                Ok(InvokeFlow::Jump(h))
            }
            None => Err(VmError::ThrownException(exc)),
        },
        Err(e) => Err(e),
    }
}
```

> 确认 invoke.rs 已 `use super::{Interpreter, Value, VmError};` 与
> `use crate::metadata::descriptor::ReturnDescriptor;`(或 `use ...::{FieldType, ReturnDescriptor}`)。
> 按 invoke.rs 既有导入对齐;若 `ReturnDescriptor` 已在作用域则不重复导入。

- [ ] **Step 3: 改 4 个 invoke 函数**

每个(`invoke_static`/`invoke_special`/`invoke_virtual`/`invoke_interface`)三处改:

**(a) 签名**增 `caller_table: &[crate::classfile::attributes::ExceptionTableEntry]`、
`caller_pc: usize`,返回类型 `Result<(), VmError>` → `Result<InvokeFlow, VmError>`。
例:

```rust
pub(super) fn invoke_static(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    methodref_index: u16,
    caller_table: &[crate::classfile::attributes::ExceptionTableEntry],
    caller_pc: usize,
) -> Result<InvokeFlow, VmError> {
```

**(b) 被调用者解释器构造**换表感知:

```rust
    let callee_interp = Interpreter::new_with_exception_table(
        &code.code,
        &target_lc.cf.constant_pool,
        &code.exception_table,
    );
```

**(c) 尾部**原 `let result = run_with_depth(...)?; match (md.return_type, result) {...} Ok(())`
替换为:

```rust
    let result = run_with_depth(vm, |vm| callee_interp.interpret_with(&mut callee, vm))?;
    finish_invoke(interp, frame, vm, caller_table, caller_pc, result, md.return_type)
```

> `invoke_special`/`virtual`/`interface` 同此三改;注意 `invoke_interface` 的 5 字节
> 在分派臂处理(此处不涉及)。`run_with_depth` 的 `?` 仍透传非异常错误。

- [ ] **Step 4: 改 4 个分派臂**(mod.rs)

```rust
                Opcode::Invokestatic => {
                    let index = self.read_u2(pc + 1)?;
                    match invoke::invoke_static(self, frame, vm, index, self.exception_table, pc)? {
                        invoke::InvokeFlow::Fallthrough => pc += 3,
                        invoke::InvokeFlow::Jump(h) => pc = h,
                    }
                }
```

`invokespecial`/`invokevirtual` 同形(`pc += 3`)。`invokeinterface` 同形但
`pc += 5`(原 `pc += 5` 不变,只在 `Fallthrough` 分支)。每臂读 index 的偏移与
原臂一致(invokeinterface 读法不变)。

- [ ] **Step 5: 看绿**

Run: `cargo test --lib`
Expected: 全绿(既有 invoke 测试不受影响——它们无异常;`finish_invoke` 正常路径
等价于原 match)。

Run: `cargo clippy --all-targets -- -D warnings` → 零告警。

> 若某 invoke 函数原尾部有额外逻辑(如 `invoke_interface` 的 count 读取在尾部前),
> 仅替换 `match (return_type, result)` 块,保留前置逻辑。

- [ ] **Step 6: 提交**

```bash
git add src/runtime/interpreter/invoke.rs src/runtime/interpreter/mod.rs
git commit -m "feat(interp): InvokeFlow + finish_invoke(invoke 异常传播)"
```

---

### Task 6: javac 集成闸门 `tests/throw.rs`

**Files:** Create `tests/throw.rs`(复用 checkcast.rs 骨架)

- [ ] **Step 1: 写测试**(整文件;`run` 改用 `new_with_exception_table`,
  `run_err` 断 `ThrownException`——但 `ThrownException(Reference)` 携句柄,断言
  `matches!(.., VmError::ThrownException(_))` 即可)

```rust
//! 集成闸门(Layer 4.7):javac 编 throw/try-catch 的真实 Java,由 rustj 执行,
//! 验证 athrow + 异常表(精确/超类/不匹配/跨帧/catch-all)与 JVM 一致。需 javac。

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

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-thr-{}-{s}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功"))
                .expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = match cf.constant_pool.get(m.name_index).unwrap() {
                ConstantPoolEntry::Utf8(s) => s == name,
                _ => false,
            };
            let d = match cf.constant_pool.get(m.descriptor_index).unwrap() {
                ConstantPoolEntry::Utf8(s) => s == desc,
                _ => false,
            };
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new_with_exception_table(&code.code, &lc.cf.constant_pool, &code.exception_table);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .unwrap_or_else(|e| panic!("{name}{desc} 执行失败:{e}"))
}

fn run_err(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> VmError {
    let lc = reg
        .get(class_name)
        .unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m
        .code
        .as_ref()
        .unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new_with_exception_table(&code.code, &lc.cf.constant_pool, &code.exception_table);
    let mut vm = Vm::new(reg);
    interp
        .interpret_with(&mut frame, &mut vm)
        .expect_err("期望失败")
}

const SOURCE: &str = r#"
public class ThrowGate {
    static class BaseExc extends Throwable {}
    static class SubExc extends BaseExc {}
    static class OtherExc extends Throwable {}

    // 精确 catch
    public static int catchExact() {
        try { throw new SubExc(); } catch (SubExc e) { return 1; }
    }
    // 超类 catch(is_instance 跨用户异常层次)
    public static int catchSuper() {
        try { throw new SubExc(); } catch (BaseExc e) { return 2; }
    }
    // 不匹配 → 传播(顶层 uncaught)
    public static int uncaughtPropagates() {
        try { throw new SubExc(); } catch (OtherExc e) { return 3; }
    }
    // 跨帧:被调用者抛出无本帧处理者,调用者 invoke 处 catch
    public static int callerCatches() {
        try { calleeThrows(); return 0; } catch (BaseExc e) { return 4; }
    }
    static void calleeThrows() { throw new SubExc(); }
    // catch-all(finally 式:catch Throwable 由 javac 编为 catch_type=Throwable;
    // 注:Throwable 未加载 → is_instance 失败;改用 catch 用户根 BaseExc 兜全用户异常)
    public static int catchAll() {
        try { throw new SubExc(); } catch (BaseExc e) { return 5; }
    }
}
"#;

fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(n) => n,
        other => panic!("期望 int,得 {other:?}"),
    }
}

#[test]
fn throw_catch_exact() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchExact", "()I")), 1);
}

#[test]
fn throw_catch_super() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchSuper", "()I")), 2);
}

#[test]
fn throw_uncaught_propagates() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert!(matches!(
        run_err(&reg, "ThrowGate", "uncaughtPropagates", "()I"),
        VmError::ThrownException(_)
    ));
}

#[test]
fn throw_cross_frame_caller_catches() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "callerCatches", "()I")), 4);
}

#[test]
fn throw_catch_all() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "ThrowGate");
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchAll", "()I")), 5);
}
```

> `Interpreter::new_with_exception_table` 须 `pub`——已在 mod.rs(Task 2)。
> `code.exception_table` 为 `Vec<ExceptionTableEntry>`,传 `&code.exception_table`。
> `catchAll` 与 `catchSuper` 字节码等价(catch BaseExc);保留以测 BaseExc 兜住 SubExc
> 的稳定性。`uncaughtPropagates`:javac 编译后 `catch (OtherExc)` 不匹配 →
> SubExc 上传 → `run_err` 得 `ThrownException`。

- [ ] **Step 2: 看红→看绿**

Run: `cargo test --test throw`
Expected: 5 PASS(有 javac)。若 `catch (OtherExc)` 后 javac 在 catch 块后追加
`return` 或 unreachable——uncaughtPropagates 仍应抛(throw 在 try 首条即触发)。

> 关键:`new SubExc()` 经 new(4.1)+ invokespecial `<init>`;`throw` 经 athrow;
> `catch (BaseExc)` 经 find_handler + is_instance;`calleeThrows()` 跨帧经
> `finish_invoke`。任一失败按错误定位(注意 javac 内部类名 `ThrowGate$SubExc` 等)。

- [ ] **Step 3: 提交**

```bash
git add tests/throw.rs
git commit -m "test: Layer 4.7 athrow/异常表 javac 集成闸门"
```

---

### Task 7: 终验

- [ ] `cargo test` → 全绿(单元 + 集成)。
- [ ] `cargo clippy --all-targets -- -D warnings` → 零告警,零 unsafe。
- [ ] 更新 `hotspot-rust-migration-project.md`:Layer 4 增 4.7 完成条;下一步候选更新
  (内部异常可捕获化 4.7b 提为首选;数组协变/类链接/monitorenter 顺延)。

---

## 自检

- **spec 覆盖:** `athrow`、`find_handler`(精确/超类/不匹配/catch-all/越界)、跨帧
  `finish_invoke`、`ThrownException`、`OperandStack::clear`、null→NPE 均覆盖。
- **类型一致:** `InvokeFlow` 在 invoke.rs 定义、mod.rs 匹配;`finish_invoke` 取代 4 处
  原 `match (return_type, result)`;`new_with_exception_table` 用于 invoke 被调用者 +
  集成入口;`new()` 默认空表(既有测试零改动)。
- **占位符:** Task 4 setup 经修正(实例不跨 Vm)无悬空;无其他占位。
- **顺延:** 内部异常可捕获化(4.7b)、catch(Throwable)/核心异常类、finally 完整语义、
  栈轨迹——已在 spec §9 与本计划注明。
