//! 真 `java/lang/String` 实例的构建与读回(对应 HotSpot `java_lang_String` 的就地构造 /
//! 文本解码)。Layer 4.10i:退役 `Oop::String` 特殊变体后,字符串字面量(`ldc`/`ldc_w`)
//! 与 `String.intern()` 经本模块在堆上构造**真** `java/lang/String` 实例(紧凑串布局:
//! `value: byte[]` + `coder: byte`,String.java:188/202),其方法(`equals`/`hashCode`/
//! `length`/…)跑真字节码(分派到 `StringLatin1`/`StringUTF16`)。
//!
//! **原始类约定:** `intern` 须注册表含已加载的真 `java/lang/String`(对应真实 JVM 启动期
//! 即载的原始类);首用经 `clinit::ensure_class_initialized` 触发其 `<clinit>`
//! (String.java:259,仅 `COMPACT_STRINGS = true`,无 native 级联)。

use crate::metadata::descriptor::FieldType;
use crate::oops::{ArrayOop, InstanceOop, Oop};
use crate::runtime::{Reference, Slot, Vm};

use super::clinit;
use super::VmError;

/// `java/lang/String` 的内部名。
const STRING: &str = "java/lang/String";

/// 返回 `text` 的 intern 引用(同文本恒同引用)。池命中则复用;否则构造真 String 实例
/// 并登记。`ldc`/`ldc_w` 与 `String.intern()` native 经此。
///
/// `pub(crate)`:供 `Vm` 在模块镜像(`Vm::intern_named_module` 置 `Module.name`)等场景
/// 构造真 String 实例(同 native 字符串 native 的复用)。
pub(crate) fn intern(vm: &mut Vm, text: &str) -> Result<Reference, VmError> {
    if let Some(&r) = vm.string_pool().get(text) {
        return Ok(r);
    }
    let r = build(vm, text)?;
    vm.string_pool_mut().insert(text.to_string(), r);
    Ok(r)
}

/// 在堆上构造一个真 `java/lang/String` 实例:`value` 编码为 Latin1(全码元 ≤ 0xFF)或
/// UTF-16(大端),设 `value`/`coder` 字段;`hash`/`hashIsZero` 取默认 0/false(与 Java 一致)。
fn build(vm: &mut Vm, text: &str) -> Result<Reference, VmError> {
    clinit::ensure_class_initialized(vm, STRING)?;
    let (bytes, coder) = encode(text);

    // 解析字段序号 + 造默认实例;块内仅借注册表('a,不绑 &self),出块后 inst/序号为 owned,
    // 故可再 `&mut vm` 写堆。
    let value_ft = FieldType::Array(Box::new(FieldType::Byte));
    let coder_ft = FieldType::Byte;
    let (inst, value_ord, coder_ord) = {
        let registry = vm
            .registry()
            .ok_or(VmError::BadConstant("构造 String 需类注册表"))?;
        let lc = registry
            .get(STRING)
            .ok_or(VmError::BadConstant("java/lang/String 未加载(原始类须预载)"))?;
        let value_ord = registry
            .instance_field(&lc, "value", &value_ft)
            .ok_or(VmError::BadConstant("String.value 字段未找到"))?;
        let coder_ord = registry
            .instance_field(&lc, "coder", &coder_ft)
            .ok_or(VmError::BadConstant("String.coder 字段未找到"))?;
        (registry.new_instance(&lc), value_ord, coder_ord)
    };

    // value: byte[](描述符 `[B`;每字节以有符号 int 入槽;baload 的 (v as i8) as i32 会归一,
    // 与存有符号/无符号等价)。`[B` 描述符供真 String.hashCode 的 (byte[]) array → checkcast [B。
    let elems: Vec<Slot> = bytes
        .iter()
        .map(|&b| Slot::Int((b as i8) as i32))
        .collect();
    let value_ref = vm
        .heap_mut()
        .alloc(Oop::Array(ArrayOop::new("[B".to_string(), elems)));

    let mut inst: InstanceOop = inst;
    inst.set_field(value_ord, Slot::Reference(value_ref));
    inst.set_field(coder_ord, Slot::Int(coder as i32));
    Ok(vm.heap_mut().alloc(Oop::Instance(inst)))
}

/// 读回 `r` 所指 String 实例的文本;非 String 实例 / 悬空 / 损坏 → `None`。
/// 供 `Class.getPrimitiveClass` 取原语名、`String.intern()` native 取文本键,以及
/// `vm::threads`(读未捕获异常默认路径的线程名)。`pub(crate)`:vm 模块非 interpreter 后代。
pub(crate) fn read_text(vm: &Vm, r: Reference) -> Result<Option<String>, VmError> {
    let value_ft = FieldType::Array(Box::new(FieldType::Byte));
    let coder_ft = FieldType::Byte;

    // inst 取 owned(clone):其后须再锁 heap 读 value 数组,持 guard 重锁会自死锁(B.2.3b)。
    let inst = match vm.heap().get(r).cloned() {
        Some(Oop::Instance(i)) if i.class_name() == STRING => i,
        _ => return Ok(None),
    };

    let (value_ord, coder_ord) = {
        let registry = vm
            .registry()
            .ok_or(VmError::BadConstant("读 String 文本需类注册表"))?;
        let lc = registry
            .get(STRING)
            .ok_or(VmError::BadConstant("java/lang/String 未加载"))?;
        let v = registry
            .instance_field(&lc, "value", &value_ft)
            .ok_or(VmError::BadConstant("String.value 字段未找到"))?;
        let c = registry
            .instance_field(&lc, "coder", &coder_ft)
            .ok_or(VmError::BadConstant("String.coder 字段未找到"))?;
        (v, c)
    };

    let value_ref = match inst.field(value_ord) {
        Slot::Reference(r) => r,
        _ => return Ok(None),
    };
    let coder = match inst.field(coder_ord) {
        Slot::Int(c) => c as u8,
        _ => return Ok(None),
    };
    let arr = match vm.heap().get(value_ref).cloned() {
        Some(Oop::Array(a)) => a,
        _ => return Ok(None),
    };
    let raw: Vec<u8> = (0..arr.length())
        .map(|i| match arr.element(i) {
            Slot::Int(v) => (v as i8) as u8,
            _ => 0,
        })
        .collect();
    let text = if coder == 0 {
        // Latin1:每字节 → 码点(0..=255)。
        raw.iter().map(|&b| char::from(b)).collect()
    } else {
        // UTF-16:大端字节对 → 码元 → String(容错解码)。
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| ((c[0] as u16) << 8) | (c[1] as u16))
            .collect();
        String::from_utf16_lossy(&units)
    };
    Ok(Some(text))
}

/// 把文本编码为 String 的 value 字节与 coder(Latin1=0 / UTF-16=1)。
/// 按 UTF-16 码元(`encode_utf16`)对齐 Java 的按代码单元布局(辅助平面字符=代理对两码元)。
fn encode(text: &str) -> (Vec<u8>, u8) {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.iter().all(|&u| u <= 0xFF) {
        (units.iter().map(|&u| u as u8).collect(), 0)
    } else {
        let mut bytes = Vec::with_capacity(units.len() * 2);
        for &u in &units {
            bytes.push((u >> 8) as u8);
            bytes.push((u & 0xFF) as u8);
        }
        (bytes, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_ascii_is_latin1() {
        let (bytes, coder) = encode("abc");
        assert_eq!(coder, 0);
        assert_eq!(bytes, vec![b'a', b'b', b'c']);
    }

    #[test]
    fn encode_supplementary_is_utf16() {
        // U+1D11E(𝄞)→ 代理对 D834 DD1E → 大端两码元。
        let (bytes, coder) = encode("𝄞");
        assert_eq!(coder, 1);
        assert_eq!(bytes, vec![0xD8, 0x34, 0xDD, 0x1E]);
    }

    #[test]
    fn encode_high_latin1_stays_latin1() {
        // U+00FF ≤ 0xFF → Latin1 单字节。
        let (bytes, coder) = encode("ÿ");
        assert_eq!(coder, 0);
        assert_eq!(bytes, vec![0xFF]);
    }
}
