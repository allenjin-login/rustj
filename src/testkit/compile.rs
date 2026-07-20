//! javac 编译 + .class 加载辅助(集成测试用)。
//!
//! 提取自 `tests/` 64 文件 copy-paste 的编译/加载样板(61 文件)。模块内 `static SEQ`
//! 内化(消除 53 处各文件 SEQ/COMPILE_SEQ),目录名统一 `rustj-test-{name}-{seq}-{pid}`
//! (消除 46 种历史前缀)。编译失败 panic(带 javac stderr)。

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::classfile::parse;
use crate::oops::ClassRegistry;

/// 全局编译目录序号(模块内单点,消除各文件 SEQ/COMPILE_SEQ)。
static SEQ: AtomicU64 = AtomicU64::new(0);

/// 构造唯一临时目录路径 `rustj-test-{name}-{seq}-{pid}`。
fn unique_dir(name: &str) -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rustj-test-{name}-{seq}-{pid}",
        pid = std::process::id()
    ))
}

/// javac 编 `src`(顶层类 `name`)到唯一目录,返回该目录。`extra` 追加 javac 参数
/// (如 `--add-exports`)。编译失败 panic(带 stderr)。
fn javac_to_dir(src: &str, name: &str, extra: &[&str]) -> PathBuf {
    let dir = unique_dir(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let src_path = dir.join(format!("{name}.java"));
    fs::write(&src_path, src).unwrap();
    let out = Command::new("javac")
        .args(extra)
        .arg("-d")
        .arg(&dir)
        .arg(&src_path)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

/// 编单顶层类 `name` → 返回 `{name}.class` 文件路径(不载入 registry)。
/// 供仅需 parse 字节码、不运行的编译测试(interpret_int_methods / parse_real_class 等)。
/// **不清理目录**(返回路径指向目录内文件;调用方读后自负,temp 目录 OS 终会回收)。
pub fn compile(src: &str, name: &str) -> PathBuf {
    javac_to_dir(src, name, &[]).join(format!("{name}.class"))
}

/// 编 `src` 到唯一目录,返回目录。支持 `extra` javac 参数(如 `--add-exports`)。
/// 供多类 / add-exports / 自定义加载(如 `load_closure`)场景。**调用方拥有目录**(不清理)。
pub fn compile_dir(src: &str, name: &str, extra: &[&str]) -> PathBuf {
    javac_to_dir(src, name, extra)
}

/// 把 `dir` 下所有 `.class` 载入 `reg`(遍历目录,逐个 parse + load)。
pub fn load_dir(reg: &mut ClassRegistry, dir: &std::path::Path) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) == Some("class") {
            reg.load(parse(&fs::read(&p).unwrap()).expect("解析应成功"))
                .expect("加载应成功");
        }
    }
}

/// 编 `src`(可含多类)+ 把生成的所有 `.class` 载入**新** `ClassRegistry`,返回之。编译后清理目录。
/// 供绝大多数"编译并运行"测试(throw/clinit/arrays 等)。
pub fn compile_and_load(src: &str, name: &str) -> ClassRegistry {
    let dir = javac_to_dir(src, name, &[]);
    let mut reg = ClassRegistry::new();
    load_dir(&mut reg, &dir);
    let _ = fs::remove_dir_all(&dir);
    reg
}
