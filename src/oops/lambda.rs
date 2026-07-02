//! Lambda 闭包对象(对应 HotSpot 经 `InnerClassLambdaMetafactory` 生成的合成类实例)。
//!
//! Layer 4.10aa:`invokedynamic` 引导方法 `LambdaMetafactory.metafactory` 的综合产物。
//! 真实 HotSpot 运行时**生成**实现 SAM 的合成类;rustj 沿「按语义移植」(同 4.10u
//! makeConcat / native 表特判 JVM_*),**不生成类**——闭包 Oop 记实现方法身份 + 捕获,
//! SAM 调用经 [`crate::runtime::interpreter::invoke`] 转发到实现方法体。
//!
//! **Step 0 源码依据**:`java.base/java/lang/invoke/LambdaMetafactory.java:339`
//! `metafactory(...)` 的实现实参(`bootstrap_arguments[1]`)为 lambda 体的 `MethodHandle`。

use crate::runtime::Value;

/// `CONSTANT_MethodHandle` 的 reference_kind(JVMS §4.4.8)。仅本层用 `InvokeStatic`。
pub(crate) const REF_INVOKE_STATIC: u8 = 6;

/// Lambda 闭包:实现方法身份 + 捕获,供 SAM 调用派发。
///
/// - `impl_*`:lambda 体(`lambda$<caller>$0`)或方法引用的 (类, 名, 描述符);描述符含
///   捕获形参在前、SAM 形参在后(javac 把 lambda 体编为 `private static`,实例捕获把
///   `this` 作显式捕获前置)。
/// - `impl_kind`:`MethodHandle` reference_kind;本层仅派发 `REF_INVOKE_STATIC`(6)。
/// - `sam_type`:函数式接口内部名(factoryType 返回),供未来 `instanceof`/`checkcast`。
/// - `captures`:按捕获类型序的值(SAM 派发时前置到 SAM 实参)。
#[derive(Debug, Clone, PartialEq)]
pub struct LambdaOop {
    impl_class: String,
    impl_name: String,
    impl_desc: String,
    impl_kind: u8,
    sam_type: String,
    captures: Vec<Value>,
}

impl LambdaOop {
    /// 由实现方法身份、SAM 类型与捕获构造。
    pub(crate) fn new(
        impl_class: String,
        impl_name: String,
        impl_desc: String,
        impl_kind: u8,
        sam_type: String,
        captures: Vec<Value>,
    ) -> Self {
        Self {
            impl_class,
            impl_name,
            impl_desc,
            impl_kind,
            sam_type,
            captures,
        }
    }

    /// 实现方法声明类(内部名)。
    pub fn impl_class(&self) -> &str {
        &self.impl_class
    }

    /// 实现方法名。
    pub fn impl_name(&self) -> &str {
        &self.impl_name
    }

    /// 实现方法描述符(捕获形参在前、SAM 形参在后)。
    pub fn impl_desc(&self) -> &str {
        &self.impl_desc
    }

    /// `MethodHandle` reference_kind。
    pub fn impl_kind(&self) -> u8 {
        self.impl_kind
    }

    /// 函数式接口内部名。
    pub fn sam_type(&self) -> &str {
        &self.sam_type
    }

    /// 按捕获类型序的值。
    pub fn captures(&self) -> &[Value] {
        &self.captures
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Reference;

    #[test]
    fn accessors_round_trip() {
        let l = LambdaOop::new(
            "LamProbe".into(),
            "lambda$run$0".into(),
            "(II)I".into(),
            REF_INVOKE_STATIC,
            "java/util/function/IntUnaryOperator".into(),
            vec![Value::Int(10), Value::Reference(Reference::null())],
        );
        assert_eq!(l.impl_class(), "LamProbe");
        assert_eq!(l.impl_name(), "lambda$run$0");
        assert_eq!(l.impl_desc(), "(II)I");
        assert_eq!(l.impl_kind(), REF_INVOKE_STATIC);
        assert_eq!(l.sam_type(), "java/util/function/IntUnaryOperator");
        assert_eq!(l.captures().len(), 2);
    }

    #[test]
    fn eq_by_fields() {
        let a = LambdaOop::new("C".into(), "m".into(), "()V".into(), 6, "F".into(), vec![]);
        let b = LambdaOop::new("C".into(), "m".into(), "()V".into(), 6, "F".into(), vec![]);
        assert_eq!(a, b);
    }
}
