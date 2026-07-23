# 4.10h JDK 系统属性引导 —— 真 `Integer` 探测转绿 设计

## 背景 / 触发

4.10g 让 `Integer.<clinit>` 跑过 `Class.getPrimitiveClass` / `desiredAssertionStatus` /
类字面量 ldc,但 `Integer$IntegerCache.<clinit>` 仍抛 `ExceptionInInitializerError`,根因:

```
IntegerCache.<clinit> → runtimeSetup() → VM.getSavedProperty("…high")
  → VM.getSavedProperty(VM.java:209) 检测 savedProps==null
  → IllegalStateException("Not yet initialized")
```

`VM.savedProps` 仅由 `VM.saveProperties(Map)`(VM.java:237,真实字节码)写入——故必须在
用户代码前运行之(等价 launcher 的 `System.initializeSystemClass` 引导片段)。本层即此引导,
目标是让真实 `java/lang/Integer` 的 `valueOf(42).intValue() == 42` 端到端跑通。

## Step 0 源码核对(每环皆引 file:line)

`saveProperties(Map)`(VM.java:237)的依赖链,逐环核对后**全部可控**:

1. `if (initLevel() != 0) throw`(VM.java:238)—— `initLevel` 为 `volatile int`(VM.java:51)
   默认 0;rustj 的 `VM.initialize()` 空操作(4.10g)不动它 → `initLevel()==0`,放行。
2. `savedProps = props`(VM.java:244)—— 写静态字段 `savedProps`(VM.java:231)。
3. `String s = props.get("sun.nio.MaxDirectMemorySize")`(VM.java:253)—— 真 `HashMap.get`
   于**空表**:`getNode`(HashMap)`(tab = table) != null` → table 为 null(从未 put)→ 短路返
   null;调用链仅触 `hash(key)` → `key.hashCode()`(String.hashCode)。**不触 `equals`。**
4. `s == null` → `directMemory = Runtime.getRuntime().maxMemory()`(VM.java:256):
   - `Runtime.<clinit>` = `private static final Runtime currentRuntime = new Runtime();`
     (Runtime.java:124);`Runtime()` 构造器**空体** `private Runtime() {}`(Runtime.java:141)。
   - `getRuntime()` 仅 `return currentRuntime`(Runtime.java:136-138)。
   - `maxMemory()` `public native long`(Runtime.java:655)。
5. `"true".equals(props.get("sun.nio.PageAlignDirectMemory"))`(VM.java:264)—— 第二次
   `props.get` 同样返 null;`"true".equals(null)` → false(String.equals:`null` 非 String 实例)。

`Integer$IntegerCache.<clinit>`(Integer.java:904)`static { runtimeSetup(); }` 之
`runtimeSetup()`(Integer.java:909):
- `VM.getSavedProperty(...)`(Integer.java:913)—— savedProps 已设 → 返 null(空表)。
- `integerCacheHighPropValue == null` → 跳过 `parseInt` / `Math.max/min`(Integer.java:914-922)。
- `CDS.initializeFromArchive(IntegerCache.class)`(Integer.java:932)—— `CDS.java:130`
  `public static native`;无 CDS → 空操作。
- `loadOrInitializeCache(null)`(Integer.java:936/942):`new Integer[(high-low)+1]`(256)+
  循环 `new Integer(j++)`(Integer.java:950/963-965)。纯字节码,无 native。

故 4.10h 需补的 native / 脚手架为**最小集**(每项单行),外加一处测试侧的 Vm 约束修正。

## 设计 / 变更

### 1. 新增 native 臂(`native.rs`)

- `("java/lang/Runtime", "maxMemory", "()J") => Long(i64::MAX)` —— 对应 jvm.cpp `JVM_MaxMemory`;
  rustj 堆为无界 Vec → 取 `i64::MAX`(directMemory 存值,本场景不用)。
- `("jdk/internal/misc/CDS", "initializeFromArchive", "(Ljava/lang/Class;)V") => Void`
  —— HotSpot `JVM_InitializeFromArchive`:无归档 → 空操作(归档字段留 null),即非 CDS 规范行为。
- `("jdk/internal/misc/CDS", "getCDSConfigStatus", "()I") => Int(0)` —— CDS.<clinit> 经
  `configStatus = getCDSConfigStatus()`(CDS.java:57)调之;0 = 所有 CDS 标志关闭
  (isUsingArchive/isDumpingArchive/… 均假)。

### 2. `String.equals` / `String.hashCode` 脚手架(`native.rs`,临时代码债)

`"true".equals(...)` 在 `Oop::String` 特殊变体上被调——真 String 类未加载前,经 `invoke_virtual`/
`invoke_interface` 的 **String 早分派**(4.10g 已有该路径)路由到 native 表:

- `("java/lang/String", "equals", "(Ljava/lang/Object;)Z")` —— null/非 String → false;
  同引用 → true;否则文本比较。
- `("java/lang/String", "hashCode", "()I")` —— Java 串哈希 `h = 31*h + char`(UTF-16 单元,
  按 `encode_utf16` 迭代对齐 Java 按代码单元求值,补码回绕)。

> **债务:** 这两条臂退役 `Oop::String` 特殊变体、加载真 `java/lang/String` 后即由真字节码取代
> (届时 String.equals/hashCode 跑真字节码,本臂删除)。见 4.10i(顺延)。

### 3. 测试侧:JDK 引导类 + 单一共享 Vm(`tests/real_integer.rs`)

新增 `RustjBootstrap`(javac 编译,`--add-exports java.base/jdk.internal.misc=ALL-UNNAMED`):
```java
class RustjBootstrap {
    static void init() { jdk.internal.misc.VM.saveProperties(new HashMap<String,String>()); }
}
```
测试流:编译 `IntegerGate` + `RustjBootstrap` → 载注册表 → `load_closure(Integer)`(传递性
载 VM→Runtime)+ 显式 `load_closure(HashMap)` → 跑 `RustjBootstrap.init()` → 跑
`IntegerGate.run()`,断言 `Int(42)`。

**关键约束(调试中发现):** 引导与 `run()` 须共用**同一 `Vm`**。静态字段区存于共享注册表
(跨调用持久),但其值是 **Vm 堆句柄**——rustj 堆随 Vm 析构而失效。若引导用临时 Vm、运行用
另一 Vm,则 `savedProps` 存的句柄在新堆里指向错对象(实测:指向了 `getPrimitiveClass("int")`
的 `"int"` 字面量)。这对应真实 JVM **单一全局堆贯穿整个程序**的约定——故测试改用单一
`Vm::new(&registry)` 贯穿引导 + 运行。`compile_dir` 增 `extra: &[&str]` 参数以传 `--add-exports`。

## 实现顺序(实际执行)

1. `native.rs`:补 `maxMemory` + `String.equals`/`hashCode` 脚手架;`invoke.rs` String 早分派
   (4.10g 已具)。
2. 重写 `tests/real_integer.rs`:加 `RustjBootstrap` + `compile_dir(extra)` + 引导 + 去 `#[ignore]`。
3. 跑探测,逐层暴露并绑定 native:CDS.initializeFromArchive → getCDSConfigStatus(各经
   `[native 缺口]` 临时诊断定位,事后删除)。
4. 修复共享 Vm 约束(经 `[getstatic]/[putstatic]` 临时诊断定位句柄失效)。
5. 删全部临时诊断 → 全绿 + clippy 干净。

## 本层闸门

- `tests/real_integer.rs::real_integer_valueof_intvalue_runs` **转绿**(`#[ignore]` 移除):
  真 `Integer.<clinit>` + `IntegerCache.<clinit>`(256 元素缓存)+ `valueOf(42)` 命中缓存 +
  `intValue()` 端到端。
- 全套测试绿 + clippy 干净。

## 债 / 顺延

- **退役 `Oop::String` 特殊变体、加载真 `java/lang/String`**(4.10i):让 equals/hashCode/length
  等跑真字节码,删除本层 String 脚手架。
- **Class 镜像 interning**:`getPrimitiveClass` 每次 alloc 新 oop,非规范(`Integer.TYPE ==
  getPrimitiveClass("int")` 同一性未保证)。
- **栈回溯捕获**:`Throwable.fillInStackTrace` 仍空操作。
- **真实 UTF-8 原始字节存储**(String 当前以 Rust String 存文本,非 Java 紧凑串字节布局)。
