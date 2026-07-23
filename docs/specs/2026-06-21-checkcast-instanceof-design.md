# Layer 4.6 `checkcast` / `instanceof` 设计

> 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_checkcast)` / `CASE(_instanceof)`。
> 配合 4.5 的引用返回,补齐**引用转型与类型判定**——`(Foo) obj`、`obj instanceof Bar`。
> 核心是**子类型判定**:`ClassRegistry::is_instance(class_name, target)`,基于 4.2/4.2b 已
> 就绪的超类链与接口闭包。

## 1. 目标

让 rustj 执行真实 Java 的强制转型与 `instanceof` 判定,与 JVM 一致:

```java
Object o = ...;
if (o instanceof String) { ... }   // instanceof
Foo f = (Foo) o;                    // checkcast(不匹配 → ClassCastException)
```

零 unsafe。

## 2. 子类型判定 `is_instance`

`ClassRegistry::is_instance(class_name, target) -> bool`:`class_name`(对象运行时类)是否
为 `target` 的实例(子类型)。

**关键:类目标与接口目标统一处理。** 遍历 `class_name` 的超类链,收集每类及传递接口闭包,
判 `target` 是否在集合内——无需区分 `target` 是类还是接口。

```rust
/// `class_name` 的所有超类型名集合:自身 + 超类链 + 各类的传递接口闭包。
fn supertypes_of(&self, class_name: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    // 超类链:每类入集合,其直接接口入队。
    let mut cur = self.get(class_name);
    while let Some(lc) = cur {
        set.insert(lc.name().to_string());
        for iface in lc.interface_names() {
            if !set.contains(&iface) { queue.push_back(iface); }
        }
        cur = lc.super_class_name().and_then(|s| self.get(s));
    }
    // BFS 接口闭包:接口的超接口递归入集合。
    while let Some(name) = queue.pop_front() {
        if !set.insert(name.clone()) { continue; }
        if let Some(ilc) = self.get(&name) {
            for si in ilc.interface_names() {
                if !set.contains(&si) { queue.push_back(si); }
            }
        }
    }
    set
}

/// `class_name` 是否 `target` 的实例。数组对象仅匹配 Object;数组目标一律不匹配类对象。
pub fn is_instance(&self, class_name: &str, target: &str) -> bool {
    if class_name.starts_with('[') {
        return target == "java/lang/Object"; // 数组仅 Object(顺延 Cloneable/Serializable)
    }
    if target == "java/lang/Object" {
        return true; // 任何类都是 Object 实例
    }
    if target.starts_with('[') {
        return false; // 非数组对象非数组(数组目标顺延)
    }
    self.supertypes_of(class_name).contains(target)
}
```

> `supertypes_of` 形同 4.2b `find_default_method` 的闭包 BFS,但收集名而非找方法。
> `Object` 特判:`target == "java/lang/Object"` 恒真(避免依赖 Object 是否加载)。

## 3. `checkcast`(0xc0)

格式:`opcode(1) + index(2)`,`pc += 3`。栈:`..., objectref → ..., objectref`(**保留** objectref)。

- objectref 为 null → 保留 null,**不报错**(null 可转型任意引用类型)。
- objectref 非 null:
  - 实例对象:`is_instance(运行时类, target)` 真 → 保留;假 → `ClassCastException`。
  - 数组对象:`target == "java/lang/Object"` → 保留;否则 → `ClassCastException`
    (数组非任意类/接口实例;数组目标/协变顺延)。

## 4. `instanceof`(0xc1)

格式:`opcode(1) + index(2)`,`pc += 3`。栈:`..., objectref → ..., result`(弹 objectref,压 int)。

- objectref 为 null → 压 `0`。
- objectref 非 null:
  - 实例对象:压 `is_instance(...) ? 1 : 0`。
  - 数组对象:`target == "java/lang/Object"` → `1`,否则 `0`。

> `instanceof` 不抛异常(不匹配返回 0),区别于 `checkcast`。

## 5. 错误模型

新增 `VmError::ClassCastException`(checkcast 不匹配时)。其余(null 不报错、instanceof 返 0)
无新错误。

## 6. 实现:对象运行时类获取

```rust
// 弹 objectref,经堆取 Oop,提取(是否数组, 类名)。own 类名避免借用纠缠。
let (is_array, class_name): (bool, Option<String>) = {
    let obj = vm.heap().get(objref).ok_or(VmError::BadConstant("checkcast/instanceof 引用悬空"))?;
    match obj {
        Oop::Instance(i) => (false, Some(i.class_name().to_string())),
        Oop::Array(_) => (true, None),
    }
};
let target = resolve_class_name(self.cp(), index)?;
let hit = if is_array {
    target == "java/lang/Object"
} else {
    let reg = vm.registry().ok_or(VmError::BadConstant("checkcast/instanceof 需类注册表"))?;
    reg.is_instance(class_name.as_deref().unwrap(), &target)
};
```

`resolve_class_name` 复用 4.1 的 CP Class→Utf8 解析。`InstanceOop::class_name()` 给运行时类。
两处共享此逻辑——置 `interpreter/` 子模块或 mod.rs 私有函数;本层小,放 mod.rs 私有辅助
`check_cast` / `instance_of`(形同 array.rs 的入口)。

## 7. 分派臂

```rust
Opcode::Checkcast => { field_or_local::check_cast(self, frame, vm, self.read_u2(pc + 1)?)?; pc += 3; }
Opcode::Instanceof => { ...::instance_of(self, frame, vm, self.read_u2(pc + 1)?)?; pc += 3; }
```

置 `interpreter/type_check.rs` 子模块(`pub(super) check_cast`/`instance_of`),与
`array.rs`/`field.rs`/`invoke.rs` 同级,职责单一(转型与类型判定)。

## 8. 顺延项

- **数组目标 / 协变**:checkcast/instanceof 对 `[Ljava/lang/String;` 等数组类型(`obj instanceof int[]`)。
  `ArrayOop` 不记组件类型(4.3a 决策),无法判定;需先给 `ArrayOop` 加组件类型(独立改动)。
- Cloneable/Serializable:数组实现的接口(罕见,YAGNI)。
- `athrow` + 异常表(ClassCastException 的捕获需异常处理层)。

## 9. 测试策略

TDD 红绿 + 真实字节码闸门:

1. **单元**(`klass.rs`):用 Shape←Square←Rect + 接口 Drawable/Resizable 层次,测
   `is_instance`:Square instanceof Square/Shape/Object(真)、Rect(假);Square instanceof
   Drawable(真,经接口);`[I` instanceof Object(真)/String(假)。
2. **单元**(`mod.rs` 或 `type_check.rs`):构造字节码 `aload; instanceof/ checkcast`,断言
   压栈结果/ClassCastException。需注册表 + 实例引用(经 `new` 或预设 local)。
3. **集成闸门**(`tests/checkcast.rs`):`javac` 编 `instanceof` 各情形(类/接口/null/不匹配)、
   `checkcast` 通过与失败(失败断言 `ClassCastException`)。无 javac 则跳过。

每任务先红(看失败原因正确)后绿,频繁提交。

## 10. 自检

- 范围:`checkcast`/`instanceof` + `is_instance` + `ClassCastException`;数组目标/协变明确顺延。
- 一致性:`supertypes_of` 复用 4.2b 闭包 BFS 形;类/接口目标统一(超类链 ∪ 接口闭包)。
- null 语义:checkcast null 不报错(保留),instanceof null → 0。
- 最小性:不加组件类型跟踪(顺延);不加异常表(ClassCastException 暂作 `VmError` 抛出,捕获顺延)。
