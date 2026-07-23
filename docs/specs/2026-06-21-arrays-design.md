# Layer 4.3a 数组(单维)设计

> 对应 HotSpot `typeArrayOop` / `objArrayOop` / `arrayOopDesc`,解释器
> `bytecodeInterpreter.cpp` 的 `CASE(_newarray)` / `CASE(_anewarray)` /
> `CASE(_arraylength)` / `CASE(_iaload)` … `CASE(_sastore)`。本层实现**单维数组**;
> `multianewarray`(多维)复杂且罕见,顺延到 **4.3b**。

## 1. 目标

让 rustj 能创建并访问一维数组,与 javac 编译的真实 `.class` 执行结果一致:

- `newarray`(基本类型数组,atype 4..11)、`anewarray`(引用类型数组);
- `arraylength`;
- 8 条加载 `iaload`/`laload`/`faload`/`daload`/`aaload`/`baload`/`caload`/`saload`;
- 8 条存储 `iastore`/`lastore`/`fastore`/`dastore`/`aastore`/`bastore`/`castore`/`sastore`。

零 unsafe(沿用 crate 级 `#![deny(unsafe_code)]`)。

## 2. 数据模型

### 2.1 统一 `ArrayOop`

沿用 4.1 实例字段"每元素一槽"的约定,新增一个统一的数组 oop:

```rust
// src/oops/array.rs
pub struct ArrayOop {
    elements: Vec<Slot>,
}
impl ArrayOop {
    pub(crate) fn new(elements: Vec<Slot>) -> Self { Self { elements } }
    pub fn length(&self) -> usize { self.elements.len() }
    pub fn element(&self, index: usize) -> Slot { self.elements[index] }      // 调用方已做越界检查
    pub fn set_element(&mut self, index: usize, slot: Slot) { self.elements[index] = slot; }
}
```

**为什么统一(不区分 TypeArray/ObjArray):** HotSpot 把数组拆 `typeArrayOop`/`objArrayOop`
是出于布局与 GC 标记的效率;rustj 用 `Vec<Slot>` 承载一切(实例字段已是如此),
元素类型由**指令自身**决定(`iaload` 读 int、`baload` 读 byte…),无需在 oop 里记
组件类型。这把"读出来怎么解释"完全收敛到指令边界的转换,模型最小。

**long/double 占一槽:** 数组的每个逻辑元素恰好一个 `Slot`(`long[]` 长 3 ⇒ 3 个
`Slot::Long`)。这与操作数栈"cat-2 占两槽"不同,但与实例字段模型一致;cat-2 的
双槽语义只在**操作数栈/局部变量**上成立,数组内部按一元素一槽存储。加载时
`laload` 把单个 `Slot::Long` 经 `push_long` 还原成栈上双槽,存储时反之。

### 2.2 `Oop` 增 `Array` 变体

```rust
pub enum Oop {
    Instance(InstanceOop),
    Array(ArrayOop),
}
```

新增变体使既有对 `Oop` 的 `match` 不再穷尽,需逐处补 `Array` 臂(见 §6)。

### 2.3 堆分配

复用 `Heap::alloc(Oop) -> Reference`,数组经 `alloc(Oop::Array(...))` 入堆,与实例
同库同句柄空间——`arraylength` / `*aload` / `*astore` 通过既有 `heap().get` /
`heap_mut().get_mut` 取回。

## 3. 指令语义

### 3.1 `newarray`(atype,1 字节操作数)

栈:`..., count → ..., arrayref`。

| atype | 类型     | 默认值          |
|-------|----------|-----------------|
| 4     | boolean  | `Slot::Int(0)`  |
| 5     | char     | `Slot::Int(0)`  |
| 6     | float    | `Slot::Float(0.0)`  |
| 7     | double   | `Slot::Double(0.0)` |
| 8     | byte     | `Slot::Int(0)`  |
| 9     | short    | `Slot::Int(0)`  |
| 10    | int      | `Slot::Int(0)`  |
| 11    | long     | `Slot::Long(0)` |

- `count < 0` ⇒ `VmError::NegativeArraySize`。
- 非法 atype ⇒ `BadConstant`。
- `vec![default; count as usize]` 后 `alloc`。

### 3.2 `anewarray`(u2 Class 索引)

栈:`..., count → ..., arrayref`。解析 Class 条目校验(组件类型内部名),
但**4.3a 不存储组件类型**(见 §7 顺延项),默认元素为 `Slot::Reference(Reference::null())`。
`count < 0` ⇒ `NegativeArraySize`。

### 3.3 `arraylength`

栈:`..., arrayref → ..., length`。null ⇒ `NullPointer`;非数组 ⇒ `BadConstant`;
压 `length as i32`。

### 3.4 加载 `*aload`(8 条)

栈:`..., arrayref, index → ..., value`。顺序:弹 `index`,弹 `arrayref`,
null 检查,越界检查,读元素,按种类压栈。

| 指令      | 期望槽         | 栈压          | 扩展 |
|-----------|----------------|---------------|------|
| `iaload`  | `Int(v)`       | int           | — |
| `laload`  | `Long(v)`      | long(cat-2)   | — |
| `faload`  | `Float(v)`     | float         | — |
| `daload`  | `Double(v)`    | double(cat-2) | — |
| `aaload`  | `Reference(r)` | reference     | — |
| `baload`  | `Int(v)`       | int           | 符号扩展 `(v as i8) as i32` |
| `caload`  | `Int(v)`       | int           | 零扩展 `(v as u16) as i32` |
| `saload`  | `Int(v)`       | int           | 符号扩展 `(v as i16) as i32` |

槽类型不符(如 `iaload` 取到 `Long`)⇒ `BadConstant`(软断言,替尚未实现的校验器兜底)。
`index < 0 || index >= length` ⇒ `ArrayIndexOutOfBounds`。

### 3.5 存储 `*astore`(8 条)

栈:`..., arrayref, index, value → ...`。**value 在栈顶**,先弹 value(按种类),
再弹 index,再弹 arrayref,null + 越界检查后写。

| 指令       | value 弹取       | 写入槽              |
|------------|------------------|---------------------|
| `iastore`  | `pop_int`        | `Slot::Int(v)`      |
| `lastore`  | `pop_long`       | `Slot::Long(v)`     |
| `fastore`  | `pop_float`      | `Slot::Float(v)`    |
| `dastore`  | `pop_double`     | `Slot::Double(v)`   |
| `aastore`  | `pop_reference`  | `Slot::Reference(r)`|
| `bastore`  | `pop_int`        | `Slot::Int(v)`      |
| `castore`  | `pop_int`        | `Slot::Int(v)`      |
| `sastore`  | `pop_int`        | `Slot::Int(v)`      |

**byte/char/short 存原始 int:** 不在存储时截断,扩展统一推迟到加载侧
(`(v as i8/i16/u16) as i32`)。两者等价:`bastore(200)` 存 `Int(200)`,
`baload` 取 `(200 as i8) as i32 = -56`,与 HotSpot 存字节再符号扩展一致。
越界(含负 index)⇒ `ArrayIndexOutOfBounds`。

## 4. 错误模型

`VmError` 增两个变体(接在 `StackOverflow` 后):

```rust
ArrayIndexOutOfBounds,   // ArrayIndexOutOfBoundsException
NegativeArraySize,       // NegativeArraySizeException
```

复用既有 `NullPointer`(null 数组引用)、`BadConstant`(非数组目标 / 类型不符 / 非法 atype)。

## 5. 模块结构

```
src/oops/
  array.rs       # 新增:ArrayOop
  mod.rs         # pub mod array; pub use array::ArrayOop;
  oop.rs         # Oop::Array(ArrayOop)
src/runtime/interpreter/
  array.rs       # 新增:new_array / a_new_array / array_length / array_load / array_store + ArrayKind
  mod.rs         # mod array; 分派臂 Newarray/Anewarray/Arraylength/8×load/8×store
```

`array.rs` 子模块签名(镜像 `field.rs`:解释器读操作数,本函数执行):

```rust
pub(super) enum ArrayKind { Int, Long, Float, Double, Ref, Byte, Char, Short }

pub(super) fn new_array(frame: &mut Frame, vm: &mut Vm<'_>, atype: u8) -> Result<(), VmError>;
pub(super) fn a_new_array(interp: &Interpreter<'_>, frame: &mut Frame, vm: &mut Vm<'_>, class_index: u16) -> Result<(), VmError>;
pub(super) fn array_length(frame: &mut Frame, vm: &mut Vm<'_>) -> Result<(), VmError>;
pub(super) fn array_load(frame: &mut Frame, vm: &mut Vm<'_>, kind: ArrayKind) -> Result<(), VmError>;
pub(super) fn array_store(frame: &mut Frame, vm: &mut Vm<'_>, kind: ArrayKind) -> Result<(), VmError>;
```

`ArrayKind` 把 8 条加载/8 条存储收敛成各一个函数,分派臂只传种类常量——避免 16 个
几乎雷同的函数(对照 `field.rs` 的 `pop_field_value`/`push_field_value` 同思路)。

## 6. 既有 `Oop` 匹配的补臂

新增 `Array` 变体后这些 `match` 不再穷尽:

| 文件:行 | 现状 | 补臂 |
|---------|------|------|
| `interpreter/field.rs:189`(`getfield`) | `Oop::Instance(i) => i.field(ordinal)` | `Oop::Array(_) => return Err(BadConstant("getfield 目标为数组"))` |
| `interpreter/field.rs:226`(`putfield`) | `Oop::Instance(i) => i.set_field(...)` | 同上 |
| `interpreter/invoke.rs:346,415` | `Oop::Instance(i) => i.class_name()...` | `Oop::Array(_) => return Err(BadConstant("invoke 目标为数组(数组方法 clone 等顺延)"))` |
| `runtime/heap.rs`(测试 69/81/84) | 测试构造实例 | 测试增 `Oop::Array(_) => panic!("期望实例")` |

数组上的 `clone()` 等虚方法调用属真实方法分派,4.3a 不支持,显式报错而非静默错分派。

## 7. 顺延项(明确不在 4.3a)

- **`multianewarray`**(4.3b):维度计数 + 嵌套 `anewarray` 分配,罕见且独立。
- **ArrayStoreException**:`aastore` 的赋值兼容性检查需组件类型跟踪,当前统一模型不记类型 ⇒ 4.3a 一律接受。
- **数组上限**:近 `i32::MAX` 长度的分配会触发原生 OOM(panic),非 `Result`;4.3a 不设上限,测试用小数组。
- 真实校验器(类型检查前移)、数组 `clone()` 虚分派、`fill`/`copyOf` 内建。

## 8. 测试策略

TDD 红绿 + 真实字节码闸门:

1. **单元**(lib):`ArrayOop` 新/读/写/长度;`array_load`/`array_store` 的种类分发;
   `newarray` atype→默认值;byte/char/short 扩展;负长 → `NegativeArraySize`;
   越界 → `ArrayIndexOutOfBounds`;null → `NullPointer`。
2. **集成闸门**(`tests/arrays.rs`):`javac` 编译使用 `int[]`/`byte[]`/`char[]`/
   `long[]`/`double[]`/`Object[]` 的 Java(求和循环、越界保护读写、引用数组),
   解析其 `.class` 用 rustj 真正执行,断言与 JVM 一致。无 `javac` 则跳过。

每任务先红(看失败原因正确)后绿,频繁提交。

## 9. 自检

- 范围:仅单维 18 条指令 + 2 个 oop/错误;`multianewarray` 顺延。
- 占位符:无 TBD;atype 表、扩展表、栈序均给齐。
- 一致性:`ArrayKind` 8 变体与加载/存储表 8 行一一对应;`Oop::Array` 补臂覆盖全部既有匹配点。
