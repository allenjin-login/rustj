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

pub mod inflate;
