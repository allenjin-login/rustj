//! 类加载基础设施:容器(jar/jmod)读取 + DEFLATE 解压 + 惰性 ClassLoader。
//!
//! 对应 HotSpot `classfile/classLoader.cpp`(`ClassPathZipEntry` / `ClassPathImageEntry` /
//! `ClassLoader::load_class`)。运行期 `lib/modules` 的 **jimage**(自定义完美哈希格式)顺延,
//! 本层先支持 zip 容器(jar / jmod)。
//!
//! 子模块:
//! - [`inflate`] — DEFLATE 解压(RFC 1951)。HotSpot 经 vendored zlib(`libzip/zlib/inflate.c`)
//!   解 zip 内 DEFLATE 条目;rustj 不引依赖,手移植 RFC 1951 解压器(纯 safe Rust,零 unsafe),
//!   算法依 RFC 1951,zlib 的 `contrib/puff/puff.c` 为参考实现。
//! - [`zip`] — zip 中心目录 + 条目读取(STORED/DEFLATED;含 jmod 前缀 base 偏移修正)。
//! - [`class_path`] — 类路径(容器列表)+ 按需 `load_class`(真 ClassLoader 雏形)。
//! - [`loader`] — 传递闭包加载器:从 ClassPath 按引用闭包 BFS 预载入 ClassRegistry
//!   (注册表为不可变借用,「惰性」= 构造 Vm 前急切预载)。

pub mod class_path;
pub mod inflate;
pub mod loader;
pub mod zip;
