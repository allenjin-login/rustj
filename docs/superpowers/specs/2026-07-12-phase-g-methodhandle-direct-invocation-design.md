# Phase G: MethodHandle 直接调用(LambdaForm 解释)— 设计

> **状态**:设计(2026-07-12)。承接 B.5(字段 DMH 短路)、4.15b(反射 invoke0/newInstance0)、4.14c。
> **决策来源**:用户在 G.1 捷径(asType→viewAsType 短路,只解锁实例 Field.get/set)与**全量 LambdaForm
> 解释**之间选了后者。本设计据此:实现 MethodHandle 的**通用直接调用**——解释其 LambdaForm 的 `Name[]`
> 图,使 `invokeExact`/`invoke` 对任意(非 DMH)MethodHandle 成立,解锁实例 Field.get/set、真 Stream、内部迭代。
> **北极星**:退役 B.5.2「仅字段 DMH」短路,使任意 BMH/包裹 MH 经真 LF 解释闭环。

---

## 1. 背景:为什么需要 LF 解释

rustj 现状(B.5.2):解释器对 `invokevirtual MethodHandle.{invoke,invokeExact,invokeBasic}` 有签名多态钩子
(`try_method_handle_field_hook`,`src/runtime/interpreter/invoke.rs:151`)——**仅当 receiver 为字段
DirectMethodHandle** 时直读 `member` 做 getfield/putfield/getstatic/putstatic;否则落正常虚分派。

**实例 `Field.get/set` 失败根因**(实测栈,`tests/reflection_field_getset.rs` `#[ignore]` 用例):

```
Field.get(p) → accessor → getter.asType((LObject;)I)        // 非恒等
  → MethodHandle.asTypeUncached (MethodHandle.java:917)
  → MethodHandleImpl.makePairwiseConvert (MethodHandleImpl.java:272)
  → makePairwiseConvertByEditor (MethodHandleImpl.java:296)
      convCount = 1 (Object→Probe checkcast)  → 非 0 → 落 rebind 路径
      BoundMethodHandle mh = target.rebind();              // :305
  → DirectMethodHandle.rebind (DirectMethodHandle.java:148)
      return BoundMethodHandle.makeReinvoker(this);
  → BMH_SPECIES.extendWith(L_TYPE).factory().invokeBasic(...)
  → BoundMethodHandle.<clinit> (BoundMethodHandle.java:399)
  → ClassSpecializer.<clinit> (ClassSpecializer.java:73)
  → ConstantUtils.referenceClassDesc(AOTSafeClassInitializer.class)
  → Class.descriptorString (Class.java:3956) → Class.isHidden (native 缺) → EIIE
```

**关键架构事实**(Step 0 源码核验):

| 事实 | 出处 | 含义 |
|---|---|---|
| `makePairwiseConvertByEditor` convCount==0 走 `viewAsType`(廉价,不建 BMH) | MethodHandleImpl.java:296-298 | 唯一无 BMH 的类型转换路径(但实例 getter 必带 checkcast → convCount≥1) |
| `target.rebind()`(DMH)→ `BoundMethodHandle.makeReinvoker` | DirectMethodHandle.java:148 | 任何非恒等 asType 必创建 BMH |
| BMH 物种经 `BMH_SPECIES.extendWith(...).factory().invokeBasic(...)` | BoundMethodHandle.java:83-281 | **物种类在运行时生成**(按物种键 L/I/J/F/D 组合发射字节码) |
| `ClassSpecializer.generateConcreteSpeciesCode` 用 `ClassFile.of().build(...)` 建物种 classfile | ClassSpecializer.java:588-628 | 物种生成**依赖 Class-File API** |
| `generateConcreteSpeciesCode` 经 `.defineClass(false)` 装载 | ClassSpecializer.java:592 | 需要 `Lookup.defineClass` native(rustj 未绑) |
| `java.lang.classfile`(Class-File API)= 161 .java,**0 native 方法** | `grep "native " src/java.base/.../classfile/` | **纯 Java**,加载即跑,非 native 墙 |
| `BoundMethodHandle` 的物种实例字段、`getter`、`factory` 均为生成类的字节码方法 | BoundMethodHandle.java:213-281 | 物种类一旦 defineClass 入注册表,其方法经解释器即可跑 |

**结论**:全量 LF 解释可行,无不可逾越的墙。三大缺口:(1) `defineClass` native + 若干 Class/runtime
native(经验式补齐);(2) Class-File API 运行时加载(纯 Java,预期"加载即通",但现代 Java 特性密集);
(3) LambdaForm `Name[]` 解释器(本设计核心,新建)。

---

## 2. 三大支柱

### 支柱 1:`defineClass` + native 缺口补齐(经验式,探针驱动)

`Lookup.defineClass(byte[])` → 移植 `JVM_LookupDefineClass`(prims/jvm.cpp)。语义:在指定 `Lookup`
的访问域内,把字节码解析成 `LoadedClass` 并注册到 `ClassRegistry`(rustj 已有 `classfile::parse` →
`ClassRegistry::load`,直接复用),返 Class 镜像。物种类无命名模块归属 → 走 unnamed/boot 路径。

Class/runtime native 缺口随探针逐个补(`Class.isHidden` → false、`Class.descriptorString` 若有 native
段补之、`Class.getComponentType`/`arrayType`/`getDeclaringClass` 等)。**不预绑**——按 RED 失败点最小补。

### 支柱 2:Class-File API 运行时支持(161 文件,纯 Java)

加载 `java/lang/classfile/**` + 其依赖(`java/lang/constant/**`、`jdk/internal/constant/**`、
`java/lang/classfile/attribute/**`、`java/lang/classfile/constantpool/**`、`java/lang/classfile/instruction/**`
等)。**无需写代码**——只需把它们纳入 `load_closure` 载入链;若 `<clinit>`/方法跑挂,按失败点补 native
或修解释器(可能踩出 invokedynamic 收尾、switch 模式、record 等技术债)。这是本 Phase 不确定性最高的支柱。

### 支柱 3:LambdaForm 解释器(核心,新建)

`invokeExact`/`invoke` 在 receiver 非 DMH 时(典型为 BMH),读 `mh.form`(LambdaForm 实例),按其
`Name[]` 图遍历执行。设计见 §3。

---

## 3. LambdaForm 解释器设计

### 3.1 LambdaForm 数据模型(JDK25 实测,LambdaForm.java:127-)

```java
class LambdaForm {
    final int arity;                // 参数数(含 MH receiver)
    final int result;               // names[result] 为返回值下标
    final boolean forceInline;
    @Stable final Name[] names;     // 计算图(拓扑序)
    final Kind kind;                // GENERIC/BOUND/etc.
    MethodType type;                // 入口类型(签名多态用)
    static final class Name {
        final BasicType type;       // L/I/J/F/D/V
        @Stable final Object[] arguments;   // Integer(参数下标) | Name(前驱) | 常量 | MemberName/NamedFunction
        final NamedFunction function;       // null = 参数节点(isParam)或常量
        int index() / boolean isParam()
    }
    static class NamedFunction { final MemberName member; ... }   // 可调用目标
}
```

`Name.arguments` 的元素语义:
- `Integer i`(0 ≤ i < arity):引用第 i 个入口参数(`invokeExact` 实参,locals 绑定)。
- `Name n`:引用前驱计算结果(`n.index` 处的值)。
- 其它(`MemberName`、`Class`、包装类型等):函数绑定的常量(如 `MH_cast.bindTo(ConvSpec)` 的类型字面量)。

### 3.2 执行算法(草案)

```
interpret_lambda_form(mh, args[]) -> Value:
    form = mh.form                                    // 读实例字段
    names = form.names
    values: Slot[ names.length ]                      // 每个 Name 的结果槽
    // 1. 绑定入口参数(names 前 arity 个为参数节点,isParam)
    for i in 0..arity: values[i] = args[i]
    // 2. 拓扑序执行(Names 已排好序,参数在前,计算节点在后)
    for n in names[arity..]:
        if n.function == null: continue               // 参数/常量节点已绑定
        argv = [ resolve(n.arguments[j], values) for j in ... ]
        values[n.index] = invoke_named_function(n.function, argv, mh)
    // 3. 返回 names[result]
    return values[form.result]
```

`invoke_named_function` 分派:
- 函数为目标 DMH 的 invoke(`REF_invokeVirtual`/`invokeStatic` on DMH):走现有字段/方法 DMH 分派
  (`dispatch_method_handle_field` / 4.15b `NativeAccessor.invoke0` 语义,直接 Rust 调)。
- 函数为 `MH_cast`/`checkcast`/基本类型转换:`BasicType` 间转换 + 引用 checkcast(rustj cast 语义)。
- 函数为普通 `invokevirtual`/`invokestatic`(MemberName 指向某 Java 方法):`resolve_dispatch` + `interpret_with`。
- 函数为 `invokeBasic`(另一 MH receiver):递归 `interpret_lambda_form`(或经 BMH species getter 取 bound arg 后递归)。

### 3.3 BMH 物种实例的字段读取

BMH 的 bound 参数存于**物种类的实例字段**(`Species_L.field0`、`Species_LL.field0/field1`、…),经
`speciesData().getter(i).invokeBasic(this)` 读取(BoundMethodHandle.java:213-219)。物种类经 defineClass
装载后,其 `getter` 是普通字节码 `invokebasic`/`getfield` —— **rustj 解释器可直接读**。LF 中引用 BMH
字段处,`Name.arguments` 指向 MH receiver + bound 槽位,经 getter Name 取出。

### 3.4 钩子接线

`try_method_handle_field_hook` 扩展为 `try_method_handle_invoke_hook`:
1. method ∈ {invoke, invokeExact, invokeBasic} 且 receiver **是 DMH** → 现有字段短路(不变)。
2. receiver **非 DMH** 但 `mh.form` 可读 → `interpret_lambda_form(mh, args)`。
3. 否则 → 正常虚分派(4.15b NativeAccessor.invoke0 路径)。

`invoke` vs `invokeExact`:`invoke` 允许类型失配,会先 `asType` 调整(可能再触发 BMH——但此时 BMH 已能建,
故闭环);`invokeExact` 严格匹配(失配 → WMTE)。rustj 当前宽松,首期两者同路径,后续按需加 WMTE。

---

## 4. 分层路线图(自底向上,探针驱动)

| 层 | 内容 | 闸门 | 风险 |
|---|---|---|---|
| **G.0** | **探针**:绑 `Class.isHidden`→false、补 `Class.descriptorString` 等;载入 Class-File API;跑 `ClassSpecializer.<clinit>` + 一次 `makeReinvoker`,实证下一墙 | `lib` 探针:BMH 能成功 `makeReinvoker` 不抛 EIIE | **高**:Class-File API 可能密集踩坑 |
| **G.1** | `Lookup.defineClass` native(移植 `JVM_LookupDefineClass`);物种字节码经 `parse`+`load` 入注册表;补 G.0 暴露的其余 native | 物种类 Class 镜像可建、可 `newInstance`、可读 species 字段 | 中 |
| **G.2** | **LambdaForm 解释器核心**:`interpret_lambda_form` + `invoke_named_function` + BMH 物种字段读;接线 `try_method_handle_invoke_hook` 非 DMH 分支 | `lib`:手工构造 BMH + `invokeExact` 跑通加法/字段读 | 中(新建,但范围清晰) |
| **G.3** | **集成闸门**:`tests/reflection_field_getset.rs::field_get_set_instance_end_to_end` 去 `#[ignore]` 转绿(经真 BMH + LF,非捷径) | 实例 Field.get(7)/set(99) 经全量路径通 | 低(G.0-G.2 已铺平) |
| **G.4** | 真 Stream / 内部迭代端到端(javac 编 `Stream.of(1,2,3).map(...).filter(...).sum()`),驱动更复杂 MH 组合 | `tests/stream_pipeline.rs` 断正确和 | 中(可能暴露更多 invokedynamic 收尾) |

**节奏**:G.0 是**经验式探针**——其结果决定 G.1/G.2 的具体 native 清单与 Class-File API 负担。每层独立
brainstorm→spec→TDD→闸门→commit;G.0 完成后再细化 G.1+ 的子计划。

---

## 5. 风险与决策点

1. **Class-File API 可能踩出大量技术债**(record、switch 模式、sealed、invokedynamic 密集)。G.0 探针的
   `makeReinvoker` 是最小压力点;若 <clinit> 即崩,需评估是否把 Class-File API 的现代化构造替换为更窄的
   手植物种生成(回退方案,但偏离"全量"语义——届时与用户再确认)。
2. **LF 解释器范围**:首期支持 `asType`/pairwiseConvert 产生的 BMH(算术转换 + 单层目标调用)。完整
   `guardWithTest`/`catchException`/折叠/收集 等复杂 LF 形态可能需后续子层(随 G.4 Stream 暴露按需加)。
3. **invokeExact 类型检查**:首期宽松(不抛 WMTE),与现有签名多态钩子一致;若 G.4 Stream 需要严格 WMTE
   以匹配 JDK 行为,再加。
4. **性能**:LF 解释每 invokeExact 遍历 Name[]——无 LF 缓存/编译。Stream 热路径可能慢,但功能正确优先;
   性能优化(缓存解析图、直接调用快路径)顺延。
5. **defineClass 安全/访问域**:物种类由 `Lookup.IMPL_LOOKUP`(全权)定义;rustj 不强制访问检查,首期
   宽松装载(与现有 lenient 校验一致)。

---

## 6. 验收(Phase G 整体)

- 实例 `Field.get/set` 经真 BMH + LF 解释通(G.3,去 `#[ignore]`)。
- 真 Stream 端到端(G.4)。
- 全套测试绿、clippy 净、零 unsafe、零依赖。
- memory `hotspot-rust-migration-project.md` 候选 g 标记 ✅,跨层技术债更新。
