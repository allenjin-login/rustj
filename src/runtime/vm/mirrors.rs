//! Class / Module 镜像法(Phase B.2.3b T7 从 [`super::vm`] 分解)。
//!
//! Class 镜像 intern(4.10t 起;4.12 退役 `Oop::Class` → 真 `java/lang/Class` Instance)、
//! Module 镜像(4.14a)、按名写实例字段 helper 归此。镜像对应的共享态在 `VmShared`
//!(`class_mirrors`/`mirror_class`/`module_mirrors`/`unnamed_module`)。

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, VmThread};

impl VmThread {
    /// Class 镜像 intern(4.10t 起;4.12 退役 `Oop::Class`):同一内部类名恒返回同一 Class
    /// 镜像引用(对应 HotSpot 每 `Klass` 的单一 `_java_mirror`)。镜像现为**真 `java/lang/Class`
    /// Instance**——首次 `new_instance` 分配,置 VM 字段(`componentType`/`primitive`),并登记
    /// 反查表 `mirror_class`;后续命中直接返。使 `Foo.class == Foo.class`、
    /// `obj.getClass() == Foo.class` 等 Class 身份相等成立。
    /// `name`/`classLoader` 字段保持默认 null(`classLoader`=null 即 Bootstrap)。
    /// `module` 由 [`Self::populate_class_mirror_fields`](4.14a)按类所属模块填。
    /// `name` 由 `getName` 真字节码首次调用时经 `initClassName` 懒填。
    pub(crate) fn intern_class_mirror(&mut self, name: &str) -> Reference {
        // 缓存命中:单次锁取 owned Reference,释 guard 再返(drop-before-recurse;B.2.3b)。
        if let Some(r) = self.runtime.class_mirrors.lock().unwrap().get(name).copied() {
            return r;
        }
        // 分配真 java/lang/Class Instance(须已加载:引导 Class 桩或经闭包预载的真 Class)。
        let r = self.alloc_class_mirror_instance();
        // 先缓存再填字段:数组组件互递归([LC→C、[[I→[I)经缓存命中终止。
        // 两表分别单语句锁 insert,释 guard 后再 populate(其递归 intern 会再锁→须 drop)。
        self.runtime
            .class_mirrors
            .lock()
            .unwrap()
            .insert(name.to_string(), r);
        self.runtime
            .mirror_class
            .lock()
            .unwrap()
            .insert(r, name.to_string());
        self.populate_class_mirror_fields(r, name);
        r
    }

    /// 镜像所表示类型的内部名(供 Class native 反查)。非镜像引用 → `None`。
    /// 返 owned `String`(mirror_class 已 Mutex 化,无法返借用 &str;B.2.3b)。
    pub(crate) fn mirror_internal_name(&self, r: Reference) -> Option<String> {
        self.runtime.mirror_class.lock().unwrap().get(&r).cloned()
    }

    /// 分配一个默认初始化的 `java/lang/Class` Instance。无注册表或 `java/lang/Class` 未加载
    /// (非真实运行场景)→ 返 null 兜底(调用方多为 native,返 null 镜像不致 panic)。
    fn alloc_class_mirror_instance(&mut self) -> Reference {
        let Some(reg) = self.registry() else {
            return Reference::null();
        };
        let Some(class_lc) = reg.get("java/lang/Class") else {
            return Reference::null();
        };
        let inst = reg.new_instance(&class_lc);
        self.runtime.heap.lock().unwrap().alloc(Oop::Instance(inst))
    }

    /// 置 VM 管理的 Class 实例字段:`componentType`(数组→组件镜像)、`primitive`(原语→true)、
    /// `module`(4.14a:按类所属命名模块填 Module 镜像,未标记→无名模块)。字段经名查序号;
    /// `java/lang/Class` 未见该字段(桩精简)→ 静默跳过。
    fn populate_class_mirror_fields(&mut self, mirror: Reference, internal: &str) {
        if let Some(comp) = component_internal_of(internal) {
            let comp_mirror = self.intern_class_mirror(&comp);
            self.set_class_instance_field(mirror, "componentType", Slot::Reference(comp_mirror));
        }
        if is_primitive_keyword(internal) {
            self.set_class_instance_field(mirror, "primitive", Slot::Int(1));
        }
        // Class.module = 所属模块的 Module 镜像(命名模块按类→模块表;否则无名模块)。
        // 对应 Class.java:1011 `private transient Module module;`,getModule() 仅 `return module`。
        let module = self.module_for_class(internal);
        self.set_class_instance_field(mirror, "module", Slot::Reference(module));
        // Class.modifiers(Class.java:1020 `private final transient char modifiers; // Set by the VM`):
        // VM 置类访问标志位(ACC_ENUM/ACC_FINAL/ACC_INTERFACE/ACC_ANNOTATION/…),供 `getModifiers()`
        //(Class.java:1364 `return modifiers;` 直接读字段)→ `isEnum()`(3365 `getModifiers()&ENUM`)、
        // `isAnnotation()`、`isInterface()` 等。对应 HotSpot `JVM_GetClassModifiers`→
        // `InstanceKlass::compute_modifier_flags`:据 `access_flags` 屏蔽到 JVM_RECOGNIZED_CLASS_MODIFIERS
        //(嵌套类另经 InnerClasses 精修可见性位——rustj 暂未解析 InnerClasses,顺延;ENUM 位在
        // access_flags 中故 isEnum 已正确)。数组/原语 → `ACC_PUBLIC|ACC_FINAL|ACC_ABSTRACT`=0x0411
        //(JVM 约定,同 `Reflection.getClassAccessFlags` 原语分支)。实类未加载(异常态)→ 0 兜底。
        // owned i32 取出后 Arc 即释,再 `&mut self` 写槽(NLL,无借用冲突)。
        let mods: i32 = if internal.starts_with('[') || is_primitive_keyword(internal) {
            0x0411
        } else {
            self.registry()
                .and_then(|r| r.get(internal).map(|lc| lc.cf.access_flags.bits() as i32))
                .unwrap_or(0)
        };
        self.set_class_instance_field(mirror, "modifiers", Slot::Int(mods));
    }

    /// 按**字段名**(忽略描述符)在 `java/lang/Class` 扁平实例字段中查序号并写槽。
    pub(crate) fn set_class_instance_field(&mut self, mirror: Reference, field_name: &str, slot: Slot) {
        self.set_instance_field_by_name(mirror, "java/lang/Class", field_name, slot);
    }

    /// 按**字段名**在指定声明类的扁平实例字段中查序号并写槽。类未加载或无此字段 → 静默跳过
    /// (供 Class 镜像字段 + Module 镜像 `name` 字段 + Thread 镜像字段等 VM 管理实例共用)。
    /// `pub(crate)`:跨子模块——[`super::threads`] 的 `alloc_main_thread` 置 Thread 字段、
    /// [`crate::runtime::interpreter::launch`] 的 `populate_module_exports` 置 Module 字段均用之。
    pub(crate) fn set_instance_field_by_name(
        &mut self,
        obj: Reference,
        declaring_class: &str,
        field_name: &str,
        slot: Slot,
    ) {
        let Some(reg) = self.registry() else { return };
        let Some(lc) = reg.get(declaring_class) else { return };
        let Some(ord) = reg
            .flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == field_name)
        else {
            return;
        };
        if let Some(Oop::Instance(i)) = self.heap_mut().get_mut(obj) {
            i.set_field(ord, slot);
        }
    }

    /// 按**字段名**读实例的引用字段(owned `Reference`)。类未加载 / 无此字段 / 非 Instance / 非引用 → `None`。
    /// `pub(crate)`:跨子模块——B.4b `set_thread_status` 读 `Thread.holder`、B.5.2 MH 调用钩子读
    /// DMH.`member` / MemberName.`clazz`.`name` 均用之。
    pub(crate) fn instance_reference_field(
        &self,
        obj: Reference,
        declaring_class: &str,
        field_name: &str,
    ) -> Option<Reference> {
        let reg = self.registry()?;
        let lc = reg.get(declaring_class)?;
        let ord = reg
            .flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == field_name)?;
        let heap = self.runtime.heap.lock().unwrap();
        match heap.get(obj)? {
            Oop::Instance(i) => match i.field(ord) {
                Slot::Reference(r) => Some(r),
                _ => None,
            },
            _ => None,
        }
    }

    /// 按**字段名**读实例的 int 字段。类未加载 / 无此字段 / 非 Instance / 非 int → `None`。
    /// `pub(crate)`:B.5.2 MH 调用钩子读 MemberName.`flags`(提取 refKind)用。
    pub(crate) fn instance_int_field(
        &self,
        obj: Reference,
        declaring_class: &str,
        field_name: &str,
    ) -> Option<i32> {
        let reg = self.registry()?;
        let lc = reg.get(declaring_class)?;
        let ord = reg
            .flattened_instance_fields(&lc)
            .iter()
            .position(|f| f.name == field_name)?;
        let heap = self.runtime.heap.lock().unwrap();
        match heap.get(obj)? {
            Oop::Instance(i) => match i.field(ord) {
                Slot::Int(v) => Some(v),
                _ => None,
            },
            _ => None,
        }
    }

    /// 分配一个默认初始化的 `java/lang/Module` Instance(须已闭包预载)。无注册表或 Module
    /// 未加载 → 返 null 兜底。**不跑 `<init>`**(named/unnamed 两构造器分别调 defineModule0/
    /// 仅置字段;rustj 直接置 `name` 字段,绕过 native 注册)。
    fn alloc_module_instance(&mut self) -> Reference {
        let Some(reg) = self.registry() else {
            return Reference::null();
        };
        let Some(lc) = reg.get("java/lang/Module") else {
            return Reference::null();
        };
        let inst = reg.new_instance(&lc);
        self.runtime.heap.lock().unwrap().alloc(Oop::Instance(inst))
    }

    /// 命名 Module 镜像(intern:同名恒同引用)。分配真 `java/lang/Module` Instance,置 `name`
    /// 字段 = intern(模块名)。对应 HotSpot 每个 `Module` 单例(JVM 侧 `java_lang_Module`)。
    /// `Module.getName()` 真字节码读 `name` 字段即得模块名;`isNamed()` = `name != null`。
    fn intern_named_module(&mut self, name: &str) -> Reference {        // 缓存命中:单次锁取 owned Reference,释 guard 再返(B.2.3b)。
        if let Some(r) = self.runtime.module_mirrors.lock().unwrap().get(name).copied() {
            return r;
        }
        let r = self.alloc_module_instance();
        if r.is_null() {
            return r;
        }
        // 单语句锁 insert,释 guard 后再 intern/set_field(其内部 &mut self;B.2.3b)。
        self.runtime
            .module_mirrors
            .lock()
            .unwrap()
            .insert(name.to_string(), r);
        // 置 Module.name = intern(模块名)(真 String 实例,供 getName/equals 用)。
        if let Ok(name_ref) = crate::runtime::interpreter::string::intern(self, name) {
            self.set_instance_field_by_name(r, "java/lang/Module", "name", Slot::Reference(name_ref));
        }
        r
    }

    /// 无名模块单例(惰性)。`Module(loader)` 未名构造器语义:`name`=null(默认)、`descriptor`=null。
    /// `getName()` 返 null、`isNamed()`=false。用户类(非模块源)经 [`Self::module_for_class`] 归此。
    fn unnamed_module(&mut self) -> Reference {
        // 命中:单次锁取 owned Option<Reference>(Copy),释 guard 再返(B.2.3b)。
        if let Some(r) = *self.runtime.unnamed_module.lock().unwrap() {
            return r;
        }
        let r = self.alloc_module_instance();
        if !r.is_null() {
            *self.runtime.unnamed_module.lock().unwrap() = Some(r);
        }
        r
    }

    /// 命名模块镜像(pub(crate) 入口;语义同 [`intern_named_module`])。供 launch bootstrap
    /// `populate_module_exports` 按**模块名**取 java.base 等模块镜像,填 `descriptor`/
    /// `exportedPackages` 实例字段(Layer 4.14c,解锁端到端反射访问检查)。
    pub(crate) fn named_module_mirror(&mut self, name: &str) -> Reference {
        self.intern_named_module(name)
    }

    /// 类内部名 → 所属模块的 Module 镜像(供 Class.module 字段填充):
    /// (1) `class_module` 命中 → 命名模块镜(load_closure 据「源容器模块」标记);
    /// (2) 数组(`[...`)→ 组件类的模块(递归剥维);
    /// (3) 未标记(用户类 / 原语 / 默认包)→ 无名模块。
    fn module_for_class(&mut self, internal: &str) -> Reference {
        if let Some(m) = self.registry().and_then(|r| r.class_module(internal)) {
            return self.intern_named_module(&m);
        }
        if let Some(comp) = component_internal_of(internal) {
            return self.module_for_class(&comp);
        }
        self.unnamed_module()
    }
}

/// 是否为原语关键字(`int`/`void`/…;非内部描述符 `I`)。原语 Class 镜像的 intern 名即关键字。
fn is_primitive_keyword(s: &str) -> bool {
    matches!(
        s,
        "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double" | "void"
    )
}

/// 数组内部名(`[I`/`[Ljava/lang/String;`/`[[I`)的**组件类型内部名**。非数组 → `None`。
/// 组件为原语时返关键字(`int`);为对象类时返内部名(`java/lang/String`);为嵌套数组返 `[I`。
fn component_internal_of(name: &str) -> Option<String> {
    let rest = name.strip_prefix('[')?;
    match rest.chars().next()? {
        'B' => Some("byte".into()),
        'C' => Some("char".into()),
        'D' => Some("double".into()),
        'F' => Some("float".into()),
        'I' => Some("int".into()),
        'J' => Some("long".into()),
        'S' => Some("short".into()),
        'Z' => Some("boolean".into()),
        'L' => Some(rest.strip_prefix('L')?.strip_suffix(';')?.to_string()),
        '[' => Some(rest.to_string()),
        _ => None,
    }
}
