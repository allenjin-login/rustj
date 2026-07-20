//! 集成测试公用基础设施(守 VM 不用的 javac 编译/jmod 探测/run/守卫/断言)。
//!
//! feature 门控:仅 `cargo test`(经 dev-deps 自引用开 `testkit` feature)或
//! `--features testkit` 时编译;`cargo build`/release 不带。VM 运行时不依赖本模块。
//!
//! 用法(`tests/*.rs`):`use rustj::testkit::*;` 引入函数;宏经 `#[macro_export]`
//! 在 crate 根,`use rustj::testkit::*;` 亦经下方 `pub use` 引入。

pub fn probe() -> bool {
    true
}

#[macro_export]
macro_rules! probe_macro {
    () => {
        42
    };
}

pub use crate::probe_macro;
