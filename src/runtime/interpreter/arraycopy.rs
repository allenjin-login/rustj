//! `System.arraycopy` native(Layer 4.10l)。
//!
//! 对应 HotSpot `prims/jvm.cpp:293-305` `JVM_ArrayCopy` → 按源数组 klass 分派
//! `typeArrayKlass::copy_array`(`oops/typeArrayKlass.cpp:108-174`)与
//! `objArrayKlass::copy_array` + `do_copy`(`oops/objArrayKlass.cpp:206-316`)。
//! 检查序、无符号越界算术、引用数组的 checkcast(首个不可赋元素处拷完前缀后抛 ASE)、
//! 重叠 memmove 择向,均忠实移植。设计见
//! `docs/superpowers/specs/2026-06-30-system-arraycopy-design.md`。

use super::type_check::array_instanceof;
use super::{throw_exception, throw_exception_with_message, Value, VmError};
use crate::oops::Oop;
use crate::runtime::{Reference, Slot, Vm};

/// 引用异数组拷贝的逐元素 checkcast 上下文:目标组件 + 失败时的 ASE 消息。
/// 消息镜像 HotSpot `throw_array_store_exception`(objArrayKlass.cpp:187-203):
/// bound(dst 组件)非 stype(src 组件)子类型 → "type mismatch: can not copy …[] into …[]";
/// 否则(逐元素个别不可赋)→ "element type mismatch: can not cast one of the elements of …[]"。
struct Checkcast<'a> {
    dst_comp: &'a str,
    msg: String,
}

/// `java/lang/System.arraycopy(Object,I,Object,I,I)V`。
///
/// 静态 native;实参已由 `native::invoke` 解构为正序 5 参传入。按 HotSpot 权威检查序
/// (null→NPE、非数组/类型不符→ASE、负值/越界→AIOOBE)校验后拷贝;void 返回。
pub(super) fn system_arraycopy(
    vm: &mut Vm<'_>,
    src: Reference,
    src_pos: i32,
    dst: Reference,
    dst_pos: i32,
    length: i32,
) -> Result<Value, VmError> {
    // 1. null → NPE(jvm.cpp:296 `src==null || dst==null`)。
    if src.is_null() || dst.is_null() {
        return Err(throw_exception(vm, "java/lang/NullPointerException"));
    }
    // 2. 非数组 → ASE(typeArrayKlass/objArrayKlass:255 "destination type … is not an array")。
    //    array_meta 仅克隆描述符,不持借用 → 释放后再 throw 取 &mut vm。
    let (src_desc, src_len) = match array_meta(vm, src) {
        Some(m) => m,
        None => {
            return Err(throw_exception_with_message(
                vm,
                "java/lang/ArrayStoreException",
                &format!(
                    "arraycopy: source type {} is not an array",
                    oop_external_name(vm, src)
                ),
            ));
        }
    };
    let (dst_desc, dst_len) = match array_meta(vm, dst) {
        Some(m) => m,
        None => {
            return Err(throw_exception_with_message(
                vm,
                "java/lang/ArrayStoreException",
                &format!(
                    "arraycopy: destination type {} is not an array",
                    oop_external_name(vm, dst)
                ),
            ));
        }
    };
    let src_comp = component_of(&src_desc);
    let dst_comp = component_of(&dst_desc);
    let src_prim = is_primitive_component(src_comp);
    let dst_prim = is_primitive_component(dst_comp);
    // 3. 类型相容(typeArrayKlass:112/123、objArrayKlass:248):先于越界。
    if src_prim {
        if !dst_prim {
            // 基本 src → 引用数组 dst(typeArrayKlass:116 "into object array[]")。
            return Err(throw_exception_with_message(
                vm,
                "java/lang/ArrayStoreException",
                &format!(
                    "arraycopy: type mismatch: can not copy {}[] into object array[]",
                    element_external(src_comp)
                ),
            ));
        } else if dst_comp != src_comp {
            // 基本 src → 基本 dst 异型(typeArrayKlass:126)。
            return Err(throw_exception_with_message(
                vm,
                "java/lang/ArrayStoreException",
                &format!(
                    "arraycopy: type mismatch: can not copy {}[] into {}[]",
                    element_external(src_comp),
                    element_external(dst_comp)
                ),
            ));
        }
    } else if dst_prim {
        // 引用 src → 基本 dst(objArrayKlass:252 "object array[] into {T}[]")。
        return Err(throw_exception_with_message(
            vm,
            "java/lang/ArrayStoreException",
            &format!(
                "arraycopy: type mismatch: can not copy object array[] into {}[]",
                element_external(dst_comp)
            ),
        ));
    }
    // 4. 负值 → AIOOBE(typeArrayKlass:133 / objArrayKlass:261)。HotSpot 按 src_pos/dst_pos/length
    //    优先序给消息。
    if src_pos < 0 || dst_pos < 0 || length < 0 {
        let msg = if src_pos < 0 {
            format!(
                "arraycopy: source index {src_pos} out of bounds for {}[{src_len}]",
                kind_label(src_prim, src_comp)
            )
        } else if dst_pos < 0 {
            format!(
                "arraycopy: destination index {dst_pos} out of bounds for {}[{dst_len}]",
                kind_label(dst_prim, dst_comp)
            )
        } else {
            format!("arraycopy: length {length} is negative")
        };
        return Err(throw_exception_with_message(
            vm,
            "java/lang/ArrayIndexOutOfBoundsException",
            &msg,
        ));
    }
    // 5. 越界 → AIOOBE(无符号算术 `(u32)len+(u32)pos > len` → i64 等价且溢出安全;
    //    typeArrayKlass:149 / objArrayKlass:277)。HotSpot 按 src/dst 优先序给消息;
    //    "last source/destination index {n}"(n = pos+length,已过负值检查故非负)。
    if src_pos as i64 + length as i64 > src_len as i64
        || dst_pos as i64 + length as i64 > dst_len as i64
    {
        let msg = if src_pos as i64 + length as i64 > src_len as i64 {
            format!(
                "arraycopy: last source index {} out of bounds for {}[{src_len}]",
                src_pos as i64 + length as i64,
                kind_label(src_prim, src_comp)
            )
        } else {
            format!(
                "arraycopy: last destination index {} out of bounds for {}[{dst_len}]",
                dst_pos as i64 + length as i64,
                kind_label(dst_prim, dst_comp)
            )
        };
        return Err(throw_exception_with_message(
            vm,
            "java/lang/ArrayIndexOutOfBoundsException",
            &msg,
        ));
    }
    // 6. length==0 → 空(typeArrayKlass:166 / objArrayKlass:296)。
    if length == 0 {
        return Ok(Value::Void);
    }
    // 7. 拷贝。是否须逐元素 checkcast:
    //    - 基本:否;引用同数组:否(do_copy:208 "source==destination, no conversion checks");
    //    - 引用异数组:src 组件非 dst 组件子类型 → 是(checkcast,首个不可赋处拷前缀后 ASE)。
    let checkcast = if src_prim || src == dst {
        None
    } else {
        let Some(reg) = vm.registry() else {
            return Err(VmError::BadConstant("arraycopy checkcast 需类注册表"));
        };
        if component_assignable(src_comp, dst_comp, reg) {
            None
        } else {
            // 消息镜像 throw_array_store_exception(objArrayKlass:192-200)。
            let msg = if !component_assignable(dst_comp, src_comp, reg) {
                format!(
                    "arraycopy: type mismatch: can not copy {}[] into {}[]",
                    element_external(src_comp),
                    element_external(dst_comp)
                )
            } else {
                format!(
                    "arraycopy: element type mismatch: can not cast one of the elements of {}[] to the type of the destination array, {}",
                    element_external(src_comp),
                    element_external(dst_comp)
                )
            };
            Some(Checkcast {
                dst_comp,
                msg,
            })
        }
    };
    copy_elements(vm, src, src_pos, dst, dst_pos, length, checkcast)?;
    Ok(Value::Void)
}

/// 逐元素拷贝(memmove 择向 + 可选 checkcast)。
///
/// 每轮:不可变借读一个 `Slot`(Copy,即释放)→(checkcast 时查可赋性,不可赋即 ASE 带
/// `throw_array_store_exception` 消息,前缀已写)→ 可变借写。读/写分属不同借用时刻,故
/// **同一数组**(src==dst)亦可安全自拷。`checkcast` 为 `Some` 时对每个非 null 引用元素查可赋性。
fn copy_elements(
    vm: &mut Vm<'_>,
    src: Reference,
    src_pos: i32,
    dst: Reference,
    dst_pos: i32,
    length: i32,
    checkcast: Option<Checkcast<'_>>,
) -> Result<(), VmError> {
    // 重叠择向:同数组且 dst_pos>src_pos → 后向(防前向读时已写位被覆盖);否则前向
    // (copy.hpp conjoint_* = memmove 语义)。异数组无重叠,前向即可。
    let backward = src == dst && dst_pos > src_pos;
    let n = length as usize;
    for k in 0..n {
        let idx = if backward { n - 1 - k } else { k };
        let slot = read_element(vm, src, src_pos as usize + idx)?;
        if let Some(cc) = &checkcast
            && let Slot::Reference(r) = slot
            && !r.is_null()
        {
            let Some(reg) = vm.registry() else {
                return Err(VmError::BadConstant("arraycopy checkcast 需类注册表"));
            };
            let elem_comp = element_component(vm, r)?;
            if !component_assignable(&elem_comp, cc.dst_comp, reg) {
                return Err(throw_exception_with_message(
                    vm,
                    "java/lang/ArrayStoreException",
                    &cc.msg,
                ));
            }
        }
        write_element(vm, dst, dst_pos as usize + idx, slot)?;
    }
    Ok(())
}

/// 取数组描述符(克隆)与长度;非数组 → `None`(供非数组判 ASE)。
fn array_meta(vm: &Vm<'_>, r: Reference) -> Option<(String, usize)> {
    match vm.heap().get(r) {
        Some(Oop::Array(a)) => Some((a.class_name().to_string(), a.length())),
        _ => None,
    }
}

/// 非数组 oop 的外部名(供 "source/destination type … is not an array" 消息):内部名 → 点分。
fn oop_external_name(vm: &Vm<'_>, r: Reference) -> String {
    match vm.heap().get(r) {
        Some(Oop::Instance(i)) => i.class_name().replace('/', "."),
        Some(Oop::Array(a)) => a.class_name().replace('/', "."),
        _ => "<unknown>".into(),
    }
}

/// 读一个元素槽(调用方已做越界检查)。
fn read_element(vm: &Vm<'_>, r: Reference, idx: usize) -> Result<Slot, VmError> {
    match vm.heap().get(r) {
        Some(Oop::Array(a)) => Ok(a.element(idx)),
        _ => Err(VmError::BadConstant("arraycopy 源非数组")),
    }
}

/// 写一个元素槽。
fn write_element(vm: &mut Vm<'_>, r: Reference, idx: usize, slot: Slot) -> Result<(), VmError> {
    match vm.heap_mut().get_mut(r) {
        Some(Oop::Array(a)) => {
            a.set_element(idx, slot);
            Ok(())
        }
        _ => Err(VmError::BadConstant("arraycopy 目标非数组")),
    }
}

/// 元素运行时类型 → 组件描述符(供 [`component_assignable`] 比对 dst 组件)。
/// 实例 `java/lang/String` → `Ljava/lang/String;`;数组 `[I` → `[I`;Class 镜像为
/// `java/lang/Class` Instance → `Ljava/lang/Class;`(经 Instance 臂)。
pub(super) fn element_component(vm: &Vm<'_>, r: Reference) -> Result<String, VmError> {
    Ok(match vm.heap().get(r) {
        Some(Oop::Instance(i)) => format!("L{};", i.class_name()),
        Some(Oop::Array(a)) => a.class_name().to_string(),
        Some(Oop::Lambda(l)) => format!("L{};", l.sam_type()),
        None => return Err(VmError::BadConstant("arraycopy 元素引用悬空")),
    })
}

/// 数组描述符的组件段(`[B`→`B`、`[Ljava/lang/String;`→`Ljava/lang/String;`、`[[I`→`[I`)。
/// 数组描述符必以 `[`(单字节)起;防御性 `get(1..)`。
pub(super) fn component_of(desc: &str) -> &str {
    desc.get(1..).unwrap_or("")
}

/// 组件段是否基本类型(单字符 BCDFIJSZ)。
fn is_primitive_component(comp: &str) -> bool {
    matches!(comp, "B" | "C" | "D" | "F" | "I" | "J" | "S" | "Z")
}

/// 组件 → HotSpot `type2name_tab` 外部名(`I`→`int`、`B`→`byte` …);引用 `L…;` → 点分类名
/// (`Ljava/lang/String;`→`java.lang.String`);数组描述符按点分兜底。供 arraycopy 消息
/// (`typeArrayKlass.cpp` / `objArrayKlass.cpp` 的 `THROW_MSG`、`external_name`)。
fn element_external(comp: &str) -> String {
    match comp {
        "B" => "byte".into(),
        "C" => "char".into(),
        "D" => "double".into(),
        "F" => "float".into(),
        "I" => "int".into(),
        "J" => "long".into(),
        "S" => "short".into(),
        "Z" => "boolean".into(),
        _ => {
            if let Some(inner) = comp.strip_prefix('L').and_then(|s| s.strip_suffix(';')) {
                inner.replace('/', ".")
            } else {
                comp.replace('/', ".")
            }
        }
    }
}

/// 数组端的 kind 描述(供越界消息):基本 → 类型名;引用 → `"object array"`
/// (HotSpot `objArrayKlass.cpp:266/269` 字面量 "object array[%d]")。
fn kind_label(is_prim: bool, comp: &str) -> String {
    if is_prim {
        element_external(comp)
    } else {
        "object array".into()
    }
}

/// 组件 `a` 可赋给组件 `b`(JVMS 数组子类型递归 ⟺「`[a` instanceof `[b`」)。
/// 数组描述符 = 组件前补单个 `[`(基本 `I`→`[I`;`Ljava/lang/String;`→`[Ljava/lang/String;`;
/// `[I`→`[[I`),**不加**右括号(`L…;` 自终结)。复用 [`array_instanceof`]:同描述符/超类
/// Object[] 等短路在前,不触注册表;引用组件方走 `is_instance`。
pub(super) fn component_assignable(a: &str, b: &str, reg: &crate::oops::ClassRegistry) -> bool {
    array_instanceof(&format!("[{a}"), &format!("[{b}"), reg)
}

#[cfg(test)]
mod tests {
    //! 直调 [`system_arraycopy`],覆盖 HotSpot 权威检查序各分支 + 重叠 memmove + checkcast。
    //! 引用组件的子类型正确性(经 `is_instance`)已由 `type_check` 单测覆盖;本组聚焦
    //! arraycopy 的编排与 check 序,用可短路判定(同型 / Object[] / 不可赋)避开注册表依赖
    //! (checkcast 路径需注册表存在,用空 `ClassRegistry`)。

    use super::*;
    use crate::oops::{ArrayOop, ClassRegistry, InstanceOop};
    use crate::runtime::Slot;

    /// 构 `int[]` 并入堆。
    fn int_array(vm: &mut Vm<'_>, vals: &[i32]) -> Reference {
        let els: Vec<Slot> = vals.iter().map(|&v| Slot::Int(v)).collect();
        vm.heap_mut()
            .alloc(Oop::Array(ArrayOop::new("[I".into(), els)))
    }

    /// 构指定描述符、给定 `Slot` 元素的数组并入堆。
    fn array_of(vm: &mut Vm<'_>, desc: &str, els: Vec<Slot>) -> Reference {
        vm.heap_mut()
            .alloc(Oop::Array(ArrayOop::new(desc.into(), els)))
    }

    /// 读 `int[]` 前 `n` 元为 `Vec<i32>`。
    fn read_ints(vm: &Vm<'_>, r: Reference, n: usize) -> Vec<i32> {
        let Some(Oop::Array(a)) = vm.heap().get(r) else {
            return vec![];
        };
        (0..n)
            .map(|i| match a.element(i) {
                Slot::Int(v) => v,
                _ => 0,
            })
            .collect()
    }

    /// 取数组元素的 `Slot::Reference`(供引用拷贝断言)。
    fn read_ref(vm: &Vm<'_>, r: Reference, idx: usize) -> Reference {
        let Some(Oop::Array(a)) = vm.heap().get(r) else {
            return Reference::null();
        };
        match a.element(idx) {
            Slot::Reference(r) => r,
            _ => Reference::null(),
        }
    }

    fn assert_exc_class(vm: &Vm<'_>, result: Result<Value, VmError>, expected: &str) {
        let Err(VmError::ThrownException(r)) = result else {
            panic!("期望 ThrownException,得 {result:?}");
        };
        let Some(Oop::Instance(i)) = vm.heap().get(r) else {
            panic!("异常应为 Instance");
        };
        assert_eq!(i.class_name(), expected);
    }

    /// 取异常的 format_trace 文本(供 detailMessage 断言)。调用方须先求 result 再借 vm。
    fn exc_trace(vm: &Vm<'_>, result: Result<Value, VmError>) -> String {
        let Err(VmError::ThrownException(r)) = result else {
            panic!("期望 ThrownException,得 {result:?}");
        };
        vm.format_trace(r)
    }

    #[test]
    fn primitive_type_mismatch_carries_message() {
        // int[] → byte[]:ASE 消息 "type mismatch: can not copy int[] into byte[]"
        // (typeArrayKlass:126)。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1, 2]);
        let dst = array_of(&mut vm, "[B", vec![Slot::Int(0); 2]);
        let result = system_arraycopy(&mut vm, src, 0, dst, 0, 2);
        let trace = exc_trace(&vm, result);
        assert!(
            trace.contains("java/lang/ArrayStoreException: arraycopy: type mismatch: can not copy int[] into byte[]"),
            "类型不符消息,得:\n{trace}"
        );
    }

    #[test]
    fn out_of_bounds_carries_last_source_index_message() {
        // src_pos(1)+length(2)=3 > src_len(2) → AIOOBE "last source index 3 out of bounds for int[2]"
        // (typeArrayKlass:155)。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1, 2]);
        let dst = int_array(&mut vm, &[0; 2]);
        let result = system_arraycopy(&mut vm, src, 1, dst, 0, 2);
        let trace = exc_trace(&vm, result);
        assert!(
            trace.contains("java/lang/ArrayIndexOutOfBoundsException: arraycopy: last source index 3 out of bounds for int[2]"),
            "越界消息,得:\n{trace}"
        );
    }

    #[test]
    fn negative_srcpos_carries_source_index_message() {
        // src_pos=-1 → AIOOBE "source index -1 out of bounds for int[1]"(typeArrayKlass:138)。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1]);
        let dst = int_array(&mut vm, &[0]);
        let result = system_arraycopy(&mut vm, src, -1, dst, 0, 1);
        let trace = exc_trace(&vm, result);
        assert!(
            trace.contains("java/lang/ArrayIndexOutOfBoundsException: arraycopy: source index -1 out of bounds for int[1]"),
            "负值消息,得:\n{trace}"
        );
    }

    #[test]
    fn null_source_is_npe() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let dst = int_array(&mut vm, &[0; 1]);
        let result = system_arraycopy(&mut vm, Reference::null(), 0, dst, 0, 0);
        assert_exc_class(&vm, result, "java/lang/NullPointerException");
    }

    #[test]
    fn null_dest_is_npe() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1]);
        let result = system_arraycopy(&mut vm, src, 0, Reference::null(), 0, 0);
        assert_exc_class(&vm, result, "java/lang/NullPointerException");
    }

    #[test]
    fn non_array_is_arraystore() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let obj = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("X".into(), vec![])));
        let dst = int_array(&mut vm, &[0; 1]);
        let result = system_arraycopy(&mut vm, obj, 0, dst, 0, 1);
        assert_exc_class(&vm, result, "java/lang/ArrayStoreException");
    }

    #[test]
    fn primitive_type_mismatch_is_arraystore() {
        // int[] → byte[]:基本但不同型 → ASE。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1, 2]);
        let dst = array_of(&mut vm, "[B", vec![Slot::Int(0); 2]);
        let result = system_arraycopy(&mut vm, src, 0, dst, 0, 2);
        assert_exc_class(&vm, result, "java/lang/ArrayStoreException");
    }

    #[test]
    fn ref_to_primitive_is_arraystore() {
        // Object[] → int[]:引用 src、基本 dst → ASE。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = array_of(
            &mut vm,
            "[Ljava/lang/Object;",
            vec![Slot::Reference(Reference::null()); 1],
        );
        let dst = int_array(&mut vm, &[0]);
        let result = system_arraycopy(&mut vm, src, 0, dst, 0, 1);
        assert_exc_class(&vm, result, "java/lang/ArrayStoreException");
    }

    #[test]
    fn negative_srcpos_is_aioobe() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1]);
        let dst = int_array(&mut vm, &[0]);
        let result = system_arraycopy(&mut vm, src, -1, dst, 0, 1);
        assert_exc_class(&vm, result, "java/lang/ArrayIndexOutOfBoundsException");
    }

    #[test]
    fn negative_length_is_aioobe() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1]);
        let dst = int_array(&mut vm, &[0]);
        let result = system_arraycopy(&mut vm, src, 0, dst, 0, -1);
        assert_exc_class(&vm, result, "java/lang/ArrayIndexOutOfBoundsException");
    }

    #[test]
    fn out_of_bounds_is_aioobe() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1, 2]);
        let dst = int_array(&mut vm, &[0; 2]);
        // srcPos+length(1+2=3) > srcLen(2) → AIOOBE。
        let result = system_arraycopy(&mut vm, src, 1, dst, 0, 2);
        assert_exc_class(&vm, result, "java/lang/ArrayIndexOutOfBoundsException");
    }

    #[test]
    fn length_zero_is_noop() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[1, 2]);
        let dst = int_array(&mut vm, &[9, 9]);
        // 边界点 srcPos==len、length==0:合法空拷(typeArrayKlass:293 注释)。
        system_arraycopy(&mut vm, src, 2, dst, 0, 0).unwrap();
        assert_eq!(read_ints(&vm, dst, 2), vec![9, 9]);
    }

    #[test]
    fn same_type_primitive_copy() {
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let src = int_array(&mut vm, &[10, 20, 30]);
        let dst = int_array(&mut vm, &[0; 3]);
        system_arraycopy(&mut vm, src, 0, dst, 0, 3).unwrap();
        assert_eq!(read_ints(&vm, dst, 3), vec![10, 20, 30]);
        // 源未变。
        assert_eq!(read_ints(&vm, src, 3), vec![10, 20, 30]);
    }

    #[test]
    fn overlap_forward_shift_left() {
        // 同数组、dst_pos<src_pos:前向。[1,2,3,4] 从 1 拷 3 到 0 → [1,2,3,4]的[0..3]=[2,3,4]。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let a = int_array(&mut vm, &[1, 2, 3, 4]);
        system_arraycopy(&mut vm, a, 1, a, 0, 3).unwrap();
        assert_eq!(read_ints(&vm, a, 4), vec![2, 3, 4, 4]);
    }

    #[test]
    fn overlap_backward_shift_right() {
        // 同数组、dst_pos>src_pos:后向(memmove)。[1,2,3,4] 从 0 拷 3 到 1 → [1,1,2,3]。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let a = int_array(&mut vm, &[1, 2, 3, 4]);
        system_arraycopy(&mut vm, a, 0, a, 1, 3).unwrap();
        assert_eq!(read_ints(&vm, a, 4), vec![1, 1, 2, 3]);
    }

    #[test]
    fn ref_bulk_subtype_copy() {
        // String[] → Object[]:src 组件子类型 dst(Object)→ 量体,无 checkcast。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let s0 = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("java/lang/String".into(), vec![])));
        let s1 = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("java/lang/String".into(), vec![])));
        let src = array_of(
            &mut vm,
            "[Ljava/lang/String;",
            vec![Slot::Reference(s0), Slot::Reference(s1)],
        );
        let dst = array_of(
            &mut vm,
            "[Ljava/lang/Object;",
            vec![Slot::Reference(Reference::null()); 2],
        );
        system_arraycopy(&mut vm, src, 0, dst, 0, 2).unwrap();
        assert_eq!(read_ref(&vm, dst, 0), s0);
        assert_eq!(read_ref(&vm, dst, 1), s1);
    }

    #[test]
    fn ref_checkcast_partial_then_arraystore() {
        // Object[]{String, Thread} → String[]:Object 非 String 子类型 → checkcast。
        // String 可赋(同型短路)→ 拷;Thread 不可赋 → ASE(dst[0] 已写、dst[1] 未变)。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let s = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("java/lang/String".into(), vec![])));
        let t = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("java/lang/Thread".into(), vec![])));
        let src = array_of(
            &mut vm,
            "[Ljava/lang/Object;",
            vec![Slot::Reference(s), Slot::Reference(t)],
        );
        let dst = array_of(
            &mut vm,
            "[Ljava/lang/String;",
            vec![Slot::Reference(Reference::null()); 2],
        );
        let err = system_arraycopy(&mut vm, src, 0, dst, 0, 2).unwrap_err();
        let VmError::ThrownException(r) = err else {
            panic!("期望 ThrownException,得 {err:?}");
        };
        let Some(Oop::Instance(i)) = vm.heap().get(r) else {
            panic!("ASE 应为 Instance");
        };
        assert_eq!(i.class_name(), "java/lang/ArrayStoreException");
        // 前缀已拷(String 进 dst[0]);不可赋位(dst[1])未写(仍 null)。
        assert_eq!(read_ref(&vm, dst, 0), s);
        assert!(read_ref(&vm, dst, 1).is_null());
    }

    #[test]
    fn same_ref_array_is_memmove_no_checkcast() {
        // 同一 Object[] 自拷(含非 String 元素):src==dst → 免 checkcast,正常 memmove。
        let reg = ClassRegistry::new();
        let mut vm = Vm::new(&reg);
        let t = vm
            .heap_mut()
            .alloc(Oop::Instance(InstanceOop::new("java/lang/Thread".into(), vec![])));
        let a = array_of(
            &mut vm,
            "[Ljava/lang/Object;",
            vec![
                Slot::Reference(t),
                Slot::Reference(Reference::null()),
                Slot::Reference(Reference::null()),
            ],
        );
        // [t, null, null] 从 0 拷 1 到 2 → [t, null, t](后向,dst_pos>src_pos)。
        system_arraycopy(&mut vm, a, 0, a, 2, 1).unwrap();
        assert_eq!(read_ref(&vm, a, 0), t);
        assert!(read_ref(&vm, a, 1).is_null());
        assert_eq!(read_ref(&vm, a, 2), t);
    }
}
