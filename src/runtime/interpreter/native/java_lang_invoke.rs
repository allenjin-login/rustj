//! `java/lang/invoke/*` 的 native 桥。当前覆盖 **`MethodHandleNatives` 字段族**(Phase B.5.1)
//! ——`init`/`objectFieldOffset`/`staticFieldOffset`/`staticFieldBase`,解锁
//! `Lookup.unreflectField` → `DirectMethodHandle.make` 字段分支建 DMH。
//!
//! **Step 0 源码依据**(移植 `methodHandles.cpp` `init_MemberName`/`init_field_MemberName`):
//! - `MethodHandleNatives.init(MemberName self, Object ref)`(MethodHandleNatives.java:51)静态
//!   native。对 `ref` 为 `java/lang/reflect/Field` 时(methodHandles.cpp:207-222):
//!   读 `Field.clazz`/`Field.slot`,据 fieldDescriptor 填 MemberName。
//! - `init_field_MemberName`(methodHandles.cpp:365-377):`flags = fd.access_flags | IS_FIELD |
//!   ((is_static ? REF_getStatic : REF_getField) << REFERENCE_KIND_SHIFT)`;置 `clazz` = 声明类镜像。
//! - `MemberName(Field, boolean)`(MemberName.java:633-644):**先**调 `init`(填 clazz+flags),
//!   **后**Java 侧置 `name`/`type`;`isResolved()` = `resolution == null`(新对象默认 null)→ 自动 true。
//! - `DirectMethodHandle.make` 字段分支(DirectMethodHandle.java:113-124)调
//!   `staticFieldOffset`/`staticFieldBase`(静态)或 `objectFieldOffset`(实例)。rustj **不解释
//!   LambdaForm**(B.5 设计 §2 shortcut),offset/base 返 dummy——B.5.2 钩子直读 `member` 做访问。
//!
//! MemberName flag 常数(MethodHandleNatives.java:88-96)+ REF_*(MethodHandleNatives.java:101-112)。

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, Value, VmThread, VmError};

use super::super::throw_exception;

/// `MN_IS_METHOD`(MethodHandleNatives.java:88)——方法类成员标志位。
const MN_IS_METHOD: i32 = 0x00010000;
/// `MN_IS_CONSTRUCTOR`(MethodHandleNatives.java:89)——构造器类成员标志位。
const MN_IS_CONSTRUCTOR: i32 = 0x00020000;
/// `MN_IS_FIELD`(MethodHandleNatives.java:90)——字段类成员标志位。
const MN_IS_FIELD: i32 = 0x00040000;
/// `MN_REFERENCE_KIND_SHIFT`(MethodHandleNatives.java:95)——flags 中 refKind 的位移。
const MN_REFERENCE_KIND_SHIFT: i32 = 24;
/// REF_getField / REF_getStatic(MethodHandleNatives.java:103-104)——字段 getter 的两种引用类。
const REF_GET_FIELD: i32 = 1;
const REF_GET_STATIC: i32 = 2;
/// `ACC_STATIC`(JVM access flag 0x0008)——判定字段/方法静态与否。
const ACC_STATIC: i32 = 0x0008;

natives! {
    // MethodHandleNatives.init(MemberName, Object)V —— 静态 native(MemberName 构造器首调)。
    // 移植 methodHandles.cpp:202 init_MemberName + :365 init_field_MemberName(字段分支)。
    (
        "java/lang/invoke/MethodHandleNatives",
        "init",
        "(Ljava/lang/invoke/MemberName;Ljava/lang/Object;)V",
    ) => |vm, _this, args| mhn_init(vm, args);

    // MethodHandleNatives.resolve(MemberName, Class, int, boolean)MemberName —— 静态 native。
    // MemberName.Factory.resolve(MemberName.java:958)调之。移植 `resolve_MemberName`→
    // `init_method_MemberName`/`init_field_MemberName`(methodHandles.cpp:248/365)的 **flags 补全**
    // 部分:`new MemberName(refc,name,type,refKind)` 已置 MN_IS_METHOD/CONSTRUCTOR(MemberName.java:759)
    // 但**不置 ACC_STATIC**(mods=0)→ checkMethod 的 isStatic() 读 `flags & ACC_STATIC` 返 false →
    // "expected a static method"(findStatic invokeStatic 路径)。resolve 据 refKind 补 ACC_STATIC +
    // 种类位(OR 进,不抹除现有 modifiers)。**不查方法/字段本身**:B.5.2 钩子只读 member.clazz/
    // name/flags(不读 vmtarget/vmindex),TRUSTED lookup 绕 checkAccess(MethodHandles.java:3737)
    // 故不需真 modifiers。Java 侧 Factory.resolve 自清 `resolution`(MemberName.java:963)。
    // 解锁 BMH.<clinit>→ClassSpecializer.findFactory→findStatic(speciesCode,"make",type)。
    (
        "java/lang/invoke/MethodHandleNatives",
        "resolve",
        "(Ljava/lang/invoke/MemberName;Ljava/lang/Class;IZ)Ljava/lang/invoke/MemberName;",
    ) => |vm, _this, args| {
        let mname = match args.first().copied() {
            Some(Value::Reference(r)) => r,
            _ => Reference::null(),
        };
        resolve_member_flags(vm, mname);
        Ok(Value::Reference(mname))
    };

    // objectFieldOffset(MemberName)J —— DirectMethodHandle.make 实例字段分支调(DirectMethodHandle.java:120)
    // 取偏移,存入 `Accessor.fieldOffset`(DMH 实例字段),供 prepared 字段 LF 的 `fieldOffset(dmh)`
    // 节点读出、再喂 `Unsafe.getInt(base, ord)`。rustj 无真实内存偏移:**返字段在声明类扁平实例
    // 布局中的序号(ord)**(同 `objectFieldOffset1` 的内部自洽模型),`Unsafe.getInt` 据 base 为
    // Instance 时按 ord 直读实例槽。读 MemberName.clazz(声明类镜像)+ name(字段名)→
    // `resolve_instance_field_by_name`(仅名匹配)。未找到 → -1。
    (
        "java/lang/invoke/MethodHandleNatives",
        "objectFieldOffset",
        "(Ljava/lang/invoke/MemberName;)J",
    ) => |vm, _this, args| object_field_offset(vm, args);

    // staticFieldOffset(MemberName)J —— HotSpot `init_field_MemberName` 填的 vmindex(fd.offset()
    // 占位)。**Phase G.1b**:物种类链接(ClassSpecializer.linkCodeToSpeciesData:938)经
    // `Unsafe.putReference(staticFieldBase(sdField), staticFieldOffset(sdField), speciesData)` 写
    // SpeciesData 到物种类静态 SD 字段 → 须返真序号。读 MemberName.clazz(声明类镜像)+
    // MemberName.name(字段名)→ `resolve_static_field_by_name`(仅名匹配,MemberName.type 对字段
    // 存 Class 对象非描述符)。序号 = 声明类 static_storage 索引;`staticFieldBase` 返同一声明类
    // 镜像 → putReference 路由到该类 static_storage[ord] 自洽。未找到 → -1(putReference 越界兜底)。
    (
        "java/lang/invoke/MethodHandleNatives",
        "staticFieldOffset",
        "(Ljava/lang/invoke/MemberName;)J",
    ) => |vm, _this, args| static_field_offset(vm, args);

    // staticFieldBase(MemberName)Object —— HotSpot 返声明类镜像(`init_field_MemberName` set_clazz)。
    // **Phase G.1b**:物种类链接读 sdField.clazz 作 putReference 的 base;putReference 据「base 是
    // Class 镜像」路由到静态字段(static_storage[ord])。返 MemberName.clazz;无 clazz → null。
    (
        "java/lang/invoke/MethodHandleNatives",
        "staticFieldBase",
        "(Ljava/lang/invoke/MemberName;)Ljava/lang/Object;",
    ) => |vm, _this, args| static_field_base(vm, args);
}

/// 据 refKind 补 `MemberName.flags` 的成员种类位(MN_IS_METHOD/CONSTRUCTOR/FIELD)+ `ACC_STATIC`
/// (`resolve` native 用)。移植 `init_method_MemberName`/`init_field_MemberName` flags 公式
/// (methodHandles.cpp:263/329/331/365)的「种类位 + ACC_STATIC」部分。`new MemberName(refc,name,
/// type,refKind)` 已置 MN_IS_METHOD/CONSTRUCTOR(MemberName.java:759)但**不置 ACC_STATIC**(mods=0)
/// → `checkMethod` 的 `isStatic()` 读 `flags & ACC_STATIC` 返 false → "expected a static method"
/// (findStatic invokeStatic 路径,如 BMH 物种 `make` 方法)。本函数据 refKind 补:
/// - 种类位:{1-4 getField/getStatic/putField/putStatic}→MN_IS_FIELD;{8 newInvokeSpecial}→MN_IS_CONSTRUCTOR;
///   {5/6/7/9 invokeVirtual/Static/Special/Interface}→MN_IS_METHOD。
/// - ACC_STATIC(refKind∈{getStatic=2, putStatic=4, invokeStatic=6}):HotSpot 据 `m->is_static()`。
///
/// **仅 OR 进位**(不抹除现有 modifiers);不查方法/字段本身(TRUSTED lookup 绕 checkAccess;
/// B.5.2 钩子只读 refKind)。flags 读/写经名查序号,MemberName 未加载/无 flags 字段 → 静默跳过。
fn resolve_member_flags(vm: &mut VmThread, mname: Reference) {
    let Some(flags) = vm.instance_int_field(mname, "java/lang/invoke/MemberName", "flags") else {
        return;
    };
    let ref_kind = (flags >> MN_REFERENCE_KIND_SHIFT) & 0x0F;
    let kind_bit: i32 = match ref_kind {
        REF_GET_FIELD | REF_GET_STATIC | 3 | 4 => MN_IS_FIELD,
        8 => MN_IS_CONSTRUCTOR,
        5 | 6 | 7 | 9 => MN_IS_METHOD,
        _ => 0,
    };
    let acc_static = if matches!(ref_kind, REF_GET_STATIC | 4 | 6) {
        ACC_STATIC
    } else {
        0
    };
    let new_flags = flags | kind_bit | acc_static;
    if new_flags != flags {
        vm.set_instance_field_by_name(
            mname,
            "java/lang/invoke/MemberName",
            "flags",
            Slot::Int(new_flags),
        );
    }
}

/// `MethodHandleNatives.staticFieldOffset(MemberName)`:返静态字段在声明类 `static_storage`
/// 中的序号(Phase G.1b 物种 SD 字段链接用)。读 MemberName.clazz(声明类 Class 镜像)+
/// MemberName.name(字段名 String)→ `resolve_static_field_by_name`(仅名匹配)。各步 `&self`
/// /`&mut self` 读出块即释放,无跨语句借用。MemberName 非法/字段未找到 → -1(putReference 越界兜底)。
fn static_field_offset(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let mname = match args.first().copied() {
        Some(Value::Reference(r)) if !r.is_null() => r,
        _ => return Ok(Value::Long(-1)),
    };
    let Some(clazz_mirror) = vm.instance_reference_field(mname, "java/lang/invoke/MemberName", "clazz")
    else {
        return Ok(Value::Long(-1));
    };
    let Some(name_ref) = vm.instance_reference_field(mname, "java/lang/invoke/MemberName", "name")
    else {
        return Ok(Value::Long(-1));
    };
    let Some(internal) = vm.mirror_internal_name(clazz_mirror) else {
        return Ok(Value::Long(-1));
    };
    let field_name = match super::super::string::read_text(vm, name_ref)? {
        Some(t) => t,
        None => return Ok(Value::Long(-1)),
    };
    let ord = vm
        .registry()
        .and_then(|reg| reg.resolve_static_field_by_name(&internal, &field_name))
        .map(|(_, o)| o as i64)
        .unwrap_or(-1);
    Ok(Value::Long(ord))
}

/// `MethodHandleNatives.objectFieldOffset(MemberName)`:返实例字段在声明类扁平实例布局中的
/// **序号**(ord;rustj 无真实内存偏移,以 ord 代之,与 `Unsafe.getInt` 的 Instance 路径自洽)。
/// DMH 实例字段 Accessor 存此 ord 作 `fieldOffset`,prepared 字段 LF 经 `fieldOffset(dmh)` 读出、
/// 喂 `Unsafe.getInt(base, ord)`。读 MemberName.clazz(声明类 Class 镜像)+ name(字段名 String)→
/// `resolve_instance_field_by_name`(仅名匹配)。各步 `&self`/`&mut self` 读出块即释放。未找到 → -1。
fn object_field_offset(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let mname = match args.first().copied() {
        Some(Value::Reference(r)) if !r.is_null() => r,
        _ => return Ok(Value::Long(-1)),
    };
    let Some(clazz_mirror) = vm.instance_reference_field(mname, "java/lang/invoke/MemberName", "clazz")
    else {
        return Ok(Value::Long(-1));
    };
    let Some(name_ref) = vm.instance_reference_field(mname, "java/lang/invoke/MemberName", "name")
    else {
        return Ok(Value::Long(-1));
    };
    let Some(internal) = vm.mirror_internal_name(clazz_mirror) else {
        return Ok(Value::Long(-1));
    };
    let field_name = match super::super::string::read_text(vm, name_ref)? {
        Some(t) => t,
        None => return Ok(Value::Long(-1)),
    };
    let ord = vm
        .registry()
        .and_then(|reg| reg.resolve_instance_field_by_name(&internal, &field_name))
        .map(|(_, o)| o as i64)
        .unwrap_or(-1);
    Ok(Value::Long(ord))
}

/// `MethodHandleNatives.staticFieldBase(MemberName)`:返声明类 Class 镜像(Phase G.1b)。
/// 物种链接以此作 `Unsafe.putReference` 的 base;putReference 据「base 是 Class 镜像」
/// 路由到静态字段。即 MemberName.clazz;无 clazz → null。
fn static_field_base(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let mname = match args.first().copied() {
        Some(Value::Reference(r)) if !r.is_null() => r,
        _ => return Ok(Value::Reference(Reference::null())),
    };
    Ok(match vm.instance_reference_field(mname, "java/lang/invoke/MemberName", "clazz") {
        Some(c) => Value::Reference(c),
        None => Value::Reference(Reference::null()),
    })
}

/// `MethodHandleNatives.init(self, ref)`:据 `ref` 类型填 MemberName。当前仅 **Field** 分支
/// (Method/Constructor 反射走 NativeAccessor 4.15b,不经此);Field 分支移植 init_field_MemberName。
fn mhn_init(vm: &mut VmThread, args: &[Value]) -> Result<Value, VmError> {
    let self_ref = match args.first().copied() {
        Some(Value::Reference(r)) => r,
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let target = match args.get(1).copied() {
        Some(Value::Reference(r)) => r,
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // 读 target 运行时类名(methodHandles.cpp:206 target_klass;精确匹配 reflect_Field_klass)。
    let target_class = {
        let heap = vm.heap();
        match heap.get(target) {
            Some(Oop::Instance(i)) => i.class_name().to_string(),
            _ => String::new(), // 非 Instance → 不填(init_MemberName 返 null 对应 clazz 仍 null)
        }
    };
    if target_class == "java/lang/reflect/Field" {
        init_from_field(vm, self_ref, target)?;
    }
    // Method/Constructor 经 NativeAccessor(4.15b c9a6bc1),本路径暂不触;留空对应 init 失败。
    Ok(Value::Void)
}

/// 字段分支:读 `Field.clazz`/`Field.modifiers` → 置 `MemberName.clazz` + `MemberName.flags`。
/// 移植 methodHandles.cpp init_field_MemberName:367-368(flags 公式)+ :377 set_clazz。
fn init_from_field(vm: &mut VmThread, self_ref: Reference, fld: Reference) -> Result<(), VmError> {
    // 读 Field.clazz(Class 镜像)+ Field.modifiers(int)。单次 heap 锁取 owned(同 read_executable_meta)。
    let (clazz, modifiers) = read_field_meta(vm, fld)?;
    let is_static = (modifiers & ACC_STATIC) != 0;
    let ref_kind = if is_static { REF_GET_STATIC } else { REF_GET_FIELD };
    let flags = modifiers | MN_IS_FIELD | (ref_kind << MN_REFERENCE_KIND_SHIFT);
    // 置 MemberName.clazz + MemberName.flags(经 set_instance_field_by_name,跨子模块 pub(crate))。
    vm.set_instance_field_by_name(
        self_ref,
        "java/lang/invoke/MemberName",
        "clazz",
        Slot::Reference(clazz),
    );
    vm.set_instance_field_by_name(
        self_ref,
        "java/lang/invoke/MemberName",
        "flags",
        Slot::Int(flags),
    );
    Ok(())
}

/// 读 `java/lang/reflect/Field` 镜像的 `clazz`(Class 镜像)+ `modifiers`(int)。单次 heap 锁取 owned。
/// 模式同 jdk_internal_reflect::read_executable_meta(Field 亦同 Executable 字段布局:clazz/modifiers)。
fn read_field_meta(vm: &VmThread, fld: Reference) -> Result<(Reference, i32), VmError> {
    let (clazz_ord, mod_ord) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("MethodHandleNatives.init 需类注册表"))?;
        let lc = reg
            .get("java/lang/reflect/Field")
            .ok_or(VmError::BadConstant("Field 类未加载"))?;
        let flat = reg.flattened_instance_fields(&lc);
        let find = |n: &str| {
            flat.iter()
                .position(|f| f.name == n)
                .ok_or(VmError::BadConstant("Field 缺 clazz/modifiers 字段"))
        };
        (find("clazz")?, find("modifiers")?)
    };
    let heap = vm.heap();
    let inst = match heap.get(fld) {
        Some(Oop::Instance(i)) => i,
        _ => return Err(VmError::BadConstant("Field 引用非 Instance")),
    };
    let clazz = match inst.field(clazz_ord) {
        Slot::Reference(r) => r,
        _ => return Err(VmError::BadConstant("Field.clazz 非引用")),
    };
    let modifiers = match inst.field(mod_ord) {
        Slot::Int(v) => v,
        _ => return Err(VmError::BadConstant("Field.modifiers 非 int")),
    };
    Ok((clazz, modifiers))
}
