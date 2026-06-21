# Layer 4.4 控制流补全 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 补齐非废弃分支与 switch 指令族(`if_acmpeq`/`if_acmpne`/`ifnull`/`ifnonnull`/
`tableswitch`/`lookupswitch`/`goto_w`),让真实 Java 的 `== null`、引用相等、`switch` 可执行。

**Architecture:** `cond_ref1`/`cond_ref2` 引用分支辅助(形同既有 `cond1`/`cond2`);
`read_s4` + `branch_target_w` 处理 i32 偏移;switch 为 `Interpreter` 私有方法,
填充对齐到 4 字节边界,偏移相对指令地址。

**依据:** `docs/superpowers/specs/2026-06-21-control-flow-design.md`。
节奏:写失败测试 → 看红 → 最小实现 → 看绿 → 提交。命令在 `E:\rustj`。

---

### Task 1: 引用比较分支

**Files:** Modify `src/runtime/interpreter/mod.rs`(辅助 + 4 分派臂 + 测试)

- [ ] **Step 1: 写失败测试**(追加到 `mod.rs` 的 `tests` 末尾,在 `}` 前)

```rust
    // ===== Layer 4.4:控制流(引用分支)=====

    #[test]
    fn ifnull_branches_on_null() {
        use crate::runtime::Reference;
        // local0 = null; aload_0; ifnull +7; iconst_0; ireturn; iconst_1; ireturn
        // null → 跳到 iconst_1 → 返回 1
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Ifnull as u8, 0x00, 0x07,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 1);
        frame.locals.set_reference(0, Reference::null()).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    #[test]
    fn ifnonnull_branches_on_nonnull() {
        use crate::runtime::Reference;
        // local0 = 非空引用; aload_0; ifnonnull +7 → iconst_1
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Ifnonnull as u8, 0x00, 0x07,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(1, 1);
        frame.locals.set_reference(0, Reference::from_id(5)).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    #[test]
    fn if_acmpeq_equal_references_jumps() {
        use crate::runtime::Reference;
        // local0=local1=同一引用; aload_0; aload_1; if_acmpeq +8 → iconst_1
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Aload1 as u8,
            Opcode::IfAcmpeq as u8, 0x00, 0x08,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(2, 2);
        frame.locals.set_reference(0, Reference::from_id(9)).unwrap();
        frame.locals.set_reference(1, Reference::from_id(9)).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }

    #[test]
    fn if_acmpne_distinct_references_jumps() {
        use crate::runtime::Reference;
        // 不同引用; if_acmpne 跳
        let code = [
            Opcode::Aload0 as u8,
            Opcode::Aload1 as u8,
            Opcode::IfAcmpne as u8, 0x00, 0x08,
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(2, 2);
        frame.locals.set_reference(0, Reference::from_id(1)).unwrap();
        frame.locals.set_reference(1, Reference::from_id(2)).unwrap();
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }
```

- [ ] **Step 2: 看红**

Run: `cargo test --lib -- ifnull_branches ifnonnull_branches if_acmpeq if_acmpne`
Expected: FAIL(`Opcode::Ifnull` 等未处理 → `UnsupportedOpcode`)。

- [ ] **Step 3: 加辅助方法**

在 `mod.rs` 的 `cond2` 方法之后、`branch_target` 之前,插入:

```rust
    /// 单引用分支:弹 v,`pred(v)` 为真则跳到 `pc+offset`,否则 `pc+3`。
    fn cond_ref1(
        &self,
        pc: usize,
        pred: impl Fn(super::slot::Reference) -> bool,
        frame: &mut Frame,
    ) -> Result<usize, VmError> {
        let v = frame.operands.pop_reference()?;
        let off = self.read_s2(pc + 1)?;
        Ok(if pred(v) {
            Self::branch_target(pc, off)?
        } else {
            pc + 3
        })
    }

    /// 双引用分支:弹 b(顶)、a(底),`pred(a,b)` 为真则跳,否则 `pc+3`。
    fn cond_ref2(
        &self,
        pc: usize,
        pred: impl Fn(super::slot::Reference, super::slot::Reference) -> bool,
        frame: &mut Frame,
    ) -> Result<usize, VmError> {
        let b = frame.operands.pop_reference()?;
        let a = frame.operands.pop_reference()?;
        let off = self.read_s2(pc + 1)?;
        Ok(if pred(a, b) {
            Self::branch_target(pc, off)?
        } else {
            pc + 3
        })
    }
```

> `Reference` 在文件顶部已 `use super::slot::Reference;`(行 17)。辅助内用全路径
> `super::slot::Reference` 以绕过闭包类型推断的歧义;亦可直接用 `Reference`。
> 若编译报歧义,改用 `Reference`(已导入)。

- [ ] **Step 4: 4 分派臂**

在 `mod.rs` 分派循环的 `Opcode::Goto => {...}` 臂之后追加:

```rust
                Opcode::IfAcmpeq => pc = self.cond_ref2(pc, |a, b| a == b, frame)?,
                Opcode::IfAcmpne => pc = self.cond_ref2(pc, |a, b| a != b, frame)?,
                Opcode::Ifnull => pc = self.cond_ref1(pc, |v| v.is_null(), frame)?,
                Opcode::Ifnonnull => pc = self.cond_ref1(pc, |v| !v.is_null(), frame)?,
```

- [ ] **Step 5: 看绿**

Run: `cargo test --lib -- ifnull_branches ifnonnull_branches if_acmpeq if_acmpne`
Expected: 4 PASS。

- [ ] **Step 6: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "feat(interp): if_acmpeq/if_acmpne/ifnull/ifnonnull 引用比较分支"
```

---

### Task 2: `goto_w` + i32 偏移辅助

**Files:** Modify `src/runtime/interpreter/mod.rs`

- [ ] **Step 1: 写失败测试**(追加到 tests 末尾)

```rust
    // ===== Layer 4.4:goto_w =====

    #[test]
    fn goto_w_unconditionally_jumps() {
        // iconst_1; goto_w +8(4 字节); iconst_2(跳过); ireturn -> 1
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::GotoW as u8, 0x00, 0x00, 0x00, 0x08,
            Opcode::Iconst2 as u8,
            Opcode::Ireturn as u8,
        ];
        let cp = empty_cp();
        let mut frame = Frame::new(0, 1);
        let interp = Interpreter::new(&code, &cp);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(1));
    }
```

- [ ] **Step 2: 看红**

Run: `cargo test --lib -- goto_w_unconditionally_jumps`
Expected: FAIL(`UnsupportedOpcode(GotoW)`)。

- [ ] **Step 3: 加 read_s4 / branch_target_w + 分派臂**

在 `read_u2` 方法之后插入:

```rust
    fn read_s4(&self, at: usize) -> Result<i32, VmError> {
        let b0 = self.read_u1(at)?;
        let b1 = self.read_u1(at + 1)?;
        let b2 = self.read_u1(at + 2)?;
        let b3 = self.read_u1(at + 3)?;
        Ok(i32::from_be_bytes([b0, b1, b2, b3]))
    }
```

在 `branch_target` 方法之后插入:

```rust
    /// 宽(i32)分支目标:`pc + offset`,负下溢 → BadPc。
    fn branch_target_w(pc: usize, offset: i32) -> Result<usize, VmError> {
        let target = (pc as i64) + (offset as i64);
        if target < 0 {
            return Err(VmError::BadPc(pc));
        }
        Ok(target as usize)
    }
```

分派循环在 `Opcode::Goto => {...}` 后(Task1 已加 if* 后)追加:

```rust
                Opcode::GotoW => {
                    let off = self.read_s4(pc + 1)?;
                    pc = Self::branch_target_w(pc, off)?;
                }
```

- [ ] **Step 4: 看绿**

Run: `cargo test --lib -- goto_w_unconditionally_jumps`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "feat(interp): goto_w + read_s4/branch_target_w i32 偏移辅助"
```

---

### Task 3: `tableswitch`

**Files:** Modify `src/runtime/interpreter/mod.rs`

- [ ] **Step 1: 写失败测试**(追加到 tests 末尾)

```rust
    // ===== Layer 4.4:tableswitch =====

    /// tableswitch:opcode 在 pc=0,填充 3 字节使 default 落在偏移 4。
    /// default=-1(即 pc0=opcode,跳到... 用相对偏移),low=0,high=2,
    /// jump[0]=+12(命中0→iconst_1),jump[1] 走 iconst_2,jump[2] 走 iconst_3,
    /// default 走 iconst_0。
    /// 编码:opcode(1) + pad(3) + default(4) + low(4) + high(4) + 3×offset(12) = 28
    /// 落点紧跟其后:iconst_0(28);ireturn(29);iconst_1(30);ireturn(31);
    ///             iconst_2(32);ireturn(33);iconst_3(34);ireturn(35)
    /// 偏移相对 opcode(pc=0):
    ///   default → iconst_0 @28 → offset +28
    ///   jump[0] → iconst_1 @30 → offset +30
    ///   jump[1] → iconst_2 @32 → offset +32
    ///   jump[2] → iconst_3 @34 → offset +34
    fn tableswitch_code() -> Vec<u8> {
        let mut c = vec![Opcode::Iload0 as u8]; // index 从 local0
        let sw = c.len();                       // switch opcode 地址
        c.push(Opcode::Tableswitch as u8);
        // 填充:使 default 落在 4 字节边界(sw+1 起)
        let pad = (4 - ((sw + 1) % 4)) % 4;
        c.extend(std::iter::repeat(0u8).take(pad));
        let default_off: i32 = (28 - sw) as i32;
        let low: i32 = 0;
        let high: i32 = 2;
        c.extend_from_slice(&default_off.to_be_bytes());
        c.extend_from_slice(&low.to_be_bytes());
        c.extend_from_slice(&high.to_be_bytes());
        // jump offsets: index-low 的落点
        c.extend_from_slice(&((30 - sw) as i32).to_be_bytes()); // [0]
        c.extend_from_slice(&((32 - sw) as i32).to_be_bytes()); // [1]
        c.extend_from_slice(&((34 - sw) as i32).to_be_bytes()); // [2]
        // 落点
        c.push(Opcode::Iconst0 as u8); c.push(Opcode::Ireturn as u8); // default
        c.push(Opcode::Iconst1 as u8); c.push(Opcode::Ireturn as u8); // 0
        c.push(Opcode::Iconst2 as u8); c.push(Opcode::Ireturn as u8); // 1
        c.push(Opcode::Iconst3 as u8); c.push(Opcode::Ireturn as u8); // 2
        c
    }

    #[test]
    fn tableswitch_hits_each_slot() {
        let cp = empty_cp();
        for (idx, expect) in [(0, 1i32), (1, 2), (2, 3)] {
            let code = tableswitch_code();
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(1, 1);
            frame.locals.set_int(0, idx).unwrap();
            assert_eq!(
                interp.interpret(&mut frame).unwrap(),
                Value::Int(expect),
                "index {idx}"
            );
        }
    }

    #[test]
    fn tableswitch_out_of_range_hits_default() {
        let code = tableswitch_code();
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(1, 1);
        frame.locals.set_int(0, 99).unwrap(); // 越界 → default
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(0));
    }
```

- [ ] **Step 2: 看红**

Run: `cargo test --lib -- tableswitch_hits_each_slot tableswitch_out_of_range`
Expected: FAIL(`UnsupportedOpcode(Tableswitch)`)。

- [ ] **Step 3: 实现 `table_switch` 方法**

在 `branch_target_w` 之后插入:

```rust
    /// `tableswitch`:填充对齐 → 读 default/low/high/jump 表 → 按栈顶 index 跳。
    fn table_switch(&self, pc: usize, frame: &mut Frame) -> Result<usize, VmError> {
        let index = frame.operands.pop_int()?;
        let pad = (4 - ((pc + 1) % 4)) % 4;
        let base = pc + 1 + pad;
        let default = self.read_s4(base)?;
        let low = self.read_s4(base + 4)?;
        let high = self.read_s4(base + 8)?;
        let off = if index < low || index > high {
            default
        } else {
            let entry = base + 12 + ((index - low) as usize) * 4;
            self.read_s4(entry)?
        };
        Self::branch_target_w(pc, off)
    }

    /// `lookupswitch`:填充对齐 → 读 default/npairs/对 → 线性匹配栈顶 key。
    fn lookup_switch(&self, pc: usize, frame: &mut Frame) -> Result<usize, VmError> {
        let key = frame.operands.pop_int()?;
        let pad = (4 - ((pc + 1) % 4)) % 4;
        let base = pc + 1 + pad;
        let default = self.read_s4(base)?;
        let npairs = self.read_s4(base + 4)?;
        let mut off = default;
        for i in 0..npairs as usize {
            let pair = base + 8 + i * 8;
            let m = self.read_s4(pair)?;
            if m == key {
                off = self.read_s4(pair + 4)?;
                break;
            }
        }
        Self::branch_target_w(pc, off)
    }
```

> `lookup_switch` 一并写好(Task 4 用),避免回头改。先只接 `tableswitch` 分派臂。

- [ ] **Step 4: tableswitch 分派臂**

在 `Opcode::GotoW => {...}` 后追加:

```rust
                Opcode::Tableswitch => pc = self.table_switch(pc, frame)?,
                Opcode::Lookupswitch => pc = self.lookup_switch(pc, frame)?,
```

(两个 switch 一起接,Task4 不再改分派臂。)

- [ ] **Step 5: 看绿**

Run: `cargo test --lib -- tableswitch_hits_each_slot tableswitch_out_of_range`
Expected: 2 PASS。

Run: `cargo test --lib`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "feat(interp): tableswitch + lookupswitch(填充对齐,i32 跳转表)"
```

---

### Task 4: `lookupswitch` 验证

**Files:** Modify `src/runtime/interpreter/mod.rs`(仅测试;实现已在 Task3 Step3 完成)

- [ ] **Step 1: 写失败测试**(追加到 tests 末尾)

```rust
    // ===== Layer 4.4:lookupswitch =====

    /// lookupswitch:稀疏 key。pairs(key,offset)。
    fn lookupswitch_code() -> Vec<u8> {
        let mut c = vec![Opcode::Iload0 as u8]; // key 从 local0
        let sw = c.len();
        c.push(Opcode::Lookupswitch as u8);
        let pad = (4 - ((sw + 1) % 4)) % 4;
        c.extend(std::iter::repeat(0u8).take(pad));
        let default_off: i32 = (30 - sw) as i32; // iconst_0 @30
        let npairs: i32 = 2;
        c.extend_from_slice(&default_off.to_be_bytes());
        c.extend_from_slice(&npairs.to_be_bytes());
        // 升序对: key=10 → iconst_1 @32; key=20 → iconst_2 @34
        c.extend_from_slice(&10i32.to_be_bytes());
        c.extend_from_slice(&((32 - sw) as i32).to_be_bytes());
        c.extend_from_slice(&20i32.to_be_bytes());
        c.extend_from_slice(&((34 - sw) as i32).to_be_bytes());
        c.push(Opcode::Iconst0 as u8); c.push(Opcode::Ireturn as u8); // default
        c.push(Opcode::Iconst1 as u8); c.push(Opcode::Ireturn as u8); // 10
        c.push(Opcode::Iconst2 as u8); c.push(Opcode::Ireturn as u8); // 20
        c
    }

    #[test]
    fn lookupswitch_matches_key() {
        let cp = empty_cp();
        for (key, expect) in [(10, 1i32), (20, 2)] {
            let code = lookupswitch_code();
            let interp = Interpreter::new(&code, &cp);
            let mut frame = Frame::new(1, 1);
            frame.locals.set_int(0, key).unwrap();
            assert_eq!(
                interp.interpret(&mut frame).unwrap(),
                Value::Int(expect),
                "key {key}"
            );
        }
    }

    #[test]
    fn lookupswitch_unmatched_hits_default() {
        let code = lookupswitch_code();
        let cp = empty_cp();
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(1, 1);
        frame.locals.set_int(0, 999).unwrap();
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(0));
    }
```

- [ ] **Step 2: 看绿**(实现已在 Task3)

Run: `cargo test --lib -- lookupswitch_matches_key lookupswitch_unmatched`
Expected: 2 PASS。

Run: `cargo test --lib`
Expected: 全绿。

- [ ] **Step 3: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "test(interp): lookupswitch key 匹配与 default 兜底"
```

---

### Task 5: javac 集成闸门

**Files:** Create `tests/control_flow.rs`

- [ ] **Step 1: 写测试**(整文件;复用 arrays.rs 骨架)

```rust
//! 集成闸门(Layer 4.4):javac 编译含 == null、引用相等、switch(int) 的真实 Java,
//! 解析 .class 由 rustj 执行,验证 ifnull/if_acmp*/tableswitch/lookupswitch 与 JVM 一致。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm};

fn javac_available() -> bool {
    Command::new("javac").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-cf-{}-{s}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac").arg("-d").arg(&dir).arg(&src).output().expect("javac 失败");
    assert!(out.status.success(), "javac:\n{}", String::from_utf8_lossy(&out.stderr));
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析")).expect("加载");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg.get(class_name).unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = lc.cf.methods.iter()
        .find(|m| match lc.cf.constant_pool.get(m.name_index).unwrap() {
            ConstantPoolEntry::Utf8(s) => s == name,
            _ => false,
        } && match lc.cf.constant_pool.get(m.descriptor_index).unwrap() {
            ConstantPoolEntry::Utf8(s) => s == desc,
            _ => false,
        })
        .unwrap_or_else(|| panic!("未找到 {name}{desc}"));
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(reg);
    interp.interpret_with(&mut frame, &mut vm).unwrap_or_else(|e| panic!("{name}{desc} 失败:{e}"))
}

const SOURCE: &str = r#"
public class ControlFlow {
    // == null → ifnull:传入长度数组视为非 null,返回 1;null 返回 0
    public static int notNullCheck(int[] a) {
        if (a == null) return 0;
        return 1;
    }
    // 引用相等:同一引用 → 1(避免 new 出两个不同对象;用同一参数)
    public static int sameRef(Object a, Object b) {
        if (a == b) return 1;
        return 0;
    }
    // 密集 switch → tableswitch
    public static int denseSwitch(int x) {
        switch (x) {
            case 0: return 100;
            case 1: return 101;
            case 2: return 102;
            default: return -1;
        }
    }
    // 稀疏 switch → lookupswitch
    public static int sparseSwitch(int x) {
        switch (x) {
            case 10: return 1;
            case 100: return 2;
            case 1000: return 3;
            default: return -1;
        }
    }
}
"#;

#[test]
fn ifnull_null_check() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    assert_eq!(run(&reg, "ControlFlow", "notNullCheck", "([I)I"), Value::Int(0));
    // 非 null:需要传一个数组引用。本测试运行无参入口,故另写一个 static 包装见下。
}

#[test]
fn if_acmpeq_same_reference() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    // 无法直接传参(解释 static 无参);仅验证可编译执行:sameRef 入口存在即编译通过。
    // 真正实参路径留待调用约定层。此处断言密集/稀疏 switch。
}

#[test]
fn dense_switch_tableswitch() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    // 无参入口无法传 x;改为验证可加载。switch 的真实执行靠单元测试(已覆盖)。
    assert!(reg.get("ControlFlow").is_some());
}

#[test]
fn sparse_switch_lookupswitch_compiles() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    assert!(reg.get("ControlFlow").is_some());
}
```

> **问题:** 上述 switch/null 的真实执行需要**带参 static 方法**,而既有集成闸门只跑无参
> 入口(`Vm` 不传参)。**修正:** 改写 Java 为无参 static,内部自带数据(本地数组/常量),
> 让 javac 仍编出 ifnull/if_acmp*/tableswitch/lookupswitch,并返回可断言的 int。

把 SOURCE 与测试**整体替换**为(无参、自带数据):

```rust
const SOURCE: &str = r#"
public class ControlFlow {
    // ifnull:new int[]{1} 非 null → 走 a[0] = 1
    public static int nullCheck() {
        int[] a = new int[] { 1, 2, 3 };
        if (a == null) return 0;
        return a[0];
    }
    // if_acmpeq:同一引用比较
    public static int sameRef() {
        int[] a = new int[] { 5 };
        int[] b = a;
        if (a == b) return 1;
        return 0;
    }
    // tableswitch:密集
    public static int denseSwitch() {
        int x = 2;
        switch (x) {
            case 0: return 100;
            case 1: return 101;
            case 2: return 102;
            default: return -1;
        }
    }
    // lookupswitch:稀疏
    public static int sparseSwitch() {
        int x = 100;
        switch (x) {
            case 10: return 1;
            case 100: return 2;
            case 1000: return 3;
            default: return -1;
        }
    }
}
"#;

#[test]
fn ifnull_returns_element_when_nonnull() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    assert_eq!(run(&reg, "ControlFlow", "nullCheck", "()I"), Value::Int(1));
}

#[test]
fn if_acmpeq_same_reference() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    assert_eq!(run(&reg, "ControlFlow", "sameRef", "()I"), Value::Int(1));
}

#[test]
fn dense_switch_hits_case_2() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    assert_eq!(run(&reg, "ControlFlow", "denseSwitch", "()I"), Value::Int(102));
}

#[test]
fn sparse_switch_hits_case_100() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "ControlFlow");
    assert_eq!(run(&reg, "ControlFlow", "sparseSwitch", "()I"), Value::Int(2));
}
```

> 执行时直接写入修正版(无参、自带数据),跳过占位版。

- [ ] **Step 2: 看红→看绿**

Run: `cargo test --test control_flow`
Expected: 4 PASS(有 javac)或全跳过。

> **关键验证:** `new int[]{1,2,3}` 触发 newarray+填充或 anewarray?——`new int[]{...}`
> 编为 `iconst_3; newarray int; dup; iconst_0; iconst_1; iastore; dup; iconst_1; iconst_2; iastore; ...`
> 已在 4.3a 支持。`a == null` 编为 `aload; ifnull`/`ifnonnull`(本层)。switch 编为
> tableswitch/lookupswitch(本层)。任一失败按指令定位。

- [ ] **Step 3: 提交**

```bash
git add tests/control_flow.rs
git commit -m "test: Layer 4.4 控制流 javac 集成闸门"
```

---

### Task 6: 终验

- [ ] `cargo test` → 全绿(单元 + 集成)。
- [ ] `cargo clippy --all-targets -- -D warnings` → 零告警,零 unsafe。
- [ ] 更新 `hotspot-rust-migration-project.md`:Layer 4 增 4.4 完成条;下一步候选更新。

---

## 自检

- **spec 覆盖:** 7 指令全覆盖;`cond_ref1/2`、`read_s4`、`branch_target_w`、
  `table_switch`/`lookup_switch`、填充公式均给齐。
- **类型一致:** 偏移语义——`if*`/`if_acmp*` i16 走 `branch_target`;switch/goto_w i32
  走 `branch_target_w`;`cond_ref1/2` 与 `cond1/2` 同形。
- **占位符:** Task5 占位版已在同 Step 给出修正版替换说明(执行时直接写修正版)。
