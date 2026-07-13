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
use crate::runtime::{Reference, Slot, Value, Vm, VmError};

use super::super::throw_exception;

/// `MN_IS_FIELD`(MethodHandleNatives.java:90)——字段类成员标志位。
const MN_IS_FIELD: i32 = 0x00040000;
/// `MN_REFERENCE_KIND_SHIFT`(MethodHandleNatives.java:95)——flags 中 refKind 的位移。
const MN_REFERENCE_KIND_SHIFT: i32 = 24;
/// REF_getField / REF_getStatic(MethodHandleNatives.java:103-104)——字段 getter 的两种引用类。
const REF_GET_FIELD: i32 = 1;
const REF_GET_STATIC: i32 = 2;
/// `ACC_STATIC`(JVM access flag 0x0008)——判定字段静态与否。
const ACC_STATIC: i32 = 0x0008;

/// `java/lang/invoke/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // MethodHandleNatives.init(MemberName, Object)V —— 静态 native(MemberName 构造器首调)。
        // 移植 methodHandles.cpp:202 init_MemberName + :365 init_field_MemberName(字段分支)。
        ("java/lang/invoke/MethodHandleNatives", "init", "(Ljava/lang/invoke/MemberName;Ljava/lang/Object;)V") => {
            mhn_init(vm, args)
        }

        // MethodHandleNatives.resolve(MemberName, Class, int, boolean)MemberName —— 静态 native。
        // MemberName.Factory.resolve(MemberName.java:958)调之。**rustj shortcut**:入参 MemberName 已由
        // 其构造器(`init(Class,String,MethodType,int)` → resolution=null)置好 clazz/name/type/flags,
        // `isResolved()` 已 true;resolve 在 HotSpot 侧填 vmtarget/vmindex 仅供 LF 解释/rustj 不解释 LF
        // (B.5 设计 §2,B.5.2 钩子直读 member)。故**原样返回入参**即足;断言全关(`-ea` off,
        // desiredAssertionStatus0=false)故 vminfoIsConsistent 不触、getMemberVMInfo 不调。
        // 解锁 DirectMethodHandle.makePreparedFieldLambdaForm:766 resolve linker(Unsafe.getXxx)。
        ("java/lang/invoke/MethodHandleNatives", "resolve", "(Ljava/lang/invoke/MemberName;Ljava/lang/Class;IZ)Ljava/lang/invoke/MemberName;") => {
            let mname = match args.first().copied() {
                Some(Value::Reference(r)) => r,
                _ => Reference::null(),
            };
            Ok(Value::Reference(mname))
        }

        // objectFieldOffset(MemberName)J / staticFieldOffset(MemberName)J —— DirectMethodHandle.make
        // 字段分支调(DirectMethodHandle.java:116/120)取偏移。rustj shortcut **不读** offset(B.5.2
        // 钩子直读 member);返 0 作 dummy(methodHandles.cpp:371 vmindex=fd.offset() 的占位)。
        ("java/lang/invoke/MethodHandleNatives", "objectFieldOffset", "(Ljava/lang/invoke/MemberName;)J")
        | ("java/lang/invoke/MethodHandleNatives", "staticFieldOffset", "(Ljava/lang/invoke/MemberName;)J") => {
            Ok(Value::Long(0))
        }

        // staticFieldBase(MemberName)Object —— DirectMethodHandle.make 静态分支调(:117)取 base
        // (HotSpot 返声明类镜像)。rustj shortcut 不读;返 null 作 dummy(StaticAccessor.base 字段占位)。
        ("java/lang/invoke/MethodHandleNatives", "staticFieldBase", "(Ljava/lang/invoke/MemberName;)Ljava/lang/Object;") => {
            Ok(Value::Reference(Reference::null()))
        }

        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

/// `MethodHandleNatives.init(self, ref)`:据 `ref` 类型填 MemberName。当前仅 **Field** 分支
/// (Method/Constructor 反射走 NativeAccessor 4.15b,不经此);Field 分支移植 init_field_MemberName。
fn mhn_init(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
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
fn init_from_field(vm: &mut Vm, self_ref: Reference, fld: Reference) -> Result<(), VmError> {
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
fn read_field_meta(vm: &Vm, fld: Reference) -> Result<(Reference, i32), VmError> {
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
