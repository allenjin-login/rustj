# Layer 4.9:`<clinit>` 类初始化 — 设计

> 状态:spec · 对应北极星路线图步骤 2。前置:4.8(字符串池)。
> 目标:首次 **active use** 时执行类的静态初始化器 `<clinit>`,使带静态初始化逻辑的
> 真实 Java 类(如 `Object`/`Class`/`System` 的 `registerNatives`)可被正确加载运行。

## 1. 问题

当前静态字段在**加载期默认零初始化**(`klass::resolve_fields`),`<clinit>` **从不执行**。
故 `static int v = 42;`、`static { ... }`、超类静态初始化器等一律被忽略——`getstatic` 永远
读到默认值。这是加载真实 `java.base` 的硬阻塞(几乎每个核心类都有 `<clinit>`)。

## 2. 范围(JVMS §5.5 子集)

**做:**
- active use 触发 `ensure_class_initialized`:`new`(实例创建,非数组)、`invokestatic`、
  `getstatic`、`putstatic` 四类目标类的首次使用。
- 状态机 `NotStarted → InProgress → Done`(重入安全);失败 → `Failed`。
- **超类先于本类**初始化(沿超类链递归,`Object`/无超类终止)。
- `<clinit>` 经既有 `interpret_with` 执行(`run_with_depth`),写静态字段走既有 `putstatic`/
  `static_storage`(零新机制)。
- `<clinit>` 抛异常 → `java/lang/ExceptionInInitializerError`(状态置 `Failed`);
  此后再次 active use → `java/lang/NoClassDefFoundError`。

**顺延(明确不做,记入路线图):**
- `invokevirtual`/`invokespecial`/`invokeinterface` 对**声明类**的触发——现实中由 `new`
  已先行触发(实例方法必先 `new`),完整"解析类即触发"留待类链接层。
- 接口 `<clinit>` / 接口 default 方法触发的超接口初始化(真实 `java.base` 接口较少依赖)。
- `ExceptionInInitializerError` 的 **cause 链**(包装原异常)——须先有 `Throwable.cause`
  字段,记为"异常 cause 链"层;本层抛无 cause 的 EIIE(顺延项)。
- `ConstantValue` 属性(final 常量字段折叠)——独立顺延项。

## 3. 表示选择

### 3.1 初始化状态:加在 `LoadedClass` 上(类比 `static_storage`)

`LoadedClass` 增 `init_state: RefCell<InitState>`。理由:
- 类初始化是**类级可变状态**,与 `static_storage` 同性;`Vm` 以不可变借用持注册表,
  只能经 `RefCell` 内部可变性改状态——**沿用既有定式,零新模式**。
- 放注册表而非 `Vm`:状态属于**类**(加载期决定),与哪个 `Vm` 执行无关;多 `Vm` 共享
  同一注册表时(如闸门复用)状态应一致。

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitState { NotStarted, InProgress, Done, Failed }
```

`LoadedClass::from_cf` 初始化 `NotStarted`;加 `init_state()` / `set_init_state()` 访问器。

### 3.2 `ensure_class_initialized` 放 `interpreter/clinit.rs`

它需要**同时**持有 `&LoadedClass`(读 `<clinit>` 字节码/常量池/超类名/状态)与 `&mut Vm`
(执行 `<clinit>` 经 `interpret_with`)。沿用 `invoke_static`/`throw_exception` 的 `'a` 借用
技巧:[`Vm::registry`] 返回 `&'a ClassRegistry`(寿命不绑 `&self`),故取出 `&'a LoadedClass`
后仍可 `&mut vm`。registry(不可变状态)不持 Vm,故此归属正确。

## 4. 数据流(单类 `C` 首次 `getstatic C.f`)

```
dispatch Getstatic
  → field::get_static
    → clinit::ensure_class_initialized(vm, "C")?
        registry.get("C") → lc          // &'a LoadedClass
        match lc.init_state():
          Done | InProgress → return Ok   // 幂等 / 重入跳过
          Failed → throw NoClassDefFoundError
          NotStarted → 继续
        lc.set_init_state(InProgress)     // 防重入:置中先于超类/本类
        if let Some(s) = lc.super_class_name() {
            ensure_class_initialized(vm, s)?;   // 超类先行(递归)
        }
        run_clinit(lc, vm)?                // 有 <clinit> 则 interpret_with 执行
          └─ <clinit> 内 putstatic C.f=42 → 写 lc.static_storage(既有机制)
        lc.set_init_state(Done)
    → 读 lc.static_storage[f] → 42(已被 <clinit> 写入)
```

重入:`C.<clinit>` 执行中若再 `getstatic C.g` → `ensure` 见 `InProgress` → 直接返回(不重跑)。
单线程,无需 HotSpot 的锁/`thread` 通知。

## 5. 错误处理

`run_clinit` 返回 `Result<(), VmError>`:
- 无 `<clinit>` → `Ok`(默认初始化已在加载期完成)。
- `<clinit>` 正常返回 → `Ok`。
- `Err(ThrownException(cause))`(本类 `<clinit>` 抛出,或超类初始化失败已上传):
  - 置 `Failed`。
  - 若 cause 已是 `ExceptionInInitializerError` / `NoClassDefFoundError`(超类失败上传)→
    **原样上传**(不重复包装)。
  - 否则(本类 `<clinit>` 直接抛的业务异常)→ **包成** `ExceptionInInitializerError` 上传。
- `Err(其他内部故障)` → 置 `Failed`,原样上传(不可捕获性质)。

判定"已是初始化失败类异常"经堆读对象类名(`heap().get(cause)` → `class_name()`)。

## 6. 触发点(4 处,函数内首步)

| 指令 | 函数 | 触发对象 |
|---|---|---|
| `new` | `field::new_instance` | 解析出的目标类(分配前) |
| `invokestatic` | `invoke::invoke_static` | Methodref 声明类 |
| `getstatic` | `field::get_static` | Fieldref 声明类 |
| `putstatic` | `field::put_static` | Fieldref 声明类 |

各函数解析出 `class_name` 后、取 `lc`/读存储前,插一行 `clinit::ensure_class_initialized(vm, &class_name)?`。
未加载类 / 无注册表 → `ensure` 内 `Ok` 跳过(让既有"未加载"错误照常上报)。

### 6.1 附带修复:`getstatic`/`putstatic` 继承静态字段解析

超类先行闸门暴露出既有缺口:Fieldref 的类对**继承静态字段**指向子类(javac 编码),而
`LoadedClass::static_field` 仅查本类。故新增 `ClassRegistry::resolve_static_field` ——沿超类链
找**(声明类, 序号)**,定位真正持有 `static_storage` 的类。`get_static`/`put_static` 改用之。
初始化触发仍对 `class_name`(子类):`ensure` 会先初始化超类链,故声明类 `<clinit>` 先行,
继承字段值已就绪。接口静态字段不沿此路径(顺延)。

## 7. 引导桩补充(`bootstrap::BOOTSTRAP_HIERARCHY`)

新增三行(单一真相源,追加即可):
```
java/lang/LinkageError                  → Error
java/lang/ExceptionInInitializerError   → LinkageError
java/lang/NoClassDefFoundError          → LinkageError
```
桩带空 `<init>`、无 `<clinit>` → `ensure` 对桩 = 一次空跑置 `Done`,无害。

## 8. 测试

**单元(`clinit.rs` test,无需 javac):**
- 合成 `Cls`(静态 `int v`、`<clinit>`=`iconst_5;putstatic;return`)→ `ensure` 后
  `static_storage[0] == Int(5)`(证明 `<clinit>` 真执行并写静态)。

**集成闸门(`tests/clinit.rs`,javac,缺则跳过):**
1. `static int v = 42` → `getV()==42`(非 final 强制 `<clinit>`)。
2. `static { derived = base + 5 }` → `getDerived()==15`(static 块 + 字段间引用)。
3. `<clinit>` 计数器:多次调用后仍 `==1`(只跑一次)。
4. 超类先行:`Sub extends Base`,`Base.b=100`,`Sub.s=b+1` → `getS()==101`。
5. `<clinit>` 抛异常 → `ExceptionInInitializerError`。
6. 失败后再访问 → `NoClassDefFoundError`(验证 `Failed` 状态)。

## 9. 债务影响

- 解锁:真实类的静态初始化(北极星步骤 2 关键缺口)。
- 净增技术债:0 新存根(EIIE/NCDFO 是标准层次,非一次性桩);cause 链为已知顺延项。
- 不触动:静态字段机制、invoke/字段路径结构、异常模型(沿用 `ThrownException`)。
