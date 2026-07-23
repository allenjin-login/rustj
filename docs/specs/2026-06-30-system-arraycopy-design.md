# Layer 4.10l — `System.arraycopy` 设计

> 北极星:退役合成桩、运行真 `java.base`。栈轨迹层(4.10k)的 probe 自动报告
> `System.arraycopy` 缺口(`StringBuilder.append → String.getBytes → System.arraycopy`)。
> 本层补上该 native,解锁 StringBuilder / String 字节拷贝 / Arrays / Collections。

## 目标

实现 `java/lang/System.arraycopy(Ljava/lang/Object;ILjava/lang/Object;II)V` 为
编译期 native 分派表(`runtime/interpreter/native.rs`)的一条臂。静态 native,经
`invoke_static` 的 ACC_NATIVE 路径抵达 `native::invoke`,实参正序
`[src, srcPos, dest, destPos, length]`。

## Step 0 源码(HotSpot,逐行核验)

- `prims/jvm.cpp:293-305` `JVM_ArrayCopy`:src/dst 任一 null → `NullPointerException`;
  随后 `s->klass()->copy_array(s, src_pos, d, dst_pos, length)` 按源数组 klass 分派。
- `oops/typeArrayKlass.cpp:108-174`(基本类型数组):
  - L112 dst 非 typeArray → `ArrayStoreException`;L123 element_type 不符 → ASE。
  - L133 `src_pos<0 || dst_pos<0 || length<0` → `ArrayIndexOutOfBoundsException`。
  - L149 `(u32)length+(u32)src_pos > s->length()` 或 dst 变体 → AIOOBE(无符号算术,溢出安全)。
  - L166 `length==0` → return。
  - L173 `ArrayAccess<ARRAYCOPY_ATOMIC>::arraycopy` = memmove(conjoint,重叠安全)。
- `oops/objArrayKlass.cpp:244-316` + `do_copy` L206-242(引用数组):
  - L248 dst 非 objArray → ASE;负值/越界 → AIOOBE(同 type)。
  - L296 `length==0` → return。
  - `do_copy`:**s==d**(同一数组)→ 量体 conjoint(memmove),**不做** checkcast;
    s!=d → `type_check = stype!=bound && !stype->is_subtype_of(bound)`:
    `type_check` 真 → CHECKCAST 拷贝(逐元素,首个不可赋元素处拷完前缀后抛 ASE);
    否则 → DISJOINT 量体拷贝(组件类型已保证可赋,免逐元素检查)。
- `utilities/copy.hpp`:`Copy::conjoint_*` = memmove(前/后向自动择向,重叠安全)。

## 权威检查序

1. `src==null || dest==null` → **NullPointerException**。
2. src 或 dest 非数组 → **ArrayStoreException**(「destination/source type … is not an array」)。
3. 类型相容:
   - 基本类型 src:dest 非同基本组件 → **ASE**。
   - 引用 src:dest 非引用数组 → **ASE**。
4. `srcPos<0 || destPos<0 || length<0` → **ArrayIndexOutOfBoundsException**。
5. `srcPos+length > srcLen || destPos+length > destLen`(i64 算术,溢出安全)→ **AIOOBE**。
6. `length==0` → 返回 void。
7. 拷贝:
   - 基本类型:逐槽 `Slot` 拷贝,src==dest 时按 memmove 择向(防自覆盖)。
   - 引用:src==dest → memmove,免检查;否则若 `src_comp` 子类型 `dest_comp` → 量体;
     否则 checkcast 逐元素(首个不可赋处已拷前缀后抛 ASE)。

## 组件可赋性(component_assignable)

复用 `type_check::array_instanceof`(JVMS §6.5 数组子类型递归):「组件 A 可赋给组件 B」
⟺「数组 `[A` instanceof `[B`」。故
`component_assignable(a, b, reg) = array_instanceof("["+a, "["+b, reg)`。

元素运行时类型 → 组件描述符:
- `Instance("java/lang/String")` → `"Ljava/lang/String;"`;
- `Array("[I")` → `"[I"`(数组描述符即其组件描述符的数组化... 实为直接取);
- `Class` → `"Ljava/lang/Class;"`。

null 元素恒可赋(跳过)。

## 实现

- 新模块 `runtime/interpreter/arraycopy.rs`:`system_arraycopy(vm, src, src_pos, dst, dst_pos, length) -> Result<Value, VmError>`。
  逐元素读(不可变借 `&heap` 取 `Slot`,Slot 为 Copy 即释放)→ (checkcast 时查可赋性)→
  逐元素写(`&mut heap`)。src==dest 且 `dst_pos>src_pos` 时**后向**迭代(memmove)。
- `type_check::array_instanceof` 提升为 `pub(super)` 供本模块复用。
- `native::invoke` 加臂:
  ```rust
  ("java/lang/System", "arraycopy", "(Ljava/lang/Object;ILjava/lang/Object;II)V") =>
      super::arraycopy::system_arraycopy(vm, src, src_pos, dst, dst_pos, length)
  ```
  收参解构(缺参/类型不符 → 内部错误;null 经 `Reference::is_null`)。

## TDD 闸门

- **单元**(arraycopy.rs `#[cfg(test)]`,直调 `system_arraycopy`,手构数组):
  null→NPE;非数组→ASE;基本类型不符(int[]→byte[])→ASE;引用→基本(Object[]→int[])→ASE;
  负值→AIOOBE;越界→AIOOBE;`length==0` 空操作;同类型基本拷贝正确(含 byte 截断语义靠槽统一);
  **重叠** src==dest 双向(dst<src 前向 / dst>src 后向)memmove 正确;
  引用量体(String[]→Object[] 全拷);引用 checkcast(Object[]{String,Thread}→String[]
  拷前缀后 ASE,且 dest 前缀已写、Thread 位未写)。
- **集成**(javac 真程序,`tests/system_arraycopy.rs`):编译用 `System.arraycopy` 的真 Java,
  断言拷贝结果 + 各异常类。
- **probe**:`tests/string_concat.rs`(StringBuilder)转绿并入库。

## 顺延债(本层不做)

- `aastore` 引用赋值检查(arraycopy 自带 checkcast,但 `aastore` 指令仍盲存 → 独立小层)。
- ArrayStoreException / AIOOBE 的 detail message。
- `Arrays.copyOf` / 数组 `clone`(后续在 arraycopy 之上构建)。
