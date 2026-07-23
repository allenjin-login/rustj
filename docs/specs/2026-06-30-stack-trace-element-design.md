# Layer 4.10r — 真 `Throwable.getStackTrace()` → `StackTraceElement[]`

**日期**:2026-06-30
**北极星**:加载并运行真实 `java.base`,逐步退役合成桩。
**前置**:4.10j(调用链捕获 `record_trace`)、4.10q(`LineNumberTable` + 每帧 bci + `SourceFile`
→ `format_trace` 行号渲染)、4.10i(真 `java/lang/String`)、d97049a(`athrow` + 异常表扫描)。

## 动机(债)

`format_trace` 已能渲染 `at Class.method(File.java:LINE)` 文本,但程序里调
`e.getStackTrace()` 拿不到真 `StackTraceElement[]`。债清单:
「真 `Throwable.getStackTrace()` → `StackTraceElement[]`(需载 `StackTraceElement` 类、
懒转 `backtrace`)」。

## 决策:忠实全量加载(方案 D),非混合桩

早期方案曾取**混合**(桩 `Throwable` 声明 native `getStackTrace` + 真 STE 字段回填)。但
`load_closure(StackTraceElement)` 会**传递性地把真 `Throwable` 替换掉引导桩**(经
`load_or_replace` 语义)——故桩上声明的 native `getStackTrace` 一并失效,真
`Throwable.getStackTrace()` 字节码(`getOurStackTrace().clone()`)直接跑,需要:

1. 数组 `clone()`(`getOurStackTrace()` 返回 STE[] 后 `.clone()`);
2. 真 `Throwable` 的 `backtrace`/`depth` 字段与 native `fillInStackTrace`;
3. native `StackTraceElement.initStackTraceElements` 回填 STE[];
4. `STE.computeFormat` 依赖的 `Class.getClassLoader0`/`Class.getModule`(经 Class 镜像
   native 表)。

混合方案在 ②/① 处撞墙(数组 `clone()` 未实现)。北极星是退役桩,故改为**忠实全量**:跑真
`Throwable.getStackTrace()` 字节码端到端,仅桥接**确属 native** 的最小集。

## Step 0 源码依据(JDK `src/java.base/`)

- `Throwable.getStackTrace()`(`Throwable.java`:857):`return getOurStackTrace().clone();`
- `Throwable.getOurStackTrace()`(861):`stackTrace == UNASSIGNED_STACK/null` 且
  `backtrace != null` → `stackTrace = StackTraceElement.of(backtrace, depth)`。
- `Throwable.fillInStackTrace(int)`(822/831):private native,构造器首调,置 `backtrace`+
  `depth`;返回 this。`backtrace`(transient Object,129)、`depth`(transient int,223)、
  `stackTrace`(`StackTraceElement[]`,初值 `UNASSIGNED_STACK`,218)。
- `StackTraceElement.of(Object, int)`(556):`new StackTraceElement[depth]` + 逐个
  `new StackTraceElement()`(私有无参构造,176)+ native
  `initStackTraceElements(ste, x, depth)`(590)+ `finishInit(ste)`(578,逐个 `computeFormat`)。
- `STE.computeFormat`(466):try 内 `cls = declaringClassObject; loader = cls.getClassLoader0();
  m = cls.getModule(); if (loader instanceof BuiltinClassLoader)…; if (isHashedInJavaBase(m))…;
  format = bits;` finally `declaringClassObject = null;`——**无 catch**,故 `cls` 须非 null。
- `STE.isHashedInJavaBase(null)`(512):`if (!VM.isModuleSystemInited()) return true;`——
  `VM.isModuleSystemInited()`(VM.java:92)= `initLevel >= MODULE_SYSTEM_INITED` 的**字节码**
  (读静态 `initLevel`,默认 0 < 常量 → 假),故 `!假 = 真` 短路返回,**不 deref null m**。
- `Class.getClassLoader0`/`getModule`(Class.java:987/1005):本为字段读字节码,但 rustj 的
  `invokevirtual`/`invokeinterface` 对 `Oop::Class` 镜像一律短路到 `"java/lang/Class"` 的
  native 表(invoke.rs),故需 native 臂返 null。
- `instanceof null → 0` 不解析类型(type_check.rs:126)→ `loader instanceof BuiltinClassLoader`
  对 null 安全。

## 桥接(仅 native,语义移植 `prims/jvm.cpp`)

`backtrace` 字段 = 异常**自指句柄**(非 null 哨兵);native `initStackTraceElements(ste, this,
depth)` 据 `this` 读 `exception_frames(this)`(4.10j 捕获帧)逆序回填 STE。`depth` = 帧数。

1. **数组 `clone()`**(`invoke_virtual`/`invoke_interface`):receiver 为 `Oop::Array` 且方法名
   `clone` → 同描述符 + 复制元素槽的浅拷贝,经 `finish_invoke` 回返引用;其余数组方法仍顺延。
2. **`capture_backtrace(vm, exc)`**(interpreter/mod.rs):`record_trace` + 在真 Throwable 实例上
   置 `backtrace`=自指 + `depth`=帧数(字段解析失败即桩 → 静默跳过)。`throw_exception` 与
   `Throwable.fillInStackTrace(0)` native 臂均调之。
3. **`Throwable.fillInStackTrace(I)` native**:→ `capture_backtrace` + 返 this。
4. **`StackTraceElement.initStackTraceElements(ste[],x,I)` native**:`init_stack_trace_elements`
   逐帧逆序回填 `declaringClass`(`/`→`.` 点分二进制名)/`methodName`/`fileName`/`lineNumber`
   (经 `frame_source`)+ `declaringClassObject`(声明类 Class 镜像,供随后 computeFormat)。
5. **`Class.getClassLoader0`/`getModule` native**:均返 null(所有类视作引导类)。
6. **`VM.isModuleSystemInited`**:字节码读 `initLevel`(默认 0)→ 自然返假,**无需 native**。

`Vm<'a>` 拆借:先在不可变阶段(借 `registry()` / `exception_frames` / `frame_source`)收集 owned
数据(帧拷贝、字段序号、点分类名),再在可变阶段(`heap_mut` / `intern`)回填。

## 变更点

- `src/runtime/interpreter/invoke.rs`:`invoke_virtual`/`invoke_interface` 加数组 `clone()`
  浅拷贝(receiver `Oop::Array` 分支);原"数组方法顺延"错误保留为非 clone 的兜底。
- `src/runtime/interpreter/mod.rs`:新增 `capture_backtrace`;`throw_exception` 以之取代裸
  `record_trace`。
- `src/runtime/interpreter/native.rs`:`fillInStackTrace` 臂改调 `capture_backtrace`;新增
  `Class.getClassLoader0`/`getModule`→null、`StackTraceElement.initStackTraceElements` 臂 +
  `init_stack_trace_elements` 实现;import `Slot`/`capture_backtrace`。
- `src/runtime/vm.rs`:`frame_source` 升 `pub(crate)`(供 native 回填行号);`exception_frames`
  已 `pub(crate)`(4.10j/本层加)。
- `tests/stack_trace_elements.rs`:javac + jmod 闸门——抛异常 → 捕获 → 调 `getStackTrace`
  → 经真 STE getter(`getClassName`/`getMethodName`/`getLineNumber`)+ `String.equals` 断言
  deep/mid/top + 行号>0,成功返 `st.length`(=3)。
- 删 `tests/probe_ste.rs`(临时探针,功能已被正式闸门取代)。

**STE/Throwable 预载约束**:`Vm` 以不可变借用持注册表,运行期不可追加(同 4.10i 的 String 预载);
故 `StackTraceElement` 须在 `Vm::new` 前 `load_closure` 入注册表(其闭包自动带入真 Throwable,
替换引导桩)。未预载 → 真 `getStackTrace` 解析落空。

## 测试(红→绿)

`St.java`:`deep(){1/0}` / `mid()` / `top()` + `check(Throwable)` 在 Java 侧断言
`st[0..2]` 的 `getClassName=="St"`、`getMethodName` 为 `deep/mid/top`、`getLineNumber>0`,
成功返 `st.length`(=3),失败返负诊断。测试预载 STE + String,跑 `St.top`(抛 → Rust 捕
`ThrownException(r)`),再跑 `St.check(r)`(传 r 为 local[0])→ 断言 `Value::Int(3)`。
`interpret_with` 在 unwind 上 push/pop 对称,故抛出后可立即跑下一方法。

## 顺延

- `Throwable.getMessage`/`getCause` 的字段回读(`detailMessage`/`cause` 字段;`throw_exception`
  的 `record_message` 已并行镜像,可顺手迁移到真字段)。
- 数组 `clone()` 仅浅拷贝(对引用数组够用);多维 / `Cloneable` 协议细节顺延。
- `Vm::exception_meta` 无界增长(用户标记暂可接受)。
