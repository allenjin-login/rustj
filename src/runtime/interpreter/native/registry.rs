//! Native 方法 fn 指针注册表(Layer 4.17):替代 4.10c 的编译期 `match`。
//! 两层 `HashMap<String, Vec<NativeEntry>>`:外层类名键经 `Borrow<str>` 零分配按 `&str` 查,
//! 内层 Vec 线性扫 name+desc(每类 native 个位数,cache 友好)。对应 HotSpot 每 `Method` 的
//! `native_function` 字段(`method.hpp:441-447`),rustj 集中成单表(per-Method 缓存顺延)。
//! `register` upsert(同键覆盖,镜像 `Method::set_native_function`);为 4.16 RegisterNatives 预留。

use std::collections::HashMap;

use crate::runtime::{Reference, Value, VmError, VmThread};

/// 单个 native 方法的实现指针。删掉 4.10c 签名里的 class/name/desc——那三者只用于分派查表,
/// native 体不需要。非捕获闭包在 `register(..., f: NativeFn)` 位自动协变为本类型(零成本)。
pub(crate) type NativeFn = fn(&mut VmThread, Option<Reference>, &[Value]) -> Result<Value, VmError>;

pub(crate) struct NativeRegistry {
    by_class: HashMap<String, Vec<NativeEntry>>,
}

struct NativeEntry {
    name: String,
    desc: String,
    f: NativeFn,
}

impl NativeRegistry {
    pub(crate) fn new() -> Self {
        Self { by_class: HashMap::new() }
    }

    /// 登记一个 native。**upsert**:同 (class,name,desc) 已存在 → 覆盖 fn;否则 push。
    /// 对应 HotSpot `Method::set_native_function`(`method.cpp:1024-1044`):同 fn 幂等、不同 fn 覆盖。
    /// 静态注册期无重键 → 零副作用;将来 4.16 `JNI_RegisterNatives` 覆盖注册直接复用。
    pub(crate) fn register(&mut self, class: &str, name: &str, desc: &str, f: NativeFn) {
        let v = self.by_class.entry(class.to_string()).or_default();
        if let Some(e) = v.iter_mut().find(|e| e.name == name && e.desc == desc) {
            e.f = f;
        } else {
            v.push(NativeEntry { name: name.to_string(), desc: desc.to_string(), f });
        }
    }

    /// 零分配查表:外层 `String` 键经 `Borrow<str>` 按 `&str` 查;内层 Vec 线性扫 name+desc。
    /// fn 指针 `Copy`,返 owned `Option<NativeFn>`,调用方释锁后再调(不在持锁态调 native 体)。
    pub(crate) fn resolve(&self, class: &str, name: &str, desc: &str) -> Option<NativeFn> {
        self.by_class
            .get(class)?
            .iter()
            .find(|e| e.name == name && e.desc == desc)
            .map(|e| e.f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(_vm: &mut VmThread, _this: Option<Reference>, _args: &[Value]) -> Result<Value, VmError> {
        Ok(Value::Int(1))
    }
    fn other(_vm: &mut VmThread, _this: Option<Reference>, _args: &[Value]) -> Result<Value, VmError> {
        Ok(Value::Int(2))
    }

    #[test]
    fn resolve_miss_returns_none() {
        let reg = NativeRegistry::new();
        assert!(reg.resolve("java/lang/Foo", "bar", "()V").is_none());
    }

    /// 行为验证(免 fn 指针地址比较告警):resolve 回的 fn 调用后产登记时的返回值。
    #[test]
    fn register_then_resolve_roundtrip() {
        let mut reg = NativeRegistry::new();
        reg.register("java/lang/Object", "hashCode", "()I", dummy);
        let f = reg.resolve("java/lang/Object", "hashCode", "()I").expect("应命中");
        let mut vm = VmThread::default();
        assert_eq!(f(&mut vm, None, &[]).unwrap(), Value::Int(1));
    }

    /// upsert:同键再 register 另一 fn → resolve 回的须是新 fn(调之产 Int(2))。
    #[test]
    fn register_upsert_overwrites_same_key() {
        let mut reg = NativeRegistry::new();
        reg.register("java/lang/Object", "hashCode", "()I", dummy);
        reg.register("java/lang/Object", "hashCode", "()I", other);
        let f = reg.resolve("java/lang/Object", "hashCode", "()I").expect("应命中");
        let mut vm = VmThread::default();
        assert_eq!(f(&mut vm, None, &[]).unwrap(), Value::Int(2), "upsert 后须返后者");
    }

    #[test]
    fn resolve_distinct_methods_in_same_class() {
        let mut reg = NativeRegistry::new();
        reg.register("java/lang/Object", "hashCode", "()I", dummy);
        reg.register("java/lang/Object", "getClass", "()Ljava/lang/Class;", other);
        let mut vm = VmThread::default();
        assert_eq!(
            reg.resolve("java/lang/Object", "hashCode", "()I").unwrap()(&mut vm, None, &[]).unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            reg.resolve("java/lang/Object", "getClass", "()Ljava/lang/Class;")
                .unwrap()(&mut vm, None, &[])
                .unwrap(),
            Value::Int(2)
        );
    }
}
