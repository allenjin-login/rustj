# Layer 4.10m — `Float`/`Double` IEEE-754 位转换 native

## 背景 / 触发

string_concat 探针经 `[clinit 诊断]` 定位到下一缺口:

```
[clinit 诊断] java/lang/Math.<clinit> 抛 java/lang/UnsatisfiedLinkError
java/lang/ExceptionInInitializerError
  at java/util/Arrays.copyOfRange
  at java/lang/String.<init>
  at java/lang/StringBuilder.toString
```

`Math.java` 源码**无** `static {}` 块、**无** `native` 方法(其三角/指数法多为
`@IntrinsicCandidate` + 转 `StrictMath`)。其 `<clinit>` 由两个**需运行期求值**的静态字段
初始化器生成(`Math.java:2043-2044`):

```java
private static final long negativeZeroFloatBits  = Float.floatToRawIntBits(-0.0f);
private static final long negativeZeroDoubleBits = Double.doubleToRawLongBits(-0.0d);
```

二者调 `Float.floatToRawIntBits` / `Double.doubleToRawLongBits` —— **native**,未登记 →
`UnsatisfiedLinkError` → `ExceptionInInitializerError`。修复即解锁 `Math.<clinit>` →
`Arrays.copyOfRange`(`Math.min`)→ `String.<init>` → `StringBuilder.toString`。

## Step 0 源码依据

四个 IEEE-754 位转换 native(均 `@IntrinsicCandidate`,HotSpot 经 intrinsic / `sharedRuntime`
提供;非 JDK Java 实现):

| 方法 | 签名 | 源码 | 语义 |
|---|---|---|---|
| `Float.floatToRawIntBits` | `(F)I` | `Float.java:971` | f32 位模式原样重解为 i32(保留 NaN 位) |
| `Float.intBitsToFloat` | `(I)F` | `Float.java:1033` | i32 位模式重解为 f32(`floatToRawIntBits` 之逆) |
| `Double.doubleToRawLongBits` | `(D)J` | `Double.java:1364` | f64 位模式原样重解为 i64 |
| `Double.longBitsToDouble` | `(J)D` | `Double.java:1428` | i64 位模式重解为 f64 |

**关键**:`floatToIntBits` / `doubleToLongBits`(非 raw)是**纯 Java 字节码**包装器 —— NaN 折叠到
规范值(`0x7fc00000` / `0x7ff8000000000000L`),非 NaN 时转调 raw native(`Float.java:928-933`)。
故它们**不**入 native 表;只需上表四个 native 即让包装器正确工作。

## 实现

Rust `f32`/`f64` 的 `to_bits` / `from_bits` 是**安全** std 方法(无 `unsafe`),精确实现上述
"位模式原样重解":

```rust
// floatToRawIntBits: F → I
Value::Float(f) => Ok(Value::Int(f.to_bits() as i32))
// intBitsToFloat: I → F
Value::Int(i)  => Ok(Value::Float(f32::from_bits(i as u32)))
// doubleToRawLongBits: D → J
Value::Double(d) => Ok(Value::Long(d.to_bits() as i64))
// longBitsToDouble: J → D
Value::Long(l)  => Ok(Value::Double(f64::from_bits(l as u64)))
```

挂在 `interpreter/native.rs::invoke` 的 `(class, name, desc)` match 表。实参取 `args.first()`;
缺参/类型不符 → `BadConstant`(理论不可达,签名已定)。

## TDD 闸门

- **单测**(native.rs `#[cfg(test)]`):四 native 各覆盖一代表值 + `±0.0` / 一个 NaN 位
  (raw 保留 NaN 位,与 `floatToIntBits` 折叠语义区分)。用 `Vm::default()` 直调(无注册表依赖)。
- **集成闸门**:string_concat 探针越过 `Math.<clinit>` / `Arrays.copyOfRange` 缺口(探针
  暴露**再下一**缺口即视为本层达成——探针本即"逐缺口下移"的研究工具)。

## 完成判据

- 四 native 单测绿;
- 探针诊断不再报 `Math.<clinit>` `UnsatisfiedLinkError`;
- 临时 `[clinit 诊断]` eprintln **移除**(诊断不入库)。
