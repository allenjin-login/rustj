//! 集成闸门(4.10 capstone):用 `javac` 编译一个调用 `new Object().hashCode()` 的最小真
//! Java 程序,`Object` 从真实 `java.base.jmod` 经 `ClassPath` 加载并**覆盖合成桩**,
//! 再由 rustj 解释器执行——端到端验证「真容器 → 真类 → `<clinit>` → native 分派 → 身份哈希」
//! 全链(北极星:加载真实 java.base 的最小可运行证据)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::classfile::parse;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class NativeGate {
    // 真 Object(从 jmod 载入覆盖桩)经 new → <clinit>(registerNatives 空)→ hashCode()。
    // 同一对象两次 hashCode 必相等(native 身份哈希 = 句柄 idx)→ 返回 1。
    public static int run() {
        Object o = new Object();
        int a = o.hashCode();
        int b = o.hashCode();
        return a == b ? 1 : 0;
    }
}
"#;

/// **capstone**:真 java.base 的 Object 经容器加载 + native 分派端到端跑通。
#[test]
fn real_object_hashcode_runs_via_native_dispatch() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编译 NativeGate;载入注册表(连同合成桩)。
    let dir = compile_dir(SOURCE, "NativeGate", &[]);
    let mut registry = ClassRegistry::new();
    let ng = parse(&std::fs::read(dir.join("NativeGate.class")).unwrap()).unwrap();
    registry.load(ng).unwrap();

    // 2) 真 Object 从 jmod 载入,**覆盖**合成桩。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let real_obj = cp
        .load_class("java/lang/Object")
        .unwrap()
        .expect("Object 须在 jmod 内")
        .0;
    registry.load_or_replace(real_obj).unwrap();
    let registry = std::sync::Arc::new(registry);

    // 3) 真 Object.hashCode 须为 ACC_NATIVE(桩无 hashCode → 证覆盖成功)。
    let obj_lc = registry.get("java/lang/Object").unwrap();
    let hash = find_method(&obj_lc.cf, "hashCode", "()I");
    assert!(hash.access_flags.is_native(), "真 Object.hashCode 须 native");

    // 4) 跑 NativeGate.run():new Object → <clinit> registerNatives(native 空操作)→
    //    hashCode()×2 → 同句柄同 idx → 相等 → 返回 1。
    assert_eq!(
        run_result(&registry, "NativeGate", "run", "()I").0.unwrap(),
        Value::Int(1)
    );
}
