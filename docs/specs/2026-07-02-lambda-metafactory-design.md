# Layer 4.10aa — `LambdaMetafactory.metafactory`(lambda / SAM 实例化)

> 日期:2026-07-02 · 北极星:运行真 `java.base`,逐步退役合成桩。

## 1. 动机

`invokedynamic` 当前只支持 `StringConcatFactory.makeConcatWithConstants`(4.10u)。
所有 lambda / 函数式接口代码(现代 java.base 与用户代码的高频模式)经
`invokedynamic <samName>(captures)samType`,其引导方法为
`java/lang/invoke/LambdaMetafactory.metafactory` —— 探针确认缺口:

```
[invokedynamic] 未支持的引导方法:java/lang/invoke/LambdaMetafactory.metafactory
```

本层按 (引导方法 类,名) 特判(沿用 4.10u 决策),综合出闭包对象并把 SAM 调用转发到
lambda 实现方法(`lambda$<caller>$0`),解锁函数式 API。

## 2. 源码依据(Step 0)

- `jdk-master/src/java.base/share/classes/java/lang/invoke/LambdaMetafactory.java`
  (line 339-358)`metafactory` 签名:

  ```
  metafactory(Lookup caller, String interfaceMethodName, MethodType factoryType,
              MethodType interfaceMethodType,
              MethodHandle implementation,
              MethodType dynamicMethodType)
  ```

  - 前 3 参(caller/interfaceMethodName/factoryType)由 `invokedynamic` 隐式供给:
    factoryType = 调用点描述符 `(captures)samType`。
  - 后 3 参 = `BootstrapMethods` 的 bootstrap_arguments(`CONSTANT_MethodType` /
    `CONSTANT_MethodHandle`):
    - `[0]` interfaceMethodType(SAM 方法类型),
    - `[1]` implementation(lambda 体 / 方法引用的 `MethodHandle`),
    - `[2]` dynamicMethodType(擦除后的实际 SAM 类型)。
- 真实 HotSpot 经 `InnerClassLambdaMetafactory` **运行时生成合成类**,实现 SAM;
  rustj 沿「按语义移植」决策(同 native 表特判 JVM_*、4.10u makeConcat),**不生成类**,
  用一个闭包 Oop 记实现方法身份 + 捕获,SAM 调用转发实现体。

## 3. 设计

### 3.1 闭包对象表示 —— 新 `Oop::Lambda(LambdaOop)`

```
LambdaOop {
    impl_class: String,   // 实现方法声明类(如 "LamProbe")
    impl_name:  String,   // 实现方法名(如 "lambda$run$0")
    impl_desc:  String,   // 实现方法描述符(如 "(II)I";含捕获在前)
    impl_kind:  u8,       // MethodHandle reference_kind(6 = REF_invokeStatic)
    sam_type:   String,   // 函数式接口内部名(factoryType 返回,剥 L;)
    captures:   Vec<Value>, // 按捕获类型序的值
}
```

- 复用既有「Oop 变体族」模式(Instance/Array/Class 之后的第 4 种)。
- `captures` 存 `Value`(crate 级类型,`oops` 已可见)而非 `invoke.rs` 私有的 `Arg`——
  避开 `oops → interpreter` 的层级倒置;派发处经 `arg_from_value` 还原 `Arg`。

### 3.2 闭包构造 —— `invoke_dynamic` 特判

`invokedynamic` 解析后(已有 `resolve_invoke_dynamic` + `pop_args` 取捕获实参):

1. 解析引导方法引用 → `(bsm_class, bsm_name)`;
2. 命中 `LambdaMetafactory.metafactory` → `build_lambda`:
   - `resolve_impl_handle(cp, bsm_args)`:取 `bsm_args[1]`(MethodHandle)→
     `(impl_class, impl_name, impl_desc, reference_kind)`(复用 `resolve_methodref`)。
   - `sam_type` = factoryType 返回类型(`FieldType::Class` 存的就是裸内部名,无需剥 `L;`)。
   - `captures` = 已弹出的捕获实参 `Vec<Arg>` → `Vec<Value>`。
   - `heap.alloc(Oop::Lambda(...))` → 按调用点返回类型(`samType`)回填引用。

### 3.3 SAM 派发 —— `invoke_interface` / `invoke_virtual` 早期分支

弹 args、弹 objref、null 检查之后,先于运行时类解析:

```
if let Some(Oop::Lambda(lambda)) = vm.heap().get(objref).cloned() {
    return dispatch_lambda(interp, frame, vm, caller_pc, lambda, args, md.return_type);
}
```

`dispatch_lambda`:
1. 仅 `impl_kind == REF_invokeStatic`(6)实现(覆盖无状态 lambda 体 + 静态方法引用 +
   实例捕获 lambda —— javac 把 lambda 体编为 `private static`,实例捕获把 `this` 作显式捕获);
   其余句柄种类(REF_invokeVirtual / newInvokeSpecial 等 = 实例方法引用 / 构造器引用)顺延报错。
2. `ensure_class_initialized(impl_class)`(幂等)。
3. `find_method(impl_class, impl_name, impl_desc)` → 目标方法(私有静态可查)。
4. 局部变量 = **捕获前置 ++ SAM 实参**(实现方法字节码本就期望「捕获…,SAM 形参…」序):
   `captures.into_iter().map(arg_from_value).chain(args)`。
5. 静态实现无 `this` → `run_callee(objref=None)`;实现为 native(方法引用到 native 静态,
   如 `Integer::valueOf`)→ `dispatch_native(this=None)`。
6. 返回类型用调用点(`invokeinterface` 描述符)的 `md.return_type`(= SAM 返回类型)。

### 3.4 穷尽匹配税

新增 `Oop::Lambda` 后,所有对 `Oop` 的穷尽 `match` 须补臂。审计结果:
- 有 `_` 通配臂的(`native/mod.rs:95` `class_arg_name`、`native/java_lang.rs:48` getClass)→
  自动覆盖,无需改。
- 其余穷尽处(`exception.rs` `class_name`、`array.rs` `arraylength`/`load`/`store`、
  `field.rs` `getfield`/`putfield`、`type_check.rs` `object_type`、
  `arraycopy.rs` `descriptor_of`、`invoke.rs` 运行时类提取、`heap.rs` 测试)→ 补臂:
  闭包非数组/非普通实例,统一归入「不可用于该操作」的报错 / panic 分支(同 Class 镜像处理)。
- `checkcast`/`instanceof`(`type_check.rs`):暂报 `(false, None)`(闭包无真实类层次);
  闸门不对闭包做显式转型,故不触发——记债。

## 4. 范围(本层)与顺延

- **本层**:无捕获 + 捕获 lambda、SAM 经 `invokeinterface` / `invokevirtual`、
  静态实现体(含 native 静态方法引用)。
- **顺延(独立债)**:
  1. 实例方法引用(`obj::method`,REF_invokeVirtual —— SAM 接收者绑定为实现首参)、
     构造器引用(`Foo::new`,REF_newInvokeSpecial)。
  2. `LambdaMetafactory.altMetafactory`(带标志位,如桥接 / 串行化标记)。
  3. 闭包的 `checkcast`/`instanceof` 正确性(按 SAM 类型可赋值)。
  4. `MethodHandle` 直接调用(`invokeexact` / `invoke`)。

## 5. 集成闸门(`tests/lambda_end_to_end.rs`)

javac 编 `LambdaGate`(无捕获 + 捕获两类),经真字节码:
- `noCapture()`:`IntUnaryOperator op = x -> x*2; return op.applyAsInt(21);` → **42**。
- `capturing(int base)`:`IntUnaryOperator add = x -> x + base; return add.applyAsInt(5);` → **base+5**。

约束(同 4.10h/y/z):引导(`RustjBootstrap.init()`)与运行共用同一 `Vm`。
