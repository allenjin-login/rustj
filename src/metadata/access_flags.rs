//! access_flags 位定义与访问器(JVMS §4.1 表 4.1-B / §4.5 表 4.5-A / §4.6 表 4.6-A)。

/// 类、字段、方法共用的访问标志位。
///
/// 注意:同一比特在不同上下文含义不同(例如 `0x0080` 在字段是 `TRANSIENT`,
/// 在方法是 `VARARGS`)。本类型只做原始位存储与按名称的便利谓词;
/// 上下文相关的强校验留给后续层。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessFlags(pub u16);

// ---- 公共位 ----
pub const ACC_PUBLIC: u16 = 0x0001;
pub const ACC_PRIVATE: u16 = 0x0002;
pub const ACC_PROTECTED: u16 = 0x0004;
pub const ACC_STATIC: u16 = 0x0008;
pub const ACC_FINAL: u16 = 0x0010;
// ---- 0x0020:类=SUPER(历史遗留),方法=SYNCHRONIZED ----
pub const ACC_SUPER: u16 = 0x0020;
pub const ACC_SYNCHRONIZED: u16 = 0x0020;
// ---- 0x0040:字段=VOLATILE,方法=BRIDGE ----
pub const ACC_VOLATILE: u16 = 0x0040;
pub const ACC_BRIDGE: u16 = 0x0040;
// ---- 0x0080:字段=TRANSIENT,方法=VARARGS ----
pub const ACC_TRANSIENT: u16 = 0x0080;
pub const ACC_VARARGS: u16 = 0x0080;
// ---- 方法 ----
pub const ACC_NATIVE: u16 = 0x0100;
pub const ACC_STRICT: u16 = 0x0800;
// ---- 类 ----
pub const ACC_INTERFACE: u16 = 0x0200;
pub const ACC_ABSTRACT: u16 = 0x0400; // 类/方法
pub const ACC_ANNOTATION: u16 = 0x2000;
pub const ACC_MODULE: u16 = 0x8000;
// ---- 通用 ----
pub const ACC_SYNTHETIC: u16 = 0x1000;
pub const ACC_ENUM: u16 = 0x4000; // 类/字段

impl AccessFlags {
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }
    pub const fn bits(self) -> u16 {
        self.0
    }
    pub const fn contains(self, flag: u16) -> bool {
        self.0 & flag == flag
    }

    pub const fn is_public(self) -> bool {
        self.contains(ACC_PUBLIC)
    }
    pub const fn is_private(self) -> bool {
        self.contains(ACC_PRIVATE)
    }
    pub const fn is_protected(self) -> bool {
        self.contains(ACC_PROTECTED)
    }
    pub const fn is_static(self) -> bool {
        self.contains(ACC_STATIC)
    }
    pub const fn is_final(self) -> bool {
        self.contains(ACC_FINAL)
    }
    pub const fn is_synchronized(self) -> bool {
        self.contains(ACC_SYNCHRONIZED)
    }
    pub const fn is_native(self) -> bool {
        self.contains(ACC_NATIVE)
    }
    pub const fn is_abstract(self) -> bool {
        self.contains(ACC_ABSTRACT)
    }
    pub const fn is_interface(self) -> bool {
        self.contains(ACC_INTERFACE)
    }
    pub const fn is_synthetic(self) -> bool {
        self.contains(ACC_SYNTHETIC)
    }
    pub const fn is_enum(self) -> bool {
        self.contains(ACC_ENUM)
    }
    pub const fn is_module(self) -> bool {
        self.contains(ACC_MODULE)
    }
    pub const fn is_annotation(self) -> bool {
        self.contains(ACC_ANNOTATION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_individual_flags() {
        let f = AccessFlags::from_bits(ACC_PUBLIC | ACC_STATIC | ACC_FINAL);
        assert!(f.is_public());
        assert!(f.is_static());
        assert!(f.is_final());
        assert!(!f.is_private());
    }

    #[test]
    fn raw_bits_roundtrip() {
        let f = AccessFlags::from_bits(0x0002 | 0x0008);
        assert_eq!(f.bits(), 0x000A);
        assert!(f.is_private());
        assert!(f.is_static());
    }

    #[test]
    fn default_is_zero() {
        assert_eq!(AccessFlags::default().bits(), 0);
    }
}
