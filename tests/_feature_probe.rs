use rustj::testkit::*;

#[test]
fn feature_and_macro_visible_from_integration_test() {
    assert!(probe());
    assert_eq!(probe_macro!(), 42);
}
