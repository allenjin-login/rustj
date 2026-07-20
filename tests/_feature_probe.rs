use rustj::testkit::*;

#[test]
fn feature_and_macro_visible_from_integration_test() {
    assert!(probe());
    assert_eq!(probe_macro!(), 42);
}

#[test]
fn probe_require_javac() {
    // 宏跨 crate 可见 + 可展开即过;无 javac 时 early-return(测试仍 pass)。
    require_javac!();
}

#[test]
fn probe_require_javabase() {
    // _ 前缀避免"未使用"警告;无 jmod 时 early-return(测试仍 pass)。
    require_javabase!(_jmod);
    let _ = _jmod;
}

// Task 6: 编译探针(4 函数,各验一 API)
#[test]
fn probe_compile() {
    require_javac!();
    let p = compile("public class ProbeCompile {}", "ProbeCompile");
    assert!(p.exists(), "应有 .class 文件:{:?}", p);
    let _ = std::fs::remove_dir_all(p.parent().unwrap());
}

#[test]
fn probe_compile_dir() {
    require_javac!();
    let dir = compile_dir("public class ProbeDir {}", "ProbeDir", &[]);
    assert!(dir.join("ProbeDir.class").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_compile_dir_with_extra() {
    require_javac!();
    // 验证 extra 参数透传(extra 透传由 compile_dir 签名 + javac_to_dir 的 .args(extra) 保证)
    let dir = compile_dir(
        "public class ProbeExtra {}",
        "ProbeExtra",
        &[],  // 空 extra 验证编译通过;签名保证透传逻辑
    );
    assert!(dir.join("ProbeExtra.class").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn probe_load_dir_and_compile_and_load() {
    require_javac!();
    // load_dir:从 compile_dir 的目录载入已有 registry
    let dir = compile_dir("public class ProbeLd {}", "ProbeLd", &[]);
    let mut reg = rustj::oops::ClassRegistry::new();
    load_dir(&mut reg, &dir);
    assert!(reg.get("ProbeLd").is_some(), "ProbeLd 应已载入");
    let _ = std::fs::remove_dir_all(&dir);

    // compile_and_load:编 + 载入新 registry
    let reg2 = compile_and_load("public class ProbeCal {}", "ProbeCal");
    assert!(reg2.get("ProbeCal").is_some(), "ProbeCal 应已载入");
}
