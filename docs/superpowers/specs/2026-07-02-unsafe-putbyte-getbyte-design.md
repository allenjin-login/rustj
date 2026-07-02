# Layer 4.10w — `Unsafe.putByte` / `getByte`(byte[] 路径)→ 解锁 int→string

> 状态:设计/实现(2026-07-02)。归属 `/goal` 自治循环(逐层自动提交)。
> 触发:能力探测闸门 `tests/probe_string_builder.rs`——真 `StringBuilder.append(String)` 已跑通(返
> 6),但 `append(int)` 在 `DecimalDigits.uncheckedPutCharLatin1` → `Unsafe.putByte` 处
> `UnsatisfiedLinkError`。同路径(DecimalDigits)解锁 `Integer.toString`/`StringBuilder.append(int)`
> 等 int→string 链,真实 java.base 中普遍。

## 1. 背景

`jdk.internal.misc.Unsafe` 的数组布局 native(`arrayBaseOffset0`/`arrayIndexScale0`,4.10i)已就绪,
但**字节内存原语** `putByte`/`getByte` 未登记 → 任何经 DecimalDigits 的 int→string 转换失败。

`DecimalDigits.uncheckedPutCharLatin1`(`DecimalDigits.java:440-442`):
```java
private static void uncheckedPutCharLatin1(byte[] buf, int charPos, int c) {
    UNSAFE.putByte(buf, ARRAY_BYTE_BASE_OFFSET + charPos, (byte) c);
}
```
`Unsafe`(`Unsafe.java:219/223`):
```java
public native byte getByte(Object o, long offset);
public native void putByte(Object o, long offset, byte x);
```

## 2. 设计

### 2.1 偏移 ↔ 索引(rustj 合成布局)

rustj 无真实偏移内存;`arrayBaseOffset0` 恒返 **16**(本模块同源约定),`byte[]` 刻度 **1**。
故 byte[] 元素 `index` 的偏移 = `16 + index`,`index = offset - 16`。DecimalDigits 的
`ARRAY_BYTE_BASE_OFFSET + charPos` 正是此形。**内部自洽**(putByte 逆算用的 16 与
arrayBaseOffset0 返回值同源),不依赖真实内存布局。

```rust
const ARRAY_BYTE_BASE_OFFSET: i64 = 16;
fn byte_index(offset: i64) -> usize { (offset - ARRAY_BYTE_BASE_OFFSET) as usize }
```
(offset 恒 ≥ 16;< 16 经 `as usize` 回绕为大值,被越界检查兜住 → AIOOBE。)

### 2.2 putByte / getByte(byte[] 路径)

- **putByte(Object, long, byte) → void**:解 `(Reference arr, Long off, Int b)`;`index = byte_index(off)`;
  `heap_mut()` 取 `Oop::Array`;越界 → `ArrayIndexOutOfBoundsException`;否则
  `set_element(index, Slot::Int(b))`(byte[] 元素为 `Slot::Int`,baload 读时 `(v as i8) as i32` 截断,
  故存原始 int 正确)。非数组(原生内存/实例)→ `InternalError`(rustj 不支持裸内存,DecimalDigits
  不会触及)。
- **getByte(Object, long) → byte**:`heap()` 取 `Oop::Array`;`element(index)` → `Slot::Int(v)` →
  `Value::Int(v)`。对称成对(toString 读 byte[] 将需要)。

native 分派臂加于 `native/jdk_internal.rs` 的 `dispatch` match:
```rust
("jdk/internal/misc/Unsafe", "putByte", "(Ljava/lang/Object;JB)V") => put_byte(vm, args),
("jdk/internal/misc/Unsafe", "getByte", "(Ljava/lang/Object;)B") => get_byte(vm, args),
```
native 实参每参数一 `Value`(J = 单个 `Value::Long`,见 invoke.rs:451-455),故 args = `[arr, off, b]`。

### 2.3 范围

仅 byte[] 路径(DecimalDigits 唯一用途)。`putInt`/`putLong`/`putCharUnaligned`/`compareAndSet` 等
其余 Unsafe 原语顺延(UTF16 StringBuilder、并发原语等需要时再加)。无新依赖;沿用
`#![deny(unsafe_code)]`。

## 3. 测试

`tests/string_builder_append.rs`(由 `probe_string_builder.rs` 改名,当前 RED):
- `appendLen()`:append("foo")+append("bar") → 6(已绿,验证 String append 不回归)。
- `appendIntLen()`:append(42) → "42" → 2(本层由 RED→GREEN:Unsafe.putByte 解锁 DecimalDigits)。

全套 + clippy 绿。
