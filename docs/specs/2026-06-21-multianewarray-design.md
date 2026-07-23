# Layer 4.3b `multianewarray` 设计

> 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_multianewarray)`。
> 补齐数组维度的最后一块:多维数组分配。`newarray`(单维基本)/`anewarray`(单维引用)
> 已在 4.3a 完成;本层仅 `multianewarray` 一条指令,复用 `ArrayOop{elements:Vec<Slot>}`
> 与既有堆分配,把多维数组表示为**嵌套的 `ArrayOop` 树**(每层元素是下一层数组的引用)。

## 1. 目标

让 rustj 执行真实 Java 的多维数组分配(`new int[2][3]`、`new int[2][3][]` 等),
与 javac 编译的 `.class` 一致。

零 unsafe。

## 2. 指令格式

`multianewarray`(0xc5,长度 4):

```
opcode(1) | index(2) | dimensions(1)
```

- `index`:常量池 `Class` 条目 → 其名(对数组类型即**类型描述符**,如 `[[I`、
  `[[Ljava/lang/Object;`)。与 `anewarray` 复用 `resolve_class_name`。
- `dimensions`(下称 `dims`):实际分配的维数,**≤** 描述符中的总维数 `ndim`。

栈:`..., count1, count2, …, countn → ..., arrayref`(`count1` 最外层,栈顶是 `countn`)。
分配 `n` 维;若 `dims < ndim`,余下维度**不分配**(内层保持 null)。

## 3. 描述符解析

类型描述符 = 若干前导 `[` + 基本组件(`I`/`J`/`F`/`D`/`Z`/`B`/`C`/`S`)或对象组件
(`L…;`)。

```rust
/// 解析数组类型描述符 → (总维数 ndim, 叶子默认槽)。
fn parse_array_descriptor(desc: &str) -> Result<(usize, Slot), VmError> {
    let b = desc.as_bytes();
    let mut ndim = 0;
    while ndim < b.len() && b[ndim] == b'[' { ndim += 1; }
    if ndim == 0 { return Err(BadConstant("multianewarray 描述符非数组")); }
    let base = match b.get(ndim) {
        Some(b'I'|b'Z'|b'B'|b'C'|b'S') => Slot::Int(0),
        Some(b'J') => Slot::Long(0),
        Some(b'F') => Slot::Float(0.0),
        Some(b'D') => Slot::Double(0.0),
        Some(b'L') => Slot::Reference(Reference::null()),
        _ => return Err(BadConstant("multianewarray 非法组件类型")),
    };
    Ok((ndim, base))
}
```

> 叶子默认值与 4.3a `new_array`/`a_new_array` 的默认约定一致(基本类型零值、引用 null)。

## 4. 嵌套分配

弹 `dims` 个 count(栈顶=最内,逆序收集后翻转成 `counts[0..dims]`,`counts[0]`=最外)。
任一 count < 0 → `NegativeArraySize`。`dims > ndim` → `BadConstant`。

递归分配一棵 `ArrayOop` 树:

```rust
fn alloc_multi(vm, counts: &[i32], depth: usize, ndim: usize, base: Slot)
    -> Result<Reference, VmError>
{
    let len = counts[depth] as usize;
    let mut elements = Vec::with_capacity(len);
    let last = depth + 1 == counts.len(); // 本层是实际分配的最后一维
    for _ in 0..len {
        if last {
            if counts.len() < ndim {
                elements.push(Slot::Reference(Reference::null())); // 类型有更多维但未分配
            } else {
                elements.push(base); // 完全分配 → 叶子默认值
            }
        } else {
            let child = alloc_multi(vm, counts, depth + 1, ndim, base)?;
            elements.push(Slot::Reference(child));
        }
    }
    Ok(vm.heap_mut().alloc(Oop::Array(ArrayOop::new(elements))))
}
```

要点:
- **完全分配**(`dims == ndim`):最内层元素为叶子默认值(基本零值/引用 null)。
- **部分分配**(`dims < ndim`):最内层元素为 `Reference::null()`——因为类型上还多
  出 `ndim - dims` 维,这些槽本应指向更深层数组,但未分配故为 null。
- **零维**(`counts[d] == 0`):该层空数组,更深层不被分配(JVM 行为一致)。

> 递归深度 = `dims`(≤255,实际 javac 产物 ≤ 2~3);Rust 调用栈足够,不做迭代化(YAGNI)。

## 5. 错误模型

复用 4.3a 既存变体,不新增:

| 场景 | 错误 |
|------|------|
| 任一 count < 0 | `NegativeArraySize` |
| 描述符非数组 / 非法组件类型 | `BadConstant` |
| `dims > ndim` | `BadConstant` |
| 描述符 CP 条目非 Class | `BadConstant`(由 `resolve_class_name` 抛) |

(数组为空、元素 null 不报错。)

## 6. 分派臂

`src/runtime/interpreter/mod.rs` 数组分派块(`Anewarray` 臂之后):

```rust
Opcode::Multianewarray => {
    let index = self.read_u2(pc + 1)?;
    let dims = self.read_u1(pc + 3)?;
    array::multi_new_array(self, frame, vm, index, dims)?;
    pc += 4;
}
```

`multi_new_array` / `parse_array_descriptor` / `alloc_multi` 置于 `interpreter/array.rs`
(`pub(super)` 入口,私有辅助)。

## 7. 测试策略

TDD 红绿 + 真实字节码闸门:

1. **单元**(`array.rs`):
   - `parse_array_descriptor`:`[[I` → (2, Int(0));`[[Ljava/lang/Object;` → (2, null);
     `[I` → (1, Int(0));`Ljava/lang/String;` → Err;`[` → Err(非法组件)。
   - `multi_new_array`(经字节码或直接调):`[[I` dims=2 counts=[2,3] → 外层 length 2,
     每个 length 3 全 0;`[[[I` dims=2 counts=[2,3] → 最内层 length 3 全 null(部分分配);
     任一 count<0 → NegativeArraySize;dims>ndim → BadConstant。
   - 经 `multianewarray` 字节码分派端到端(构造 `iconst_2; iconst_3; multianewarray [[I 2`,
     断言堆中结构与 `arraylength`/`iaload` 读回一致)。
2. **集成闸门**(`tests/multianewarray.rs`):`javac` 编译 `new int[2][3]`(返回元素和/
   各维长度)、`new int[2][3][]`(部分分配,断言 `a[0][0] == null`)。无 javac 则跳过。

每任务先红(看失败原因正确)后绿,频繁提交。

## 8. 顺延项

- `ArrayStoreException`(`aastore` 组件类型不匹配,需组件类型跟踪);
- 数组上限/OOM 保护;
- `clone()` 对数组的虚分派(浅拷贝嵌套结构)。

## 9. 自检

- 范围:仅 `multianewarray`;`newarray`/`anewarray`/`*aload`/`*astore` 不动。
- 表示:嵌套 `ArrayOop` 树,非扁平——与单维一致(每层一个 `Vec<Slot>`),复用既有堆与
  `ArrayOop` API,无需新 `Oop` 变体或新字段。
- 部分分配语义:`dims < ndim` 时最内层填 null,与 JVM 一致(`a[0][0]` 为 null 而非数组)。
- 偏移/长度:`pc += 4`(opcode+index+dim);`dims` 为 u8,`ndim` 由描述符前导 `[` 计数。
