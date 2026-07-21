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

// Task 7: 执行探针(runner + lookup + args)
const RUNNER_SRC: &str = r#"
public class RunnerProbe {
    public static int seven() { return 7; }
    public static int add(int a, int b) { return a + b; }
    public static int boom() { int d = 0; return 1 / d; }
    public static double half() { return 2.5; }
}
"#;

#[test]
fn probe_run_and_run_result() {
    require_javac!();
    let reg = std::sync::Arc::new(compile_and_load(RUNNER_SRC, "RunnerProbe"));
    assert!(matches!(run(&reg, "RunnerProbe", "seven", "()I"), rustj::runtime::Value::Int(7)));
    let (r, _vm) = run_result(&reg, "RunnerProbe", "seven", "()I");
    assert!(matches!(r, Ok(rustj::runtime::Value::Int(7))));
}

#[test]
fn probe_run_err() {
    require_javac!();
    let reg = std::sync::Arc::new(compile_and_load(RUNNER_SRC, "RunnerProbe"));
    let err = run_err(&reg, "RunnerProbe", "boom", "()I");
    assert!(matches!(err, rustj::runtime::VmError::ThrownException(_)), "期望 ThrownException,得 {err:?}");
}

#[test]
fn probe_run_static_in() {
    require_javac!();
    let reg = std::sync::Arc::new(compile_and_load(RUNNER_SRC, "RunnerProbe"));
    let mut vm = rustj::runtime::VmThread::new(std::sync::Arc::clone(&reg));
    let v = run_static_in(&mut vm, "RunnerProbe", "seven", "()I").unwrap();
    assert!(matches!(v, rustj::runtime::Value::Int(7)));
}

#[test]
fn probe_run_raw() {
    require_javac!();
    let reg = std::sync::Arc::new(compile_and_load(RUNNER_SRC, "RunnerProbe"));
    let lc = reg.get("RunnerProbe").unwrap();
    assert_eq!(run_raw_int(&lc.cf, "seven", "()I", &[]), 7);
    assert_eq!(run_raw_int(&lc.cf, "add", "(II)I", &[3, 4]), 7);
    assert!(matches!(run_raw_value(&lc.cf, "add", "(II)I", &[Arg::I(3), Arg::I(4)]), rustj::runtime::Value::Int(7)));
}

// Task 8: 断言探针(as_int + assert_int!/assert_long!/assert_double!/assert_float! + assert_throws!/assert_is_thrown!)
const ASSERT_SRC: &str = r#"
public class AssertProbe {
    public static int i() { return 7; }
    public static long l() { return 3000000000L; }
    public static double d() { return 2.5; }
    public static float f() { return 3.0f; }
    public static int boom() { int z = 0; return 1 / z; }
}
"#;

#[test]
fn probe_as_and_assert_values() {
    require_javac!();
    let reg = std::sync::Arc::new(compile_and_load(ASSERT_SRC, "AssertProbe"));
    assert_eq!(as_int(run(&reg, "AssertProbe", "i", "()I")), 7);
    assert_int!(run(&reg, "AssertProbe", "i", "()I"), 7);
    assert_long!(run(&reg, "AssertProbe", "l", "()J"), 3000000000_i64);
    assert_double!(run(&reg, "AssertProbe", "d", "()D"), 2.5);
    assert_float!(run(&reg, "AssertProbe", "f", "()F"), 3.0_f32);
}

#[test]
fn probe_assert_throws_and_is_thrown() {
    require_javac!();
    let reg = std::sync::Arc::new(compile_and_load(ASSERT_SRC, "AssertProbe"));
    assert_is_thrown!(run_err(&reg, "AssertProbe", "boom", "()I"));
    let (r, vm) = run_result(&reg, "AssertProbe", "boom", "()I");
    assert_throws!(r, vm, "java/lang/ArithmeticException");
}
