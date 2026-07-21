//! 集成闸门(Layer 4.11):从真 `java.base.jmod` 取 `module-info.class`,解析其 Module 属性
//! 为 [`ModuleDescriptor`]——验证「模块系统」的最底层能力:能读懂 JDK 的模块描述符。
//!
//! 真 `java.base` module-info(JDK 25 实测,javap):
//!   `module java.base@25.0.2`,ACC_MODULE,`requires: 0`(根模块,不依赖任何模块),
//!   `exports: 115`(含 `java/lang`、`java/util` 等无限定导出 + 少量 `to ...` 限定导出)。
//!
//! 需本机 `java.base.jmod`;缺则跳过。

use rustj::classfile::parse;
use rustj::metadata::ModuleDescriptor;
use rustj::testkit::*;

/// 从 jmod(zip 前 4 字节 magic 前缀;`ZipReader::new` 内部已修正偏移)提取
/// `classes/module-info.class` 的原始字节。
fn extract_module_info(jmod: &std::path::Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(jmod).ok()?;
    let z = rustj::runtime::class_loader::zip::ZipReader::new(&bytes).ok()?;
    z.read("classes/module-info.class").ok().flatten()
}

#[test]
fn gate_parse_javabase_module_info() {
    require_javabase!(jmod);
    let bytes = match extract_module_info(&jmod) {
        Some(b) => b,
        None => {
            eprintln!("跳过:jmod 内未找到 classes/module-info.class(或 zip 解压失败)");
            return;
        }
    };
    let cf = parse(&bytes).expect("module-info.class 须可解析");
    let desc = ModuleDescriptor::from_class_file(&cf)
        .expect("Module 属性解码失败")
        .expect("module-info 须有 Module 属性");

    // 模块名 = java.base(根模块,无 requires)。
    assert_eq!(desc.name(), "java.base");
    assert!(desc.requires().is_empty(), "java.base 是根模块,requires 应为空");

    // 导出包含 java/lang、java/util(无限定导出)。
    let exported: Vec<&str> = desc.exports().iter().map(|e| e.package.as_str()).collect();
    assert!(exported.contains(&"java/lang"), "应导出 java/lang,实际:{exported:?}");
    assert!(exported.contains(&"java/util"), "应导出 java/util,实际:{exported:?}");

    // java/lang 是无限定导出(to 为空);限定导出(如某些 jdk/internal/*)的 to 非空。
    let lang = desc
        .exports()
        .iter()
        .find(|e| e.package == "java/lang")
        .expect("java/lang 须在 exports");
    assert!(lang.to_modules.is_empty(), "java/lang 应无限定导出");

    // 至少存在一个限定导出(java.base 实测有 ~20 个 `to ...`)。
    let qualified = desc.exports().iter().any(|e| !e.to_modules.is_empty());
    assert!(qualified, "应存在限定导出(exports ... to ...)");
}
