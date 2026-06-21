# Layer 4.6 `checkcast` / `instanceof` 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement
> this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 `checkcast`/`instanceof`,让 rustj 执行引用转型与 `instanceof` 判定。

**Architecture:** `ClassRegistry::is_instance`(超类链 ∪ 接口闭包含 target)做子类型判定;
新 `VmError::ClassCastException`;`interpreter/type_check.rs` 子模块封装 `check_cast`/
`instance_of` 入口(checkcast 保留 objectref,instanceof 弹 objectref 压 int)。

**依据:** `docs/superpowers/specs/2026-06-21-checkcast-instanceof-design.md`。
节奏:写失败测试 → 看红 → 最小实现 → 看绿 → 提交。命令在 `E:\rustj`。

---

### Task 1: 子类型判定 `is_instance`

**Files:** Modify `src/oops/klass.rs`(方法 + 单元测试)

- [ ] **Step 1: 写失败测试**(在 `klass.rs` tests 末尾追加)

```rust
    fn checkcast_hierarchy() -> ClassRegistry {
        // utf8: 1=Object 2=Shape 3=Square 4=Rect 5=Drawable
        // class: 6=Object 7=Shape 8=Square 9=Rect 10=Drawable
        let cp = mk_cp(
            &["java/lang/Object", "Shape", "Square", "Rect", "Drawable"],
            &[1, 2, 3, 4, 5],
        );
        let mut reg = ClassRegistry::new();
        let load = |reg: &mut ClassRegistry, cf| { reg.load(cf).unwrap(); };
        // Shape extends Object
        load(&mut reg, mk_cf(cp.clone(), 7, 6, vec![], vec![]));
        // Square extends Shape, implements Drawable
        load(&mut reg, mk_cf(cp.clone(), 8, 7, vec![10], vec![]));
        // Rect extends Shape
        load(&mut reg, mk_cf(cp.clone(), 9, 7, vec![], vec![]));
        // Drawable (interface) extends Object
        load(&mut reg, mk_cf(cp.clone(), 10, 6, vec![], vec![]));
        reg
    }

    #[test]
    fn is_instance_class_and_super() {
        let reg = checkcast_hierarchy();
        assert!(reg.is_instance("Square", "Square"));
        assert!(reg.is_instance("Square", "Shape"));
        assert!(reg.is_instance("Square", "java/lang/Object"));
        assert!(!reg.is_instance("Square", "Rect"));
    }

    #[test]
    fn is_instance_interface() {
        let reg = checkcast_hierarchy();
        assert!(reg.is_instance("Square", "Drawable"));
        assert!(!reg.is_instance("Rect", "Drawable"));
    }

    #[test]
    fn is_instance_array_only_object() {
        let reg = checkcast_hierarchy();
        assert!(reg.is_instance("[I", "java/lang/Object"));
        assert!(!reg.is_instance("[I", "Shape"));
    }
```

> `mk_cp`/`mk_cf` 已存在(行 423/439)。`ClassFile` 可能需 `Clone`——若 `cp.clone()` 不行,
> 改为每次 `mk_cp` 重建(见 Step 3 备注)。

- [ ] **Step 2: 看红**

Run: `cargo test --lib -- is_instance_class_and_super is_instance_interface is_instance_array_only_object`
Expected: 编译错误(`is_instance` 未定义)。

- [ ] **Step 3: 实现**(在 `find_default_method` 之后插入;顶部 `use std::collections::{VecDeque, HashSet}` 已有)

```rust
    /// `class_name` 的所有超类型名集合:自身 + 超类链 + 各类的传递接口闭包。
    fn supertypes_of(&self, class_name: &str) -> HashSet<String> {
        let mut set = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        let mut cur = self.get(class_name);
        while let Some(lc) = cur {
            set.insert(lc.name().to_string());
            for iface in lc.interface_names() {
                if !set.contains(&iface) {
                    queue.push_back(iface);
                }
            }
            cur = lc.super_class_name().and_then(|s| self.get(s));
        }
        while let Some(name) = queue.pop_front() {
            if !set.insert(name.clone()) {
                continue;
            }
            if let Some(ilc) = self.get(&name) {
                for si in ilc.interface_names() {
                    if !set.contains(&si) {
                        queue.push_back(si);
                    }
                }
            }
        }
        set
    }

    /// `class_name` 是否 `target` 的实例(子类型)。数组对象仅匹配 Object;
    /// 数组目标不匹配类对象(数组目标/协变顺延)。
    pub fn is_instance(&self, class_name: &str, target: &str) -> bool {
        if class_name.starts_with('[') {
            return target == "java/lang/Object";
        }
        if target == "java/lang/Object" {
            return true;
        }
        if target.starts_with('[') {
            return false;
        }
        self.supertypes_of(class_name).contains(target)
    }
```

> 若 `mk_cf(cp.clone(), ...)` 因 `ConstantPool` 非 `Clone` 失败,改测试为每次
> `mk_cp(&[...], &[1,2,3,4,5])` 重建(cp 是测试夹具,可重复构造)。

- [ ] **Step 4: 看绿**

Run: `cargo test --lib -- is_instance_class_and_super is_instance_interface is_instance_array_only_object`
Expected: 3 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/oops/klass.rs
git commit -m "feat(oops): ClassRegistry::is_instance 子类型判定"
```

---

### Task 2: `checkcast` / `instanceof` 分派

**Files:** Modify `mod.rs`(ClassCastException + 子模块声明 + 分派臂 + 测试)、
Create `src/runtime/interpreter/type_check.rs`

- [ ] **Step 1: 加 `ClassCastException` 变体**

`mod.rs` 的 `VmError` 枚举(`AbstractMethodError` 旁)加:

```rust
    /// ClassCastException:checkcast 不匹配。
    ClassCastException,
```

并在 `impl Display for VmError` 加臂:

```rust
    VmError::ClassCastException => write!(f, "ClassCastException"),
```

- [ ] **Step 2: 写 `type_check.rs` 子模块**(整文件)

```rust
//! 类型检查:`checkcast` / `instanceof`。
//!
//! 对应 HotSpot `bytecodeInterpreter.cpp` 的 `CASE(_checkcast)` / `CASE(_instanceof)`。
//! 子类型判定经 `ClassRegistry::is_instance`(超类链 ∪ 接口闭包)。

use super::field::resolve_class_name;
use super::{Interpreter, VmError};
use crate::oops::Oop;
use crate::runtime::{Frame, Vm};

/// 取栈顶 objectref 的(是否数组, 运行时类名;own 字符串避免借用纠缠)。
/// objectref 调用方已保证非 null(由 check_cast/instance_of 的 null 分支处理)。
fn object_type(
    vm: &Vm<'_>,
    objref: crate::runtime::Reference,
) -> Result<(bool, Option<String>), VmError> {
    let obj = vm
        .heap()
        .get(objref)
        .ok_or(VmError::BadConstant("checkcast/instanceof 引用悬空"))?;
    Ok(match obj {
        Oop::Instance(i) => (false, Some(i.class_name().to_string())),
        Oop::Array(_) => (true, None),
    })
}

/// 命中判定:objectref(非 null)是否 target 实例。数组仅 Object 命中。
fn matches(
    interp: &Interpreter<'_>,
    vm: &Vm<'_>,
    objref: crate::runtime::Reference,
    index: u16,
) -> Result<bool, VmError> {
    let target = resolve_class_name(interp.cp(), index)?;
    let (is_array, class_name) = object_type(vm, objref)?;
    Ok(if is_array {
        target == "java/lang/Object"
    } else {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("checkcast/instanceof 需类注册表"))?;
        reg.is_instance(class_name.as_deref().unwrap(), &target)
    })
}

/// `checkcast`:弹 objectref,判定,保留 objectref;不匹配 → ClassCastException。null 保留。
pub(super) fn check_cast(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    index: u16,
) -> Result<(), VmError> {
    let objref = frame.operands.pop_reference()?;
    let ok = if objref.is_null() {
        true
    } else {
        matches(interp, vm, objref, index)?
    };
    frame.operands.push_reference(objref)?;
    if ok {
        Ok(())
    } else {
        Err(VmError::ClassCastException)
    }
}

/// `instanceof`:弹 objectref,压 1/0。null → 0。
pub(super) fn instance_of(
    interp: &Interpreter<'_>,
    frame: &mut Frame,
    vm: &mut Vm<'_>,
    index: u16,
) -> Result<(), VmError> {
    let objref = frame.operands.pop_reference()?;
    let result = if objref.is_null() {
        0
    } else {
        i32::from(matches(interp, vm, objref, index)?)
    };
    frame.operands.push_int(result)?;
    Ok(())
}
```

- [ ] **Step 3: 声明子模块 + 分派臂**(`mod.rs`)

顶部 `mod array; mod field; mod invoke;` 后加:

```rust
mod type_check;
```

分派循环(`Multianewarray` 臂之后)加:

```rust
                Opcode::Checkcast => {
                    let index = self.read_u2(pc + 1)?;
                    type_check::check_cast(self, frame, vm, index)?;
                    pc += 3;
                }
                Opcode::Instanceof => {
                    let index = self.read_u2(pc + 1)?;
                    type_check::instance_of(self, frame, vm, index)?;
                    pc += 3;
                }
```

- [ ] **Step 4: 写失败测试**(追加到 `mod.rs` tests;复用 klass 层次思路,但此处用字节码端到端)

```rust
    // ===== Layer 4.6:checkcast / instanceof =====

    /// 构 Shape←Square(impl Drawable) 注册表 + Square 实例引用 + 含 target Class 的 cp。
    /// cp:#1=Utf8 "java/lang/Object" #2="Shape" #3="Square" #4="Drawable"
    ///    #5=Class(2)=Shape #6=Class(3)=Square #7=Class(4)=Drawable
    fn type_check_setup() -> (crate::oops::ClassRegistry, ConstantPool, crate::runtime::Reference) {
        use crate::classfile::Reader;
        use crate::metadata::{AccessFlags, ClassFile};
        // 用 klass 测试的 mk_cp/mk_cf 不在此模块;此处内联最小 CP/CF 构造。
        let cp_bytes = {
            let mut b = vec![0x00, 0x07]; // count=7
            for s in ["java/lang/Object", "Shape", "Square", "Drawable"] {
                b.push(0x01);
                b.extend_from_slice(&(s.len() as u16).to_be_bytes());
                b.extend_from_slice(s.as_bytes());
            }
            // #5..#7: Class entries pointing at #2,#3,#4
            for idx in [2u16, 3, 4] {
                b.push(0x07);
                b.extend_from_slice(&idx.to_be_bytes());
            }
            b
        };
        let mk_cp = || ConstantPool::parse(&mut Reader::new(&cp_bytes)).unwrap();
        let mk_cf = |this: u16, super_c: u16, ifaces: Vec<u16>| ClassFile {
            minor_version: 0, major_version: 52,
            constant_pool: mk_cp(), access_flags: AccessFlags::from_bits(0).unwrap(),
            this_class: this, super_class: super_c, interfaces: ifaces,
            fields: Vec::new(), methods: Vec::new(), attributes: Vec::new(),
        };
        let mut reg = crate::oops::ClassRegistry::new();
        reg.load(mk_cf(6, 5, vec![])).unwrap();       // Square (#6) extends Shape (#5)
        reg.load(mk_cf(5, 1, vec![])).unwrap();       // Shape (#5) extends Object utf8 #1? — 见下
        reg.load(mk_cf(7, 5, vec![])).unwrap();       // Drawable (#7)
        // 注:super_class 必须是 Class 索引;Object 无 Class 条目 → Shape 的 super 设为 0(无)?
        // 修正:Shape super 用一个指向 Object 的 Class。补 #8?为简化,Shape 不设超类(super=0)
        // → LoadedClass.super_class_name()=None。is_instance("Square","Object") 走 Object 特判。
        let _ = (); // 见实际实现;此处以编译通过为准,运行断言在具体测试。
        let cp = mk_cp();
        let mut vm = crate::runtime::Vm::new(&reg);
        let square_lc = reg.get("Square").unwrap();
        let inst = vm
            .heap_mut()
            .alloc(crate::oops::Oop::Instance(reg.new_instance(square_lc)));
        // 实例引用在 vm 内;但 vm 会随返回 drop。改为返回 reg/cp 并在测试内重建 vm。
        (reg, cp, inst)
    }
```

> **注意:** 上述 `type_check_setup` 因 `vm` 生命周期问题无法直接返回实例引用(`Vm` 持
> `&registry`)。**改用更简策略**——不在 `mod.rs` 单测 checkcast/instanceof 的字节码端到端
> (它强依赖注册表+堆+实例),而把行为验证放在 `tests/checkcast.rs`(javac 闸门,Task 3)。
> 本步只验证**编译通过**(分派臂接好、type_check 模块无误),并删去 `type_check_setup` 占位。
> 单元的"子类型判定"已在 Task 1(klass.rs)覆盖;字节码端到端由集成闸门覆盖。

删去 `type_check_setup`,仅保留注释说明覆盖分布。

- [ ] **Step 5: 看绿**

Run: `cargo test --lib`
Expected: 全绿(无新单测;编译通过即分派接好)。

Run: `cargo clippy --all-targets -- -D warnings`
Expected: 零告警。

- [ ] **Step 6: 提交**

```bash
git add src/runtime/interpreter/mod.rs src/runtime/interpreter/type_check.rs
git commit -m "feat(interp): checkcast/instanceof + ClassCastException"
```

---

### Task 3: javac 集成闸门

**Files:** Create `tests/checkcast.rs`

- [ ] **Step 1: 写测试**(复用 areturn.rs 骨架;整文件)

```rust
//! 集成闸门(Layer 4.6):javac 编 instanceof / 强制转型的真实 Java,由 rustj 执行,
//! 验证 checkcast/instanceof 与 JVM 一致。需 `javac`(无则跳过)。

use std::process::Command;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;
use rustj::metadata::{ClassFile, MethodInfo};
use rustj::oops::ClassRegistry;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn compile_and_load(source: &str, public_name: &str) -> ClassRegistry {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-cc-{}-{s}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac").arg("-d").arg(&dir).arg(&src).output().expect("javac 执行失败");
    assert!(out.status.success(), "javac 编译失败:\n{}", String::from_utf8_lossy(&out.stderr));
    let mut reg = ClassRegistry::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&std::fs::read(&p).unwrap()).expect("解析应成功")).expect("加载应成功");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    reg
}

fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods.iter().find(|m| {
        let n = match cf.constant_pool.get(m.name_index).unwrap() {
            ConstantPoolEntry::Utf8(s) => s == name, _ => false,
        };
        let d = match cf.constant_pool.get(m.descriptor_index).unwrap() {
            ConstantPoolEntry::Utf8(s) => s == desc, _ => false,
        };
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}

fn run(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> Value {
    let lc = reg.get(class_name).unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(reg);
    interp.interpret_with(&mut frame, &mut vm).unwrap_or_else(|e| panic!("{name}{desc} 失败:{e}"))
}

fn run_err(reg: &ClassRegistry, class_name: &str, name: &str, desc: &str) -> VmError {
    let lc = reg.get(class_name).unwrap_or_else(|| panic!("类 {class_name} 未加载"));
    let m = find_method(&lc.cf, name, desc);
    let code = m.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool);
    let mut vm = Vm::new(reg);
    interp.interpret_with(&mut frame, &mut vm).expect_err("期望失败")
}

const SOURCE: &str = r#"
public class CheckCast {
    static class Shape {}
    static class Square extends Shape {}
    static interface Drawable { }
    static class Circle extends Shape implements Drawable {}

    // instanceof 类:true
    public static boolean squareIsShape() {
        Object o = new Square();
        return o instanceof Shape;
    }
    // instanceof 接口:true(Circle implements Drawable)
    public static boolean circleIsDrawable() {
        Object o = new Circle();
        return o instanceof Drawable;
    }
    // instanceof 不匹配:false
    public static boolean squareIsCircle() {
        Object o = new Square();
        return o instanceof Circle;
    }
    // instanceof null:false
    public static boolean nullIsShape() {
        Object o = null;
        return o instanceof Shape;
    }
    // checkcast 通过:返回字段无关,转型成功即返回 1
    public static int castOk() {
        Object o = new Square();
        Square s = (Square) o;
        return 1;
    }
    // checkcast 失败:ClassCastException
    public static int castFail() {
        Object o = new Square();
        Circle c = (Circle) o;  // Square 不能转 Circle
        return 1;
    }
}
"#;

fn bool_to_int(v: Value) -> i32 {
    match v { Value::Int(b) => b, other => panic!("期望 int,得 {other:?}") }
}

#[test]
fn instanceof_class_match() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "squareIsShape", "()Z")), 1);
}

#[test]
fn instanceof_interface_match() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "circleIsDrawable", "()Z")), 1);
}

#[test]
fn instanceof_no_match() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "squareIsCircle", "()Z")), 0);
}

#[test]
fn instanceof_null_is_zero() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "nullIsShape", "()Z")), 0);
}

#[test]
fn checkcast_passes() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(bool_to_int(run(&reg, "CheckCast", "castOk", "()I")), 1);
}

#[test]
fn checkcast_fails_with_classcastexception() {
    if !javac_available() { eprintln!("跳过"); return; }
    let reg = compile_and_load(SOURCE, "CheckCast");
    assert_eq!(run_err(&reg, "CheckCast", "castFail", "()I"), VmError::ClassCastException);
}
```

> `VmError` 需 `pub` 且 `PartialEq`——已是(`runtime` 重导出)。确认 `rustj::runtime::VmError`
> 可用;若否,改用 `rustj::runtime::VmError` 的完整路径。

- [ ] **Step 2: 看红→看绿**

Run: `cargo test --test checkcast`
Expected: 6 PASS(有 javac)或全跳过。

> 关键:`new Square()` 经 `new`(4.1);`instanceof Shape` 经本层;`(Square) o` 经本层
> checkcast;`new Circle()` 实现 Drawable 经接口闭包。任一失败按指令定位。注意 javac 内部类
> 生成 `CheckCast$Square` 等独立 `.class`,加载目录全部类即可。

- [ ] **Step 3: 提交**

```bash
git add tests/checkcast.rs
git commit -m "test: Layer 4.6 checkcast/instanceof javac 集成闸门"
```

---

### Task 4: 终验

- [ ] `cargo test` → 全绿(单元 + 集成)。
- [ ] `cargo clippy --all-targets -- -D warnings` → 零告警,零 unsafe。
- [ ] 更新 `hotspot-rust-migration-project.md`:Layer 4 增 4.6 完成条;下一步候选更新。

---

## 自检

- **spec 覆盖:** `is_instance`/`supertypes_of`、checkcast/instanceof 语义、ClassCastException、
  null、数组限制(顺延)均覆盖。
- **类型一致:** `resolve_class_name` 复用;`InstanceOop::class_name()` 复用;`is_instance` 形同
  4.2b 闭包 BFS。
- **占位符:** Task2 占位 `type_check_setup` 已说明删除(改由集成闸门覆盖);无其他占位。
- **测试分布:** 子类型判定→klass.rs 单测;字节码端到端→集成闸门(避免 mod.rs 单测重建注册表
  +堆+实例的生命周期复杂度)。
