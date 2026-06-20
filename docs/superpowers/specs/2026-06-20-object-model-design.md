# Layer 4 — 对象模型设计(rustj):4.1 基础 + new + 字段访问

- 日期:2026-06-20
- 对应 HotSpot 源:`src/hotspot/share/oops/{instanceKlass,instanceOop,oopDesc}.hpp`、
  `gc/shared/collectedHeap.hpp`(堆)、`interpreter/zero/bytecodeInterpreter.cpp` 的对象/字段分支。
- 上游:`[[hotspot-rust-migration-project]]` 的第 4 层;依赖 Layer 1–3。
- 本文档主述 **4.1**(用户已确认范围:基础 + `new` + 字段访问)。4.2(对象方法分派
  `invokevirtual`/`invokespecial`/`invokeinterface`)、4.3(数组)在其后增量细化。

## 1. 目标

让解释器能**创建对象、读写实例字段与静态字段**,跑通真实 `javac` 编译的、使用对象
字段的 Java 方法(如 `new Point(); p.x = 3; return p.x + p.y;` 与静态计数器)。

不做(留待 4.2/4.3):对象方法调用(`<init>`/`invokevirtual`)、数组、GC、`<clinit>`
自动执行、`checkcast`/`instanceof`/`athrow`、多层继承字段叠加(4.1 仅 Object 超类)。

## 2. 表示决策(已与用户确认)

- **引用 = u32 句柄**:沿用 Layer 2 的 `Reference(Option<u32>)`,不改其类型(避免波及
  `Slot`/`OperandStack`/`LocalVars`)。堆按 id 索引,等价于 HotSpot 的对象地址(用 id 代替裸指针,安全)。
- **堆 = id-arena**:`Heap { objects: Vec<Oop> }`,`alloc` 追加并返回 `Reference(id=index)`。
  无 GC、无回收(测试用量有限;GC 留待后续层)。
- **实例字段存储 = 每字段一个 `Slot`**:实例字段数组按**声明序**一字段一槽,long/double
  也只占一槽(`Slot::Long`/`Slot::Double`)。类型在 `getfield`/`putfield` 边界按描述符转换
  (读 `Slot::Long` → `push_long` 占两槽;`pop_long` → 写一槽)。与栈/局部变量的两槽约定
  不同,但二者各自独立,在指令边界保持一致。

## 3. 模块

```
src/oops/                     # 对应 HotSpot oops/
  mod.rs                      # 再导出
  oop.rs                      # Oop 枚举(4.1 仅 Instance;数组变体留待 4.3)
  instance.rs                 # InstanceOop:class_name + fields: Vec<Slot>
  klass.rs                    # ResolvedField / LoadedClass / ClassRegistry
src/runtime/
  heap.rs                     # Heap:id-arena
  vm.rs                       # Vm<'a> { heap, registry: &'a ClassRegistry }
  interpreter/{mod,invoke}.rs # interpret 签名改造 + 新指令
```

`lib.rs` 增 `pub mod oops;`;`runtime/mod.rs` 增 `pub mod {heap, vm};`。

## 4. 核心类型

```rust
// oops/oop.rs
pub enum Oop {
    Instance(InstanceOop),
    // 4.3: TypeArray(TypeArray), ObjArray(ObjArray)
}

// oops/instance.rs
pub struct InstanceOop {
    class_name: String,
    fields: Vec<Slot>,            // 按声明序,每字段一槽
}

// oops/klass.rs
pub struct ResolvedField {
    pub name: String,
    pub descriptor: FieldType,
}
pub struct LoadedClass {
    pub cf: ClassFile,            // 拥有;方法查找沿用 Layer 3
    pub instance_fields: Vec<ResolvedField>,   // 序 = 实例槽位序
    pub static_fields: Vec<ResolvedField>,
    pub static_storage: Vec<Slot>,            // 与 static_fields 同序,默认初始化
}
pub struct ClassRegistry {
    classes: HashMap<String, LoadedClass>,
}
```

`ClassRegistry::load(cf)`:取 `this_class_name`,解析每个字段(名/描述符/`ACC_STATIC`),
非静态入 `instance_fields`、静态入 `static_fields` + `static_storage`(默认值),存入 map。
`get(name) -> Option<&LoadedClass>`。

默认值:`Int/Byte/Char/Short/Boolean → Int(0)`、`Long → Long(0)`、`Float → Float(0.0)`、
`Double → Double(0.0)`、`Class/Array → Reference(null)`。

## 5. Vm 与 interpret 改造

```rust
// runtime/vm.rs
pub struct Vm<'a> {
    pub heap: Heap,
    pub registry: &'a ClassRegistry,
}

// interpret 签名(由 frame 改为 frame + vm):
pub fn interpret(&self, frame: &mut Frame, vm: &mut Vm) -> Result<Value, VmError>;
```

- **Interpreter 删除 `classes` 字段与 `ClassProvider`/`with_classes`**(类解析统一走 `Vm.registry`)。
- `invokestatic` 经 `invoke::invoke_static(self, frame, vm, index)`,递归 `callee.interpret(callee_frame, vm)`
  reborrow `vm`(registry 为 `&'a`,与 `vm` 的 `&mut` 借用不相干,可并存)。
- 字段指令访问 `vm.heap`(`&mut`)与 `vm.registry`(`&'a`,Copy 出来)。`Vm` 的两个字段
  为不相交借用,可分别 `&mut vm.heap` 与读 `vm.registry`。

**测试侧改动**:既有 122 单元测试走测试 helper(`run_int`/`run_long`/…),只需在各 helper
内构造 `let mut vm = Vm::empty();`(空堆 + 空注册表)并传 `&mut vm`,不逐个改测试。
`empty()` 的注册表对纯数值测试无关(它们不访问类/堆)。

## 6. 新指令(4.1)

| 指令 | 字节 | 语义 |
|------|------|------|
| `aconst_null` | 0x01 | 压 `Reference::null()` |
| `new` | 0xbb | `read_u2` → Class → 名 → 注册表取 `LoadedClass` → 堆分配默认初始化实例 → 压引用 |
| `getfield` | 0xb4 | `read_u2` → Fieldref 解析(类/名/描述符)→ 弹 objref → 按类型读实例槽 → 压 |
| `putfield` | 0xb5 | `read_u2` → 解析 → 弹 value、弹 objref → 按类型写实例槽 |
| `getstatic` | 0xb2 | `read_u2` → 解析静态字段 → 读 `static_storage[序]` → 压 |
| `putstatic` | 0xb3 | `read_u2` → 解析静态字段 → 弹 value → 写 `static_storage[序]` |

**Fieldref 解析**:`Fieldref{class_index, name_and_type_index}` → `(类名, 字段名, 描述符)`。
在 `LoadedClass` 中按名定位字段:实例字段在 `instance_fields`、静态在 `static_fields`,
核对描述符匹配;`getfield/putfield` 要求实例字段、`getstatic/putstatic` 要求静态字段,
不匹配 → `VmError::BadConstant`。

**null 检查**:`getfield`/`putfield` 对 null objref → `VmError::NullPointer`(新增变体)。

## 7. 错误模型扩展

`VmError` 增 `NullPointer`(NullPointerException 语义)。其余沿用既有变体
(`BadConstant` 描述字段/类解析失败,`Frame` 描述栈错误)。

## 8. 测试策略(TDD)

1. **Heap**:`alloc` 返回递增 id;`get`/`get_mut` 命中/越界。
2. **InstanceOop**:按 `LoadedClass` 默认初始化;`get_field`/`set_field` 按序读写。
3. **ClassRegistry**:`load` 一个手搓 `ClassFile`(含实例+静态字段)→ 字段布局正确、静态默认值正确。
4. **Vm 改造**:既有 122 单元 + 4 集成测试全绿(签名改造后)。
5. **集成(执行闸门)**:`tests/object_fields.rs`:`javac` 编译 `Point`(实例字段 x/y)、
   `Counter`(静态字段),执行 `new`/`getfield`/`putfield`/`getstatic`/`putstatic`,结果与 JVM 一致。

每片先红后绿;clippy 零告警,零 unsafe。

## 9. 与 HotSpot 的差异

| 点 | HotSpot | rustj |
|----|---------|-------|
| 引用 | 压缩/原始指针 | u32 句柄 |
| 堆 | `CollectedHeap` 分区 + GC | `Vec<Oop>` id-arena,无 GC |
| 对象头 | mark word + klass 指针 | `class_name: String` + 字段数组 |
| 字段布局 | 字节偏移,对齐填充 | 声明序槽位数组 |
| 类元数据 | `InstanceKlass`(vtable/itable) | `LoadedClass`(4.2 加方法分派表) |
