# Layer 4.10p — `arraycopy` 忠实 detailMessage

## 背景

异常债之三(顺延 4.10o 算术 "/ by zero"):`System.arraycopy` 的自动异常**无消息**。
HotSpot 的 `THROW_MSG` 在 typeArrayKlass / objArrayKlass 各分支带精确诊断文本,
rustj 侧此前只抛类名(ArrayStoreException / ArrayIndexOutOfBoundsException),排查靠猜。

## 源码依据(Step 0)

- `typeArrayKlass::copy_array`(typeArrayKlass.cpp:108-174):
  - 非数组源/目的:`arraycopy: source/destination type {ext} is not an array`
  - 类型不符:`arraycopy: type mismatch: can not copy {src}[] into {dst}[]`
  - 负值:`arraycopy: source/destination index {n} out of bounds for {T}[{len}]` /
    `arraycopy: length {n} is negative`
  - 越界:`arraycopy: last source/destination index {n} out of bounds for {T}[{len}]`
- `objArrayKlass::copy_array`(objArrayKlass.cpp:244-316):同结构,数组类型名作
  "object array"(内部类型描述符不展开)。
- `throw_array_store_exception`(objArrayKlass.cpp:187-203)checkcast 失败两子情形:
  - 组件不可赋值:`type mismatch: can not copy {src}[] into {dst}[]`
  - 组件可赋值但元素实例不可转:`element type mismatch: can not cast one of the
    elements of {src}[] to the type of the destination array, {dst}`

## 落地设计

`arraycopy.rs` 新增私有工具,将内部类型/描述符映射为 HotSpot 外部名:

- `element_external(&str)`(组件描述符)→ 原语返 `type2name_tab`(`I`→`int`、`B`→`byte` …);
  引用 `L…;` → 点分类名(`Ljava/lang/String;`→`java.lang.String`);其余按点分兜底。
- `kind_label(is_prim, comp)` → 原语返 `element_external` 结果;引用返字面 `"object array"`
  (objArrayKlass.cpp:266/269 "object array[%d]")。
- `oop_external_name(vm, r)` → 借 `registry` 查 oop 数组元素 klass 的外部名(供
  "source/destination type … is not an array")。

`system_arraycopy` 的步骤 2–7 改为在 `throw_exception_with_message` 前拼好对应文本:

```
非数组      → "...type {ext} is not an array"
类型不符    → "...type mismatch: can not copy {src}[] into {dst}[]"
负 src_pos  → "...source index {n} out of bounds for {T}[{len}]"
负 dst_pos  → "...destination index {n} out of bounds for {T}[{len}]"
负 length   → "...length {n} is negative"
越界        → "...last source/destination index {n} out of bounds for {T}[{len}]"
checkcast   → 见下
```

checkcast 由 `Checkcast { dst_comp, msg }` 结构承载消息文本,**复制逐元素时**才在首个
不可转元素处抛(ArrayStoreException,HotSpot 在 copy 出错点而非 copy 前 throw):
copy 前预算消息取决于 `component_assignable(dst_comp, src_comp, reg)`——组件整体不可赋值
取 "type mismatch";否则取 "element type mismatch"(真正逐元素转时才可能失败)。

## TDD(已绿)

复用既有合成数组助手(`int_array` / `array_of` / `array_of_refs`),新增三条消息断言
(均先求 `result` 再借 `&vm` 取 `format_trace`,避开 `&mut vm` / `&vm` 同表达式借用冲突):

- `primitive_type_mismatch_carries_message`:int[] → byte[] → ASE
  "arraycopy: type mismatch: can not copy int[] into byte[]"。
- `out_of_bounds_carries_last_source_index_message`:src_pos(1)+length(2)=3 > src_len(2)
  → AIOOBE "arraycopy: last source index 3 out of bounds for int[2]"。
- `negative_srcpos_carries_source_index_message`:src_pos=-1
  → AIOOBE "arraycopy: source index -1 out of bounds for int[1]"。

既有 15 条只断异常类的测试不变(它们不锁消息,故不冲突),全部仍绿。

## 顺延

- `null` → NPE 仍无消息(jvm.cpp `THROW(...NPE())` 无 msg,HotSpot 确然无消息;保持)。
- 行号(4.10q):`LineNumberTable` 解码 + 每帧记抛出点 pc → `pc↔line` 映射。
- 真 `Throwable.getStackTrace()` → `StackTraceElement[]`(依赖行号)。
