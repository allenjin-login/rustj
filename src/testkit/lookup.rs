//! 测试侧类文件查找工具(panic 版,集成测试用)。
//!
//! 区别于 VM 内部 `cp_util::utf8`(`Result` 版,源码侧):此处 `utf8` 为 **panic 版**
//! (测试断言失败直接 panic 合理)。

use crate::constant_pool::ConstantPoolEntry;
use crate::metadata::{ClassFile, MethodInfo};

/// 取 `Utf8` 条目的字符串(owned)——**panic 版**(区别于 `cp_util::utf8` 的 `Result` 版)。
pub fn utf8(cf: &ClassFile, index: u16) -> String {
    match cf.constant_pool.get(index).unwrap() {
        ConstantPoolEntry::Utf8(s) => s.clone(),
        e => panic!("expected Utf8 at {index}, got {e:?}"),
    }
}

/// 在类中按名 + 描述符查找方法;未命中 panic。统一 `matches!` 变体。
pub fn find_method<'a>(cf: &'a ClassFile, name: &str, desc: &str) -> &'a MethodInfo {
    cf.methods
        .iter()
        .find(|m| {
            let n = matches!(
                cf.constant_pool.get(m.name_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == name
            );
            let d = matches!(
                cf.constant_pool.get(m.descriptor_index),
                Ok(ConstantPoolEntry::Utf8(s)) if s == desc
            );
            n && d
        })
        .unwrap_or_else(|| panic!("未找到方法 {name}{desc}"))
}
