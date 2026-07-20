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
