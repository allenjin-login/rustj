# Layer 4.10v — `aastore` 组件类型可赋性检查(ArrayStoreException)

> 状态:设计/实现(2026-07-02)。归属 `/goal` 自治循环(逐层自动提交)。
> 前置:4.10i 已实现 `anewarray`/引用数组,但**有意延后** `aastore` 的 ArrayStoreException;
>       4.7b 的异常二分(可捕获 `ThrownException` vs 内部故障)与 athrow/find_handler 已就绪;
>       4.10w(System.arraycopy)已沉淀 `element_component` / `component_of` / `component_assignable`
>       三个可复用判定原语。

## 1. 背景与动机

引用数组的 `aastore`(`Opcode::Aastore`)当前在 `array_store` 的 `ArrayKind::Ref` 分支**无组件
类型可赋性检查**:任何非 null 引用都被静默写入(JVMS 违例)。HotSpot 在
`objArrayKlass.cpp` 的 `ObjArrayKlass::array_store_incompat` 处对该场景抛
`java/lang/ArrayStoreException`。

`ArrayStoreException` 已在 `src/oops/bootstrap.rs:45` 注册为 bootstrap 桩,超类链
`RuntimeException → Exception → Throwable → Object`,故 `find_handler` 可捕获(与
arraycopy 路径同源,见 `arraycopy.rs:226`)。`array_store` 现已对 null(NPE)、越界(AIOOBE)
走 `throw_exception` → `VmError::ThrownException` → 解释器循环 `find_handler`;ASE 复用同一路径。

## 2. 设计

### 2.1 复用 arraycopy 三原语(升可见性)

`arraycopy.rs` 已有(均为 `fn` 私有):

- `element_component(vm, r) -> Result<String, VmError>`:`Reference` → 组件描述符。
  `Instance("java/lang/String")` → `Ljava/lang/String;`;`Array("[I")` → `[I`;
  `Class(_)` → `Ljava/lang/Class;`;悬空 → `Err`。
- `component_of(desc) -> &str`:数组描述符剥前导 `[`。`[Ljava/lang/String;` → `Ljava/lang/String;`,
  `[[I` → `[I`。
- `component_assignable(a, b, reg) -> bool`:组件 `a` 可赋给组件 `b`(等价「`[a` instanceof `[b`」,
  内部 `array_instanceof`,引用组件方走 `is_instance`)。

三项改为 `pub(super)`,`array.rs` 经 `super::arraycopy::*` 复用。**不重复实现**,避免判定漂移。

### 2.2 `array_store` 注入检查

在现有「不可变借读长度 → 释放 → 抛 AIOOBE / 可变借写」两段式之间,为 `ArrayKind::Ref` +
非 null `Slot::Reference` 元素插入可赋性检查。借用模式:把所有不可变读(registry、数组组件、
元素组件)收敛进一个块,块内完成 `component_assignable` 判定,块结束(借用释放)后再以
`&mut vm` 抛 ASE / 写入。镜像 arraycopy `copy_elements`(`arraycopy.rs:212-231`)的读→判→写分时。

伪代码:
```rust
if let ArrayKind::Ref = kind
    && let Slot::Reference(elem) = &value
    && !elem.is_null()
{
    let not_assignable = {
        let Some(reg) = vm.registry() else {
            return Err(VmError::BadConstant("aastore 组件可赋性检查需类注册表"));
        };
        let array_comp = match vm.heap().get(arrayref) {
            Some(Oop::Array(a)) => component_of(a.class_name()).to_string(),
            _ => return Err(VmError::BadConstant("aastore 目标非数组")),
        };
        let elem_comp = super::arraycopy::element_component(vm, *elem)?;
        !super::arraycopy::component_assignable(&elem_comp, &array_comp, reg)
    };
    if not_assignable {
        return Err(throw_exception(vm, "java/lang/ArrayStoreException"));
    }
}
```

基本数组种类(int/long/…/byte)由 `pop_array_value` 的槽类型保证,不走此查。

### 2.3 语义对照

| 场景 | array_comp | elem_comp | 判定 |
|---|---|---|---|
| `String[]`,存 `String` | `Ljava/lang/String;` | `Ljava/lang/String;` | 同型 → 可赋 ✓ |
| `Object[]`(运行期实为 `String[]`),存 `int[]` | `Ljava/lang/String;` | `[I` | 数组 vs 引用 → ASE ✗ |
| `Object[]`,存 `String` | `Ljava/lang/Object;` | `Ljava/lang/String;` | String is_instance Object → 可赋 ✓ |
| `Number[]`,存 `Integer` | `Ljava/lang/Number;` | `Ljava/lang/Integer;` | Integer is_instance Number → 可赋 ✓ |

## 3. 测试(javac 集成闸门,真 `.class`)

`tests/array_store_ase.rs`:默认 javac 编译,预载真 `String` 闭包(同 `indy_concat.rs` 模式)。

- `mismatch()`:`Object[] a = new String[1]; try { a[0] = new int[1]; return 0; } catch(ASE){return 1;}`
  → 须返 `1`(运行期 `String[]` 存 `int[]` → ASE 被捕获)。
- `okMatch()`:`Object[] a = new String[1]; a[0] = "x"; return a.length;` → 须返 `1`(String 存入
  String[] 合法,无 ASE)。

红:当前静默写入 → `mismatch()` 返 `0`(应为 1)。绿:注入检查后两断言皆过。

## 4. 范围与债

- 本层仅 `aastore`(`ArrayKind::Ref`)。`arraylength`/aload/基本 astore 不受影响。
- 可赋性沿用 `array_instanceof` 的现有精度(已含数组协变 + 引用 `is_instance` 超类/接口链)。
- 无新依赖;沿用 `#![deny(unsafe_code)]`。
