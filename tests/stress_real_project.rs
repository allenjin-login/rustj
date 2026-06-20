//! 批量压力测试:递归解析某个真实 Java 项目 `target/classes` 下的**全部** `.class`。
//!
//! 默认 `#[ignore]`(依赖外部目录)。显式运行:
//!   cargo test --release --test stress_real_project -- --ignored --nocapture
//!
//! 可用环境变量 `RUSTJ_STRESS_DIR` 指定目录,默认指向 UsbThief 项目。

use std::path::PathBuf;

use rustj::classfile::parse;

fn stress_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RUSTJ_STRESS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(r"E:\IdeaProjects\UsbThief\target\classes")
}

fn collect_classes(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_classes(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("class") {
            out.push(path);
        }
    }
}

#[test]
#[ignore]
fn parses_every_class_in_real_project() {
    let dir = stress_dir();
    let mut classes = Vec::new();
    collect_classes(&dir, &mut classes);
    assert!(!classes.is_empty(), "在 {} 下未找到 .class 文件", dir.display());

    let mut ok = 0usize;
    let mut total_cp = 0usize;
    let mut total_methods = 0usize;
    let mut total_code_bytes = 0usize;
    let mut failures: Vec<(PathBuf, String)> = Vec::new();

    for path in &classes {
        match std::fs::read(path).and_then(|b| parse(&b).map_err(|e| std::io::Error::other(e.to_string()))) {
            Ok(cf) => {
                ok += 1;
                total_cp += cf.constant_pool.len();
                total_methods += cf.methods.len();
                for m in &cf.methods {
                    if let Some(c) = &m.code {
                        total_code_bytes += c.code.len();
                    }
                }
            }
            Err(e) => failures.push((path.clone(), e.to_string())),
        }
    }

    println!("解析 {}/{} 个类", ok, classes.len());
    println!("常量池条目总数: {total_cp}");
    println!("方法总数:       {total_methods}");
    println!("字节码字节数:   {total_code_bytes}");

    for (path, err) in &failures {
        eprintln!("FAIL {}: {err}", path.display());
    }
    assert!(failures.is_empty(), "{} 个类解析失败", failures.len());
}
