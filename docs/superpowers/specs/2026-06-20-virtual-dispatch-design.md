# Layer 4.2 设计:虚分派(`invokevirtual`)+ 继承字段

> 2026-06-20 · 对应 HotSpot `LinkResolver::resolve_virtual` / `InstanceKlass::find_method` /
> 非静态字段布局。承接 [4.1 对象模型](2026-06-20-object-model-design.md)。

## 1. 目标

执行使用**类继承 + 方法重写 + 继承字段**的真实 Java 程序,结果与 JVM 一致:

```java
abstract class Shape { int id; abstract int area(); }
class Square extends Shape { int side; int area() { return side * side; } }
class Rect extends Square { int h; int area() { return side * h; } }
// new Rect(); r.id = 5; r.side = 3; r.h = 4; r.area() == 12  ← invokevirtual 虚分派 + 继承字段
```

## 2. 范围

**包含(本增量):**
- `invokevirtual`(0xb6)虚分派:按对象**运行时实际类**沿超类链查找方法。
- **继承实例字段扁平化**:子类实例含整条超类链的字段;`getfield`/`putfield` 对继承字段(含
  Fieldref 指向超类)能正确定位。
- `new` 创建子类时分配扁平化布局(含继承字段,默认初始化)。

**不含(后续增量):**
- `invokeinterface`(itable / 接口分派)。
- `invokespecial` 对 `private` / `super.m()` 的完整语义(4.1 已覆盖 `<init>`;本增量不动)。
- 显式帧栈 / `StackOverflowError`(仍用 Rust 调用栈作隐式调用栈)。
- 字段访问的访问控制(protected/private)校验。

## 3. 核心不变量:字段序号对齐

> 全设计的关键洞见。

子类扁平布局 = `[超类链字段..., 本类字段...]`,即超类布局是子类布局的**前缀**。因此:

- `new Sub()` 分配 Sub 的扁平布局。
- `getfield` 解析到**声明类** C(可能为某超类),字段 f 在 **C 的扁平布局**中的下标 i,
  恰好等于 f 在实际对象(Sub 扁平布局)中的下标——因 Sub 布局 = `C 布局 + (C 与 Sub 之间及 Sub 自身字段)`,
  C 布局是 Sub 布局的前缀。

**结论**:字段序号 = 它在**声明类的扁平布局**里的下标;无需运行时类型即可与实际对象对齐。
(静态字段不扁平化:Fieldref 声明类即静态字段归属类,4.1 既有解析已正确,无需改动。)

## 4. 表示变更

### 4.1 `LoadedClass`(`oops/klass.rs`)

```
LoadedClass {
    cf: ClassFile,
    instance_fields: Vec<ResolvedField>,            // 本类声明的实例字段(声明序)— 不变,语义为"本类"
    static_fields: Vec<ResolvedField>,
    static_storage: RefCell<Vec<Slot>>,             // 4.1 既有
    super_class_name: Option<String>,               // 新增:None = Object/无超类
    flat_cache: RefCell<Option<Vec<ResolvedField>>>, // 新增:扁平实例布局惰性缓存
}
```

- `super_class_name`:取自 `cf.super_class_name()`(`None` 表示 `java/lang/Object` 或无超类)。
- `flat_cache`:**惰性**计算,首次访问时沿注册表走超类链填入,之后复用。解耦加载顺序
  (`compile_and_load_all` 按目录序加载,不确定)。

`from_cf` 增填 `super_class_name`;`flat_cache` 初值 `RefCell::new(None)`。

### 4.2 `ClassRegistry` 新增方法

```rust
/// 扁平化实例字段(超类链 ++ 本类),惰性缓存。
fn flattened_instance_fields(&self, lc: &LoadedClass) -> Vec<ResolvedField>
/// 按名 + 类型在 lc 的扁平布局中定位实例字段 → 全局序号。
fn instance_field(&self, lc: &LoadedClass, name, ft) -> Option<usize>
/// 创建默认初始化实例(扁平布局全零/null)。
fn new_instance(&self, lc: &LoadedClass) -> InstanceOop
/// 虚分派查找:从 class_name 沿超类链找首个 (name, desc) 方法 → (声明类, 方法)。
fn find_virtual_method(&self, class_name, name, desc) -> Option<(&LoadedClass, &MethodInfo)>
```

**扁平化算法**(`flattened_instance_fields`):
1. 命中缓存即返回。
2. 若 `super_class_name` 存在、非 `java/lang/Object`、且在注册表中 → 递归取超类扁平布局,**置前**。
3. 追加本类 `instance_fields`。
4. 写入缓存,返回。

(单继承无环;`java/lang/Object` 未加载,作根处理。)

**虚分派查找**(`find_virtual_method`):从 `class_name` 起,逐类 `cf.methods` 比对 (name, desc);
未命中则走 `super_class_name`,直至 Object 或未加载。

## 5. 指令语义

### `invokevirtual`(0xb6)
1. 解析 Methodref → `(declared_class, name, desc)`。`declared_class` 仅用于校验/报错,**不参与分派**。
2. 按描述符逆序弹 args,再弹 objref;objref null → `VmError::NullPointer`。
3. 取运行时类:`heap.get(objref)` → `InstanceOop.class_name()` → owned `String`(不持借用)。
4. `find_virtual_method(runtime_class, name, desc)` → `(target_lc, method)`;未找到 → `BadConstant`。
5. 取 `method.code`;构造被调用者帧:`local[0] = objref`,args 其后(沿用 `Arg`/`store_arg`)。
6. 递归 `interpret_with`(目标类常量池)→ 按返回类型回填。
7. `pc += 3`。

### `new`(0xbb,改动)
`field::new_instance` 改为 `registry.new_instance(lc)`(扁平布局,含继承字段)。

### `getfield`/`putfield`(0xb4/0xb5,改动)
字段序号改为 `registry.instance_field(declaring_lc, name, ft)`(扁平布局查找)。
其余(null 检查、cat-2 类型转换、静态存储路径)不变。

## 6. 借用要点

沿用 4.1 的 `'a` 模式:`Vm::registry()` 返回 `Option<&'a ClassRegistry>`,与 `&self` 借用解耦。
`invokevirtual` 取运行时类时,`heap.get()` 的不可变借用取出 `class_name`(owned `String`)即释放,
随后 `&mut vm` 递归无冲突。

## 7. 模块布局

- `oops/klass.rs`:`LoadedClass` 增 `super_class_name` / `flat_cache`;`ClassRegistry` 增
  `flattened_instance_fields` / `instance_field` / `new_instance` / `find_virtual_method`。
- `runtime/interpreter/invoke.rs`:新增 `invoke_virtual`。
- `runtime/interpreter/mod.rs`:新增 `Opcode::Invokevirtual` 分派臂。
- `tests/virtual_dispatch.rs`(新):javac 编译继承层次,真实执行虚分派 + 继承字段。

## 8. 测试策略

**单元**(klass.rs):
- `flattened_instance_fields_prepends_super`(手构或真实类:Sub 扁平 = Super 字段 + Sub 字段)。
- `find_virtual_method_walks_to_super`(重写/未重写两种情形)。
- 缓存命中二次返回同值。

**集成**(执行闸门):javac 编译 `Shape`/`Square`/`Rect`(或 `Animal`/`Dog`/`Puppy`)层次,
真实执行:
- `invokevirtual` 命中子类重写方法(多态)。
- `invokevirtual` 命中超类未重写方法(继承)。
- `getfield`/`putfield` 继承字段(跨层)。
- `new` 子类 + 默认初始化继承字段为零。
- 对 null 引用 `invokevirtual` → `NullPointer`。
结果与 JVM 一致。

## 9. HotSpot 对照

| rustj(4.2) | HotSpot |
|---|---|
| `flattened_instance_fields` 惰性扁平 | `InstanceKlass::nonstatic_field_size` + 布局计算(解析期) |
| `find_virtual_method` 线性上行查找 | `InstanceKlass::vtable` + `InstanceKlass::find_method`(我们用线性查找,不用 vtable——更简单,优化留待 JIT 层) |
| 运行时类 = `InstanceOop.class_name` | `oop->klass()` |
| `invokevirtual` 无 itable | `invokeinterface` 才走 itable(本增量不做) |

## 10. 构建序(TDD)

1. klass.rs:`LoadedClass` 加字段 + `ClassRegistry` 加四方法。单元测试先行(红→绿)。
2. invoke.rs:`invoke_virtual`。mod.rs 加分派臂。
3. `field.rs`:`new`/`getfield`/`putfield` 改走扁平布局。
4. `tests/virtual_dispatch.rs` 集成闸门。
5. clippy `--all-targets`、零 unsafe、全测试绿;提交。
