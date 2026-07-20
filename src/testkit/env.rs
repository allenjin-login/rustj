//! 环境探测:javac 可用性 + 本机 `java.base.jmod` 定位;以及跳过守卫宏。
//!
//! 提取自 `tests/` 64 文件各自 copy-paste 的 `javac_available`(61 处)与
//! `find_javabase_jmod`(46 处),逐字一致。守卫宏封装 early-return 跳过模式
//! (函数做不到 early-return,故用宏)。

use std::path::{Path, PathBuf};
use std::process::Command;

/// `javac` 是否在 PATH 且可执行(`javac -version` 退出码 0)。
pub fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// 找本机首个 `java.base.jmod`;无则 `None`。
///
/// 扫 `C:/Program Files/Java/{jdk-25.0.2,jdk-24,jdk-21,jdk-17,jdk-11.0.30}/jmods/java.base.jmod`,
/// 再回退 `JAVA_HOME/jmods/java.base.jmod`。
pub fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java")
            .join(ver)
            .join("jmods/java.base.jmod");
        if p.exists() {
            return Some(p);
        }
    }
    std::env::var("JAVA_HOME")
        .ok()
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

/// 跳过守卫:无 `javac` 时 `eprintln` 提示并 early-return。
/// 仅可用于返回 `()` 的函数(如 `#[test] fn`)。
#[macro_export]
macro_rules! require_javac {
    () => {
        if !$crate::testkit::javac_available() {
            eprintln!("跳过:无 javac");
            return;
        }
    };
}

/// 跳过守卫:无 `java.base.jmod` 时 `eprintln` 提示并 early-return;
/// 命中则把路径绑到 `$var`。仅可用于返回 `()` 的函数。
#[macro_export]
macro_rules! require_javabase {
    ($var:ident) => {
        let Some($var) = $crate::testkit::find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
    };
}
