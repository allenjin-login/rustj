//! 集成闸门(Layer 4.10v):`aastore` 的 **ArrayStoreException**。
//!
//! 引用数组存入不可赋元素时,rustj 须按 HotSpot `objArrayKlass` 抛
//! `java/lang/ArrayStoreException`(可捕获)。默认 javac 编译,预载真 `String` 闭包。
//!
//! - `mismatch()`:`Object[] a = new String[1]` 后存 `int[]` → 运行期 `String[]` 拒收 → ASE → 捕获返 1。
//! - `okMatch()`:存 `String` 入 `String[]` → 合法 → 返 1(防检查误杀合法存入)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::testkit::*;
use rustj::runtime::VmThread;

const SOURCE: &str = r#"
public class AstoreAse {
    // 运行期为 String[],存 int[] → ArrayStoreException → 捕获返 1。
    public static int mismatch() {
        Object[] a = new String[1];
        try { a[0] = new int[1]; return 0; }
        catch (ArrayStoreException e) { return 1; }
    }
    // 合法存入(String 入 String[])→ 返 1(防误杀)。
    public static int okMatch() {
        Object[] a = new String[1];
        a[0] = "x";
        return a.length;
    }
}
"#;

/// **集成闸门**:aastore 不可赋元素 → ArrayStoreException。
#[test]
fn aastore_array_store_exception() {
    use rustj::oops::ClassRegistry;
    use rustj::runtime::class_loader::class_path::ClassPath;
    use rustj::runtime::class_loader::loader::load_closure;

    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "AstoreAse", &[]);
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("AstoreAse.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载真 String 闭包(String[] 组件 + "x" 字面量须为真 String)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    let mut vm = VmThread::new(registry);
    assert_eq!(run_static_int(&mut vm, "AstoreAse", "okMatch").unwrap(), 1, "String 入 String[] 须合法");
    assert_eq!(
        run_static_int(&mut vm, "AstoreAse", "mismatch").unwrap(),
        1,
        "int[] 入 String[] 须抛 ArrayStoreException → 捕获返 1"
    );
}
