# 4.10i 退役 `Oop::String` —— 加载真实 `java/lang/String` 设计

## 背景 / 触发

4.10h 让真 `Integer.valueOf(42).intValue()` 端到端跑通,但留下两处临时脚手架:
- `native.rs` 的 `("java/lang/String","equals",…)` / `("…","hashCode",…)` 两条臂(在 `Oop::String`
  特殊变体上模拟 String 方法);
- `invoke.rs` 的 `invoke_virtual`/`invoke_interface` **String 早分派**(收者为 `Oop::String` 时先于
  类链解析路由到 native 表)。

`Oop::String` 是一处合成桩(持 Rust `String` 文本),与北极星「退役合成桩、运行真 java.base」
相悖。用户明确要求「**为代码优雅,全面退役 `Oop::String`**」。本层即此退役。

## Step 0 源码核对(每环引 file:line)

`java/lang/String.java`(jdk-master):
- `<clinit>`(String.java:259)仅 `COMPACT_STRINGS = true`——**无 native、无 registerNatives、
  无重依赖**。故加载真 String 不触发 native 级联。
- 实例字段(紧凑串布局):`value: byte[]`(String.java:188,`@Stable`)、`coder: byte`
  (String.java:202,`LATIN1=0`/`UTF16=1`)、`hash: int`(String.java:206)、`hashIsZero: boolean`
  (String.java:214)。
- 唯一 native:`intern()`(String.java:5086)。
- `length()` = `value.length >> coder()`(String.java:1801 附近)——纯字节码,不读 `COMPACT_STRINGS`。
- `equals`/`hashCode` 按 `coder()` 分派到 `StringLatin1` / `StringUTF16` 的同名**静态**方法——
  纯字节码(`StringLatin1.hashCode`:`h = 31*h + (v & 0xff)`;`StringLatin1.equals`:逐字节 `!=`)。

`byte[]` 在 rustj 已就绪(`ArrayOop` 存 `Vec<Slot>`,byte 以 `Slot::Int` 承载):
`baload` 做 `(v as i8) as i32`(符号扩展,JVMS 规范),`StringLatin1.hashCode` 的 `& 0xff`
再把无符号值还原——故槽里存有符号/无符号字节**等价**(归一到同一 baload 结果)。

**结论:** 退役 `Oop::String`、让 String 方法跑真字节码,仅需一处 `intern()` native 桥。
**注(String 自身无级联,但其 `hashCode` 经 `ArraysSupport` 间接需 `Unsafe.<clinit>`,见 §8)。**

## 设计 / 变更

### 1. `Oop` 枚举去 `String` 变体(`oops/oop.rs`)

```text
Oop = Instance | Array | Class          // 删 String(StringOop)
```
字符串 = `java/lang/String` 的 `Oop::Instance`(实例字段 value/coder/hash/hashIsZero)。
删 `oops/string.rs`(`StringOop`)及其 `oops/mod.rs` 导出。

### 2. 真 String 实例的构建 / 读回(`runtime/interpreter/string.rs`,新)

提供三函数(均需注册表,`'a` 借用技巧:`registry()` 不绑 `&self`,故取 `&'a LoadedClass` 后
仍可 `&mut vm` 写堆):

- `intern(vm, text) -> Result<Reference>`:池命中 → 复用;否则 `build` + `StringPool::insert`。
  **先 `clinit::ensure_class_initialized(vm,"java/lang/String")`**(真类首用即初始化,等价 HotSpot
  原始类引导)。`ldc`/`ldc_w` 取 `CONSTANT_String` 经此。
- `build(vm, text) -> Reference`:
  1. `encode_utf16` 取 UTF-16 码元;全 ≤ 0xFF → Latin1(`coder=0`,逐码元一字节),否则 UTF-16
     大端(`coder=1`,每码元两字节)。
  2. 分配 `value: byte[]`(`ArrayOop`,每字节 `Slot::Int((b as i8) as i32)`)。
  3. `new_instance(String)` → 按序号写 `value`(`[B`)/`coder`(`B`);`hash`/`hashIsZero` 取
     默认槽(0/false),与 Java 一致。
- `read_text(vm, r) -> Result<Option<String>>`:`r` 非 String 实例 → `None`;否则读 `value` byte[]
  + `coder` → 解码(Latin1:逐字节→码点;UTF-16:大端对→`from_utf16_lossy`)。供 `getPrimitiveClass`
  取原语名、`intern()` native 取文本键。

### 3. `StringPool` 退化为纯备忘(`runtime/string_pool.rs`)

`HashMap<String, Reference>` 仅 `get`/`insert`(不再碰堆、不再造 `Oop::String`)。构建逻辑移入
interpreter(因其需 `clinit`)。`Vm` 暴露 `string_pool()` / `string_pool_mut()` 访问器;
删 `Vm::intern_string`。

### 4. native 表(`native.rs`)

- `getPrimitiveClass`:收参文本改经 `super::string::read_text` 读回(原 `Oop::String(s).text()`)。
- **删** `("java/lang/String","equals",…)` / `("…","hashCode",…)`(4.10h 脚手架)——真字节码接管。
- **加** `("java/lang/String","intern","()Ljava/lang/String;")`:`read_text(this)` → `intern` →
  返规范引用(对应 `jvm.cpp` `JVM_InternString` / StringTable)。

### 5. 分派(`invoke.rs`)

删 `invoke_virtual`/`invoke_interface` 的 **String 早分派**块及其 `Oop::String(_) => unreachable!`
臂。String 收者现走正常路径:`runtime_class = "java/lang/String"` → `resolve_dispatch` → 真
`equals`/`hashCode` 字节码。**保留** `Oop::Class` 早分派(Class 镜像仍非注册表类)。

### 6. 去除各处 `Oop::String(_)` 匹配臂(枚举变体已删,须同步)

- `type_check.rs`:`Oop::String(_) => "java/lang/String"` 臂删——String 实例经 `Instance.class_name()`
  自然得该名。
- `exception.rs` / `field.rs` / `array.rs` / `heap.rs`:各 match 的 `Oop::String(_)` 臂删
  (枚举剩 Instance/Array/Class,仍穷尽)。

### 7. 原始类约定:`java/lang/String` 须预载

`ldc String` 经 `intern` 需注册表含已加载的真 `java/lang/String`。这对应真实 JVM 的**原始类**
(String 在 VM 启动期即载)。测试闸门经 `load_closure(registry, cp, "java/lang/String")` 预载
(从 `java.base.jmod`);未预载则 `intern` 报「String 未加载」(明确错误,非静默)。

### 8. 实现中发现的两处增量(非原设计所料,记录成案)

跑通真 `String.hashCode` 暴露两处更深的依赖,本层一并补齐:

**(a) `Unsafe` 数组布局 native** —— `StringLatin1.hashCode` → `ArraysSupport.hashCodeOfUnsigned`
→ `ArraysSupport.<clinit>`(读 `Unsafe.ARRAY_*_INDEX_SCALE`)→ `Unsafe.<clinit>`
(`Unsafe.java:61` 经 `runtimeSetup`→`registerNatives`,再初始化 `ARRAY_*_BASE_OFFSET`/
`_INDEX_SCALE` 静态字段)。这些字段 = `theUnsafe.arrayBaseOffset(X[].class)`(Unsafe.java:1222
等,**非 native 字节码包装器**)转调私有 native `arrayBaseOffset0`/`arrayIndexScale0`
(Unsafe.java:3879/3880)。故 `native.rs` 新增两条臂:

- `("jdk/internal/misc/Unsafe","arrayBaseOffset0","(Ljava/lang/Class;)I") => Int(16)`
  (基偏移取常量;rustj 数组无真实偏移,**且不参与 hash 计算**)。
- `("…","arrayIndexScale0","(Ljava/lang/Class;)I")` 按数组组件名定刻度(`[B`/`[Z`→1、
  `[C`/`[S`→2、`[I`/`[F`→4、`[J`/`[D`→8;须 2 的幂,供 `ArraysSupport.exactLog2`)。

**关键(读 ArraysSupport.java:361-382 得):** `vectorizedHashCode` 对 `T_BOOLEAN` 走
`unsignedHashCode` —— **朴素 baload 循环** `result = 31*result + Byte.toUnsignedInt(a[i])`,
**无任何 Unsafe 内存读取**。故只需让 `Unsafe.<clinit>` 成功(上述二 native 不抛),hash 即正确;
**不必**实现 `getLongUnaligned` 等 Unsafe 内存访问原语。`addressSize()`/`pageSize()`/
`isBigEndian()`/`unalignedAccess()` 均为返回常量字段(`ADDRESS_SIZE` 等,`.class` 中已是字面量)
的字节码方法,不经 native。

**(b) `ArrayOop` 增数组类型身份** —— 上述 `(byte[]) array` 编为 `checkcast [B`。原 `ArrayOop`
仅存 `Vec<Slot>`、**不记组件类型**(4.3a 为不做 `ArrayStoreException` 而省),故 `checkcast [B`
恒失败 → `ClassCastException`。本层给 `ArrayOop` 加 `class_name: String`(数组描述符 `[B`/
`[I`/`[Ljava/lang/String;`/`[[I`,对应 HotSpot `typeArrayKlass`/`objArrayKlass` 之名):
`newarray`/`anewarray`/`multianewarray`(逐层 `&desc[depth..]`)与 `string.rs` 的 byte[] 各自
填入;`type_check::matches` 实现数组 `instanceof`(JVMS §6.5:`Object`/`Cloneable`/
`java/io/Serializable` + 同描述符 + `[Ljava/lang/Object;` 组件为引用/数组 + 同维递归 + 引用
组件类层 `is_instance`)。**这是 4.3a 顺延项的前置一部分,顺带落地。**

## 实现顺序

1. 新 `interpreter/string.rs`(`intern`/`build`/`read_text`)+ 注册模块。
2. `ldc`/`ldc_w` 改调 `string::intern`;`StringPool` 退化 + `Vm` 访问器;删 `Vm::intern_string`。
3. `native.rs`:getPrimitiveClass 用 read_text、删 equals/hashCode、加 intern。
4. `invoke.rs`:删 String 早分派 + unreachable 臂。
5. 删 `Oop::String` 变体 + `oops/string.rs`;同步 type_check/exception/field/array/heap 各臂。
6. 调测试:lib(mod.rs ldc-string、native getPrimitiveClass、heap、string_pool 单测改/移,
  由集成闸门覆盖)+ 集成(`string_literals.rs` 预载 String + 验真 String;`real_integer.rs` 确保
  不回归——其 `HashMap.get`/`"true".equals` 现走真 String 字节码)。
7. 全绿 + clippy 干净。

## 本层闸门

- **新增** `string_literals.rs`:javac 编含 `ldc`/`"x"=="x"` 的真 Java,预载真 String,验证
  `greet()` 返的 String 文本经 `read_text` 回读正确;`"x"=="x"` 引用相等(intern)。
- **新增** 真 String 方法闸门:`"abc".length()==3`、`"abc".equals("abc")` 真、
  **`"abc".equals(new String("abc"))` 深 equals**(绕开 `this==o` 短路,真经 StringLatin1.equals 逐
  字节)、`"abc".hashCode()` 匹配 Java 值 96354——**经真字节码**(StringLatin1→ArraysSupport→
  Unsafe 仅初始化),非 native 桩。
- **不回归** `real_integer.rs`:其引导链 `"true".equals(props.get(...))` + `HashMap.get`(键
  `hashCode`)现走真 String 字节码,须仍 42。

## 债 / 顺延

- Class 镜像 interning(`getPrimitiveClass` 每次 alloc 新 oop)。
- 栈回溯捕获(`fillInStackTrace` 空操作)。
- String 字段 `@Stable`/真紧凑串字节布局已忠实(value byte[] + coder);`intern` 表全程序共享
  (单 Vm 约束,见 4.10h)。
- `ArrayStoreException`(4.3a 顺延):数组组件类型现已存于 `ArrayOop.class_name`,可据此做
  `aastore` 的组件可赋值检查;`array instanceof` 的少数边角(如 `int[][] instanceof Object[]`
  已支持,基本类型数组非 `Object[]` 已正确)仍待真实用例驱动补全。
- `Unsafe` 仅绑 `arrayBaseOffset0`/`arrayIndexScale0`(够 `ArraysSupport.<clinit>`);其余 Unsafe
  内存访问原语(`getInt`/`compareAndSet`/…)按真实用例逐步补。
