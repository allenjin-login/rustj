//! rustj — HotSpot 虚拟机向 Rust 迁移。
//!
//! 第一层:类文件解析 + 常量池 + 元数据。
//! 对应 HotSpot 源码 `src/hotspot/share/classfile/`、`src/hotspot/share/oops/`。
//!
//! 全 crate 默认禁止 unsafe;仅在后续 JIT/内存层确有必要时,
//! 才在具体条目上用 `#[allow(unsafe_code)]` 显式开窗。

// 默认拒绝 unsafe。允许在确有必要的具体 item 上用 allow 覆盖。
#![deny(unsafe_code)]

pub mod bytecode;
pub mod classfile;
pub mod constant_pool;
pub mod metadata;
pub mod runtime;
