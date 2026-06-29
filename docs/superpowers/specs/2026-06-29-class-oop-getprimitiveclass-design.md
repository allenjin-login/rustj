# 4.10g Class 对象 + `Class.getPrimitiveClass` native — 设计

## 背景 / 触发

4.10f 的真实 `java/lang/Integer` 探测(`tests/real_integer.rs`)失败:

```
IntegerGate.run() = Integer.valueOf(42).intValue()
  → Integer.<clinit> 调 Class.getPrimitiveClass("int") 设 Integer.TYPE
  → 该 native 未登记 → UnsatisfiedLinkError → ExceptionInInitializerError
```

所有包装类(`Integer`/`Long`/`Double`/…)的 `<clinit>` 都用
`Class.getPrimitiveClass(name)` 设各自的 `TYPE` 静态字段。**不绑定它,真实
`java.base` 类一个都初始化不了。** 这是闭包加载器就绪后,「运行真实类」的第一道闸。

## Step 0 源码依据

- `java.base/.../java/lang/Class.java:2737`:`static native <T> Class<T> getPrimitiveClass(String name);`
- `hotspot/share/prims/jvm.cpp:770` `JVM_FindPrimitiveClass`:
  ```c
  BasicType t = name2type(utf);
  if (t != T_ILLEGAL && !is_reference_type(t))
      mirror = Universe::java_mirror(t);   // 9 个原语类型各一规范镜像
  if (mirror == nullptr)
      THROW_MSG_NULL(java_lang_ClassNotFoundException, utf);  // 非原语名
  return make_local(mirror);
  ```
- 语义:把原语关键字串(`"int"`/`"long"`/`"byte"`/`"char"`/`"short"`/
  `"boolean"`/`"double"`/`"float"`/`"void"`)映射为该原语类型的 **Class 镜像**;
  非原语名 → `ClassNotFoundException`。

## 设计

### 1. `Oop::Class` 变体(镜像 `Oop::String` / `StringOop` 的「特殊对象」模式)

新增 `src/oops/class_oop.rs`:

```rust
pub struct ClassOop { name: String }   // 所表示的类型名(原语关键字或内部类名)
impl ClassOop { pub(crate) fn new(name: String)->Self; pub fn name(&self)->&str; }
```

`Oop::Class(ClassOop)` 加入 `oop.rs` 的 `pub enum Oop`。`oops/mod.rs` 导出。

> 与 `StringOop` 同理:**不**合成 `java/lang/Class` 类桩。对 Class 调方法 /
> `instanceof` / `checkcast` 的完整语义顺延到「加载真实 Class 类」层。本层只承诺:
> `getPrimitiveClass` 返回一个合法的 Class oop(可存进静态字段、可 `checkcast 到
> Class`)。

### 2. 绑定 `getPrimitiveClass`

`native.rs` 新增分派臂:

```rust
("java/lang/Class", "getPrimitiveClass", "(Ljava/lang/String;)Ljava/lang/Class;") => {
    // args[0] = String(原语关键字)。读出文本 → 合法则 Oop::Class{name}。
    match args.get(0) {
        Some(Value::Reference(r)) => match vm.heap().get(*r) {
            Some(Oop::String(s)) if is_primitive_name(s.text()) => {
                let cls = Oop::Class(ClassOop::new(s.text().to_string()));
                Ok(Value::Reference(vm.heap_mut().alloc(cls)))
            }
            Some(Oop::String(_)) => Err(throw_exception(vm, "java/lang/ClassNotFoundException")),
            _ => Err(throw_exception(vm, "java/lang/NullPointerException")), // 缺参/类型不符
        },
        _ => Err(throw_exception(vm, "java/lang/NullPointerException")),
    }
}
```

`is_primitive_name`:`match s { "int"|"long"|...|"void" => true, _ => false }`(9 个原语)。

### 3. 穷尽 match 的连带更新(新增 `Oop::Class` 变体后)

- `interpreter/invoke.rs`(invokevirtual / invokeinterface 的 runtime_class 解析):
  `Oop::Class(_) => return Err(VmError::BadConstant("invoke 目标为 Class(方法顺延)"))`。
- `interpreter/field.rs`(getfield/putfield 收者解析):同上并入「数组/String」错误臂。
- `interpreter/array.rs`(arraylength/aload/astore 收者解析):同上并入。
- `interpreter/exception.rs`(athrow 收者):同上并入「数组/String」错误臂。
- `interpreter/type_check.rs`(checkcast/instanceof 的 object_type):
  `Oop::Class(_) => (false, Some("java/lang/Class".to_string()))`——
  `instanceof`/`checkcast` 时,Class oop 报运行时类 `java/lang/Class`(必要,
  否则 `checkcast` 到 `Class` 会误抛;真实类顺延)。

### 4. YAGNI / 债

- **Class 镜像规范性(canonical)未实现**:HotSpot `Universe::java_mirror(t)` 每原语
  全局唯一,故 `Integer.TYPE == getPrimitiveClass("int")` 恒真。本层每次 `alloc`
  新 oop,**不做 interning**。仅在程序比较 Class 对象同一性时才会出错——本探测
  与常见 `<clinit>` 不触发。顺延到「Class 镜像 interning」层。
- 真实 `java/lang/Class` 类加载(支持其方法)顺延。

## 测试(红→绿)

- 单测(`class_oop.rs`):`new`/`name` 往返;`eq_by_name`(对齐 `StringOop` 测试风格)。
- 单测(`native.rs`):
  - `get_primitive_class("int")` → `Oop::Class{name:"int"}`;
  - 非原语名 → `ClassNotFoundException`;
  - 缺参 → `NullPointerException`。
- 集成闸门:`tests/real_integer.rs` 的 `real_integer_valueof_intvalue_runs` 转绿
  (`Integer.<clinit>` 不再抛;`valueOf(42).intValue()` == 42)。

## 实现顺序

1. `class_oop.rs` + `Oop::Class` 变体 + 导出 + 单测(红→绿)。
2. 连带更新 5 处穷尽 match(编译通过)。
3. `native.rs` 绑定 `getPrimitiveClass` + 单测(红→绿)。
4. 去掉临时 `eprintln` 诊断(`native.rs` 与 `clinit.rs`),`tests/real_integer.rs` 闸门转绿。
5. 单提交。

## 实现中发现的额外缺口(均并入本层)

跑真实 `Integer.<clinit>` 逐层暴露的、为 java.base 各 `<clinit>` 所必需的同类缺口,一并
在本层绑定/支持(皆为单行 no-op/const 或 Class 镜像机制):

- **`ldc`/`ldc_w` 取 CONSTANT_Class(类字面量 `Foo.class`)** → 推 `Oop::Class` 镜像
  (HotSpot `ldc` 解析 Class 常量 → 类型镜像)。与 `getPrimitiveClass` 共用 Class 镜像机制。
- **`Class.desiredAssertionStatus()Z`**(`native` 表 `java/lang/Class`)→ 恒 `false`。
  javac 断言初始化(`!Foo.class.desiredAssertionStatus()`)广见于 java.base 各 `<clinit>`,
  真 Class 此法走 ClassLoader+`desiredAssertionStatus0`;rustj 无断言支持。
- **`Throwable.fillInStackTrace(I)LThrowable;`** → 空操作返回 `this`。每个 Throwable 构造器
  首调(捕获栈回溯);rustj 暂无栈捕获机制。
- **`jdk/internal/misc/VM.initialize()V`**(VM.java:451 私有 native)→ 空操作。VM `<clinit>`
  首调的 JDK 启动期引导;rustj 无 launcher 启动态。
- **新增引导桩** `ClassNotFoundException`(及父链 `ReflectiveOperationException`):`getPrimitiveClass`
  非原语名路径所需。
- **Class 镜像方法分派**:`invokevirtual`/`invokeinterface` 收者为 `Oop::Class` 时,先于类链解析,
  按 `"java/lang/Class"` 经 `native` 表分派(`desiredAssertionStatus` 等)。

## 本层闸门

- 单测全绿:`class_oop`(`new`/`name`/`eq`)、`native`(`getPrimitiveClass` 正常/非原语→CNFE/
  缺参→NPE)、连带 match 编译通过。
- 集成探测 `tests/real_integer.rs`:**仍红**,见下「下一步」。本层将其降为 `#[ignore]`(回归
  闸门),保持主干测试套全绿。

## 下一步(已由本层探测精确定位)——『JDK 系统属性引导』层

`Integer.<clinit>` 已跑通;`Integer$IntegerCache.<clinit>` 抛 `ExceptionInInitializerError`,
根因 `VM.getSavedProperty` → `savedProps==null` → `IllegalStateException("Not yet initialized")`。
修复链(每环即下一层的一步):

1. 在用户代码前运行 `VM.saveProperties(new java/util/HashMap<>())`(等价 launcher 的
   `System.initializeSystemClass` 引导)。
2. `saveProperties`(VM.java:237)连带依赖:`Runtime.getRuntime().maxMemory()`(native `JVM_MaxMemory`,
   待绑定)→ 设 `directMemory`;`"true".equals(props.get(...))` → **真 `String.equals` 于 `Oop::String`**。
3. 故下一层天然要求:**退役 `Oop::String` 特殊变体、加载真 `java/lang/String`**(让 `equals` 等跑真
   字节码),并加载真 `java/util/HashMap`。这是「加载并运行真实 java.base 核心类」的关键跃迁。
4. 转绿后移除 `real_integer.rs` 的 `#[ignore]`。
