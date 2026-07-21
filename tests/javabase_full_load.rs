//! **里程碑集成闸门(Layer 4.32):java.base 全量结构性加载零失败。**
//!
//! /goal 子指令"完全加载 java.base"的结构性完备度度量:遍历 java.base.jmod 全部
//! `classes/*.class` 条目(7332+ 类),对每个经 `load_closure`(解析 + 注册 + 传递性闭包)
//! 加载,断言**全部成功**。修后实测 7332/7332(100%)——证明类文件解析器 + 传递性加载闭包
//! 覆盖整个 java.base 模块,无结构性缺口。
//!
//! 本闸门是**回归守卫**:确保后续层的改动不破坏 java.base 全量加载能力。
//! **注意**:本闸门仅度量"加载"(parse+register),不跑 `<clinit>`(初始化)——后者依赖
//! 各类 native,是后续层的前线。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::class_loader::zip::ZipReader;
use rustj::testkit::*;

/// **里程碑闸门**:java.base.jmod 全部类条目经 `load_closure` 加载零失败。
#[test]
fn javabase_all_classes_load_zero_failures() {
    require_javabase!(jmod);
    let bytes = std::fs::read(&jmod).unwrap();
    let zr = ZipReader::new(&bytes).expect("jmod 解析");

    // 枚举全部 classes/<X>.class 条目 → 内部名 <X>,排序保确定序。
    let mut all: Vec<String> = zr
        .names()
        .filter(|n| n.starts_with("classes/") && n.ends_with(".class"))
        .map(|n| {
            n.trim_start_matches("classes/")
                .trim_end_matches(".class")
                .to_string()
        })
        .collect();
    all.sort();
    assert!(
        all.len() > 1000,
        "java.base.jmod 类条目异常少:{}",
        all.len()
    );

    let mut registry = ClassRegistry::new();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();

    let mut failed: Vec<(String, String)> = Vec::new();
    for name in &all {
        if let Err(e) = load_closure(&mut registry, &cp, name) {
            failed.push((name.clone(), format!("{e:?}")));
        }
    }

    let ok = all.len() - failed.len();
    eprintln!(
        "java.base 加载成功:{}/{}({:.1}%)",
        ok,
        all.len(),
        100.0 * ok as f64 / all.len() as f64
    );
    for (name, err) in failed.iter().take(25) {
        eprintln!("  ❌ {name}: {err}");
    }

    // 里程碑断言:java.base 全量结构性加载零失败。
    assert!(
        failed.is_empty(),
        "java.base 有 {} 个类加载失败,首批:{:?}",
        failed.len(),
        failed.first().map(|(n, e)| (n.as_str(), e.as_str()))
    );

    // 复查:jmod 每个类条目都已注册到 registry。
    let registered = all.iter().filter(|n| registry.get(n).is_some()).count();
    assert_eq!(
        registered,
        all.len(),
        "jmod 类中仅 {registered}/{} 实际注册", all.len()
    );
}
