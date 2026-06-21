# Layer 4.3b `multianewarray` 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `multianewarray`(0xc5),让 rustj 执行真实 Java 的多维数组分配。

**Architecture:** 数组类型描述符(`[[I`)解析出总维数 `ndim` + 叶子默认槽;弹 `dims` 个
count,递归 `alloc_multi` 造嵌套 `ArrayOop` 树;`dims < ndim` 时最内层填 null。复用
`resolve_class_name` / `ArrayOop` / 堆分配,不新增 `Oop` 变体。

**依据:** `docs/superpowers/specs/2026-06-21-multianewarray-design.md`。
节奏:写失败测试 → 看红 → 最小实现 → 看绿 → 提交。命令在 `E:\rustj`。

---

### Task 1: 描述符解析 `parse_array_descriptor`

**Files:** Modify `src/runtime/interpreter/array.rs`(函数 + 单元测试模块)

- [ ] **Step 1: 写失败测试**(在 `array.rs` 末尾追加)

```rust
#[cfg(test)]
mod multi_tests {
    use super::*;
    use crate::runtime::Slot;

    #[test]
    fn parse_int_2d() {
        let (n, base) = parse_array_descriptor("[[I").unwrap();
        assert_eq!(n, 2);
        assert_eq!(base, Slot::Int(0));
    }

    #[test]
    fn parse_object_2d() {
        let (n, base) = parse_array_descriptor("[[Ljava/lang/Object;").unwrap();
        assert_eq!(n, 2);
        assert_eq!(base, Slot::Reference(crate::runtime::Reference::null()));
    }

    #[test]
    fn parse_long_1d() {
        let (n, base) = parse_array_descriptor("[J").unwrap();
        assert_eq!(n, 1);
        assert_eq!(base, Slot::Long(0));
    }

    #[test]
    fn parse_non_array_rejected() {
        assert!(parse_array_descriptor("Ljava/lang/String;").is_err());
    }
}
```

- [ ] **Step 2: 看红**

Run: `cargo test --lib -- parse_int_2d`
Expected: 编译错误(`parse_array_descriptor` 未定义)。

- [ ] **Step 3: 实现**(在 `array.rs` 的 `a_new_array` 之后插入)

```rust
/// 解析数组类型描述符(`[[I` 等)→ (总维数, 叶子默认槽)。
/// 组件类型决定叶子默认值(基本零值 / 引用 null)。
fn parse_array_descriptor(desc: &str) -> Result<(usize, Slot), VmError> {
    let b = desc.as_bytes();
    let mut ndim = 0;
    while ndim < b.len() && b[ndim] == b'[' {
        ndim += 1;
    }
    if ndim == 0 {
        return Err(VmError::BadConstant("multianewarray 描述符非数组"));
    }
    let base = match b.get(ndim) {
        Some(b'I' | b'Z' | b'B' | b'C' | b'S') => Slot::Int(0),
        Some(b'J') => Slot::Long(0),
        Some(b'F') => Slot::Float(0.0),
        Some(b'D') => Slot::Double(0.0),
        Some(b'L') => Slot::Reference(Reference::null()),
        _ => return Err(VmError::BadConstant("multianewarray 非法组件类型")),
    };
    Ok((ndim, base))
}
```

- [ ] **Step 4: 看绿**

Run: `cargo test --lib -- parse_int_2d parse_object_2d parse_long_1d parse_non_array_rejected`
Expected: 4 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/runtime/interpreter/array.rs
git commit -m "feat(interp): multianewarray 描述符解析"
```

---

### Task 2: 嵌套分配 + 分派臂(完全分配)

**Files:** Modify `src/runtime/interpreter/array.rs`(alloc + 入口)、`mod.rs`(分派臂 + 测试)

- [ ] **Step 1: 写失败测试**(追加到 `mod.rs` 的 `tests` 末尾;先加 CP 辅助)

在 `mod.rs` 测试模块内(既有 `cp_with_int` 旁)加辅助:

```rust
    /// CP:#1=Class(name_index=#2),#2=Utf8(name)。multianewarray 用索引 1。
    fn cp_with_class(name: &str) -> ConstantPool {
        let mut bytes = vec![0x00, 0x03, 0x07, 0x00, 0x02]; // count=3, Class@1->Utf8@2
        bytes.push(0x01); // Utf8 tag
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        ConstantPool::parse(&mut crate::classfile::Reader::new(&bytes)).unwrap()
    }
```

再追加测试:

```rust
    // ===== Layer 4.3b:multianewarray(完全分配)=====

    #[test]
    fn multianewarray_full_outer_length() {
        // iconst_2; iconst_3; multianewarray [[I dims=2; arraylength -> 2
        let cp = cp_with_class("[[I");
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8, 0x00, 0x01, 0x02,
            Opcode::Arraylength as u8,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(2));
    }

    #[test]
    fn multianewarray_full_inner_length_and_leaf() {
        // iconst_2; iconst_3; multianewarray [[I dims=2; iconst_0; aaload; arraylength -> 3
        let cp = cp_with_class("[[I");
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8, 0x00, 0x01, 0x02,
            Opcode::Iconst0 as u8,
            Opcode::Aaload as u8,
            Opcode::Arraylength as u8,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(3));
    }
```

- [ ] **Step 2: 看红**

Run: `cargo test --lib -- multianewarray_full_outer_length multianewarray_full_inner_length_and_leaf`
Expected: FAIL(`UnsupportedOpcode(Multianewarray)`)。

- [ ] **Step 3: 实现 `alloc_multi` + `multi_new_array`**(在 `array.rs` 的 `parse_array_descriptor` 后)

```rust
/// 递归分配嵌套数组树。`counts[depth]` 为当前层长度。
/// 最后一层:`dims == ndim` 填叶子默认值;`dims < ndim` 填 null(余下维度未分配)。
fn alloc_multi(
    vm: &mut Vm<'_>,
    counts: &[i32],
    depth: usize,
    ndim: usize,
    base: Slot,
) -> Result<Reference, VmError> {
    let len = counts[depth] as usize;
    let last = depth + 1 == counts.len();
    let mut elements = Vec::with_capacity(len);
    for _ in 0..len {
        if last {
            if counts.len() < ndim {
                elements.push(Slot::Reference(Reference::null()));
            } else {
                elements.push(base);
            }
        } else {
            let child = alloc_multi(vm, counts, depth + 1, ndim, base)?;
            elements.push(Slot::Reference(child));
        }
    }
    Ok(vm.heap_mut().alloc(Oop::Array(ArrayOop::new(elements))))
}

/// `multianewarray`:解析描述符 → 弹 dims 个 count → 递归分配 → 压外层引用。
pub(super) fn multi_new_array(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    class_index: u16,
    dims: u8,
) -> Result<(), VmError> {
    let name = resolve_class_name(interp.cp(), class_index)?;
    let (ndim, base) = parse_array_descriptor(&name)?;
    if dims == 0 || dims as usize > ndim {
        return Err(VmError::BadConstant("multianewarray dims 与 ndim 不符"));
    }
    let mut counts: Vec<i32> = Vec::with_capacity(dims as usize);
    for _ in 0..dims {
        counts.push(frame.operands.pop_int()?);
    }
    counts.reverse(); // counts[0] = 最外层
    if counts.iter().any(|&c| c < 0) {
        return Err(VmError::NegativeArraySize);
    }
    let r = alloc_multi(vm, &counts, 0, ndim, base)?;
    frame.operands.push_reference(r)?;
    Ok(())
}
```

- [ ] **Step 4: 分派臂**(在 `mod.rs` 的 `Anewarray => {...}` 臂之后)

```rust
                Opcode::Multianewarray => {
                    let index = self.read_u2(pc + 1)?;
                    let dims = self.read_u1(pc + 3)?;
                    array::multi_new_array(self, frame, vm, index, dims)?;
                    pc += 4;
                }
```

- [ ] **Step 5: 看绿**

Run: `cargo test --lib -- multianewarray_full_outer_length multianewarray_full_inner_length_and_leaf`
Expected: 2 PASS。

Run: `cargo test --lib`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add src/runtime/interpreter/array.rs src/runtime/interpreter/mod.rs
git commit -m "feat(interp): multianewarray 嵌套数组分配(完全分配)"
```

---

### Task 3: 部分分配 + 错误用例

**Files:** Modify `src/runtime/interpreter/mod.rs`(仅测试;实现已在 Task2)

- [ ] **Step 1: 写测试**(追加到 `mod.rs` tests 末尾)

```rust
    // ===== Layer 4.3b:multianewarray(部分分配 + 错误)=====

    /// dims=2 < ndim=3:a[0][0] 应为 null(ifnonnull 不跳 → 返回 0)。
    #[test]
    fn multianewarray_partial_inner_is_null() {
        let cp = cp_with_class("[[[I");
        let code = [
            Opcode::Iconst2 as u8,
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8, 0x00, 0x01, 0x02,
            Opcode::Iconst0 as u8,
            Opcode::Aaload as u8,   // a[0] = int[][] len 3
            Opcode::Iconst0 as u8,
            Opcode::Aaload as u8,   // a[0][0] = null(部分分配)
            Opcode::Ifnonnull as u8, 0x00, 0x05, // 非空跳到 iconst_1
            Opcode::Iconst0 as u8,
            Opcode::Ireturn as u8,
            Opcode::Iconst1 as u8,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(interp.interpret(&mut frame).unwrap(), Value::Int(0));
    }

    #[test]
    fn multianewarray_negative_size_rejected() {
        let cp = cp_with_class("[[I");
        let code = [
            Opcode::IconstM1 as u8, // 外层 -1
            Opcode::Iconst3 as u8,
            Opcode::Multianewarray as u8, 0x00, 0x01, 0x02,
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert_eq!(
            interp.interpret(&mut frame),
            Err(crate::runtime::VmError::NegativeArraySize)
        );
    }

    #[test]
    fn multianewarray_dims_exceeds_ndim_rejected() {
        let cp = cp_with_class("[[I"); // ndim=2
        let code = [
            Opcode::Iconst1 as u8,
            Opcode::Iconst1 as u8,
            Opcode::Iconst1 as u8,
            Opcode::Multianewarray as u8, 0x00, 0x01, 0x03, // dims=3 > 2
            Opcode::Ireturn as u8,
        ];
        let interp = Interpreter::new(&code, &cp);
        let mut frame = Frame::new(0, 4);
        assert!(matches!(
            interp.interpret(&mut frame),
            Err(crate::runtime::VmError::BadConstant(_))
        ));
    }
```

- [ ] **Step 2: 看绿**(实现已在 Task2)

Run: `cargo test --lib -- multianewarray_partial_inner_is_null multianewarray_negative_size_rejected multianewarray_dims_exceeds_ndim_rejected`
Expected: 3 PASS。

Run: `cargo test --lib`
Expected: 全绿。

- [ ] **Step 3: 提交**

```bash
git add src/runtime/interpreter/mod.rs
git commit -m "test(interp): multianewarray 部分分配与错误用例"
```

---

### Task 4: javac 集成闸门

**Files:** Create `tests/multianewarray.rs`

- [ ] **Step 1: 写测试**(复用 arrays.rs 骨架;整文件)

```rust
//! 集成闸门(Layer 4.3b):javac 编译多维数组分配的真实 Java,解析 .class 由 rustj 执行,
//! 验证 multianewarray 与 JVM 一致。需要 PATH 中有 `javac`(无则跳过)。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm};

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
    let dir = std::env::temp_dir().join(format!("rustj-mna-{}-{s}-{public_name}", std::process::id()));
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
    assert!(out.status.success(), "javac 编译失败:\n{}", String::from_utf8_lossy(&out.stderr));
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功")).expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods.iter().find(|m| {
        let n = match cf.constant_pool.get(m.name_index).unwrap() {
            ConstantPoolEntry::Utf8(s) => s == name, _ => false,
        };
        let d = match cf.constant_pool.get(m.descriptor_index).unwrap() {
            ConstantPoolEntry::Utf8(s) => s == desc, _ => false,
        };
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg.get(class_name).unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(reg);
    interp.interpret_with(&mut frame, &mut vm).unwrap_or_else(|e| panic!("{name}{desc} 失败:{e}"))
}

const SOURCE: &str = r#"
public class MultiArray {
    // 完全分配 int[2][3]:写 a[0][1]=7, 返回 a[0][1] + a[1][2]
    public static int fullAlloc() {
        int[][] a = new int[2][3];
        a[0][1] = 7;
        a[1][2] = 9;
        return a[0][1] + a[1][2]; // 16
    }
    // 各维长度:int[2][3] -> a.length * a[0].length = 6
    public static int lengths() {
        int[][] a = new int[2][3];
        return a.length * a[0].length; // 6
    }
    // 部分分配 int[2][]:a[0] == null -> 1
    public static int partialIsNull() {
        int[][] a = new int[2][];
        if (a[0] == null) return 1;
        return 0;
    }
    // 三维部分 int[2][3][]:a[0][0] == null -> 1
    public static int threeDimPartial() {
        int[][][] a = new int[2][3][];
        if (a[0][0] == null) return 1;
        return 0;
    }
}
"#;

#[test]
fn full_allocation_write_and_read() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "MultiArray");
    assert_eq!(run(&reg, "MultiArray", "fullAlloc", "()I"), Value::Int(16));
}

#[test]
fn multi_dimension_lengths() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "MultiArray");
    assert_eq!(run(&reg, "MultiArray", "lengths", "()I"), Value::Int(6));
}

#[test]
fn partial_dimension_is_null() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "MultiArray");
    assert_eq!(run(&reg, "MultiArray", "partialIsNull", "()I"), Value::Int(1));
}

#[test]
fn three_dim_partial_inner_null() {
    if !javac_available() { eprintln!("跳过:未找到 javac"); return; }
    let reg = compile_and_load(SOURCE, "MultiArray");
    assert_eq!(run(&reg, "MultiArray", "threeDimPartial", "()I"), Value::Int(1));
}
```

- [ ] **Step 2: 看红→看绿**

Run: `cargo test --test multianewarray`
Expected: 4 PASS(有 javac)或全跳过。

- [ ] **Step 3: 提交**

```bash
git add tests/multianewarray.rs
git commit -m "test: Layer 4.3b multianewarray javac 集成闸门"
```

---

### Task 5: 终验

- [ ] `cargo test` → 全绿(单元 + 集成)。
- [ ] `cargo clippy --all-targets -- -D warnings` → 零告警,零 unsafe。
- [ ] 更新 `hotspot-rust-migration-project.md`:Layer 4 增 4.3b 完成条;4.3a 顺延项里
      `multianewarray` 划掉,下一步候选更新。

---

## 自检

- **spec 覆盖:** `parse_array_descriptor` / `alloc_multi` / `multi_new_array` / 分派臂 / 错误
  (NegativeArraySize、dims>ndim、dims==0)/ 部分分配 null 均覆盖。
- **类型一致:** 复用 `resolve_class_name(interp.cp(), idx)`、`ArrayOop::new(Vec<Slot>)`、
  `vm.heap_mut().alloc`、`frame.operands.pop_int/push_reference`;分派臂形同 `Anewarray`。
- **占位符:** 无;每步含完整代码与命令。
