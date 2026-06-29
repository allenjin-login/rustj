//! Class 镜像对象(对应 HotSpot 原语类型 / 类型的 `java.lang.Class` 实例)。
//!
//! Layer 4.10g:`Class.getPrimitiveClass(name)` 的返回值载体。包装类 `<clinit>`
//! 用它设各自的 `TYPE` 静态字段(如 `Integer.TYPE = Class.getPrimitiveClass("int")`)。
//!
//! **不合成 `java/lang/Class` 类桩**——对 Class 调方法 / `instanceof` / `checkcast`
//! 的完整语义顺延到"加载真实 Class 类"层。本层只承诺:Class oop 可存进静态字段、
//! `checkcast` 到 `java/lang/Class` 能命中(见 [`type_check`](crate::runtime::interpreter::type_check))。
//!
//! **Step 0 源码依据**:`hotspot/share/prims/jvm.cpp:770` `JVM_FindPrimitiveClass`
//! → `Universe::java_mirror(t)`:每个原语类型(`int`/…/`void`)有唯一 Class 镜像。

/// Class 镜像:所表示的类型名(原语关键字如 `"int"`,或内部类名)。
#[derive(Debug, Clone, PartialEq)]
pub struct ClassOop {
    name: String,
}

impl ClassOop {
    /// 由类型名构造。
    pub(crate) fn new(name: String) -> Self {
        Self { name }
    }

    /// 所表示的类型名。
    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_name_round_trip() {
        let c = ClassOop::new("int".into());
        assert_eq!(c.name(), "int");
    }

    #[test]
    fn eq_by_name() {
        assert_eq!(ClassOop::new("int".into()), ClassOop::new("int".into()));
        assert_ne!(ClassOop::new("int".into()), ClassOop::new("long".into()));
    }
}
