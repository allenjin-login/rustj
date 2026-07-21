//! 集成闸门(4.10s):真 `Throwable.getMessage()` / `getCause()` 经真实例字段。
//!
//! 三静态法,成功返 1/2/3,失配返负诊断:
//! - `autoMessage`:idiv→ArithmeticException(由 JVM **自动**抛出,不经真 `<init>`),
//!   catch 内 `e.getMessage().equals("/ by zero")` → 1。修前 `detailMessage` 字段未填
//!   (new_instance 跳过 `<init>`,record_message 仅写并行 exception_meta)→ getMessage 返
//!   null → `.equals` 抛 NPE;修后 throw_exception_with_message 直接回填真字段。
//! - `userMessage`:`throw new RuntimeException("boom")`(经真 `<init>(String)` 自置 detailMessage)
//!   → 2。验证既有真 `<init>` 路径。
//! - `userCause`:`throw new RuntimeException("boom", root)`(经真 `<init>(String,Throwable)`
//!   自置 cause 字段,无 toString 依赖)→ getCause() == root → 3。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::testkit::*;
use rustj::runtime::VmThread;

const SOURCE: &str = r#"
public class Tm {
    // JVM 自动抛出(idiv 除零),不经真 <init>:detailMessage 须由 throw_exception_with_message 回填。
    public static int autoMessage() {
        try { int x = 1 / 0; return x; }
        catch (ArithmeticException e) {
            return e.getMessage().equals("/ by zero") ? 1 : -1;
        }
    }
    // 用户抛出:经真 RuntimeException.<init>(String) 自置 detailMessage。
    public static int userMessage() {
        try { throw new RuntimeException("boom"); }
        catch (RuntimeException e) {
            return e.getMessage().equals("boom") ? 2 : -2;
        }
    }
    // 用户包裹:经真 RuntimeException.<init>(String,Throwable) 自置 cause(无 toString 依赖)。
    public static int userCause() {
        Exception root = new Exception("root");
        try { throw new RuntimeException("boom", root); }
        catch (RuntimeException e) {
            Throwable c = e.getCause();
            if (c == null) return -3;
            if (c == root) return 3;
            return -31;
        }
    }
}
"#;

/// **集成闸门**:getMessage/getCause 经真实例字段回读正确。
#[test]
fn get_message_and_get_cause_via_real_fields() {
    use rustj::oops::ClassRegistry;
    use rustj::runtime::class_loader::class_path::ClassPath;
    use rustj::runtime::class_loader::loader::load_closure;

    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编译 Tm;载入注册表。
    let dir = compile_dir(SOURCE, "Tm", &[]);
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("Tm.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 2) 预载真 java.base 的 ArithmeticException(闭包带入 RuntimeException/Exception/Throwable/
    //    Object)+ String(getMessage/equals 返回真 String)。Vm 以不可变借用持注册表,运行期
    //    不可追加 → 须在 Vm::new 前装好(同 4.10i/4.10r 预载约束)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ArithmeticException").unwrap();
    assert!(!registry.get("java/lang/ArithmeticException").unwrap().is_synthetic_stub(), "ArithmeticException 须为真类");
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    // 3) getMessage/getCause 须为真字节码(非 native)——证字段回读走真字节码。
    let thr = registry.get("java/lang/Throwable").unwrap();
    let gm = find_method(&thr.cf, "getMessage", "()Ljava/lang/String;");
    assert!(!gm.access_flags.is_native(), "Throwable.getMessage 须为真字节码");
    let gc = find_method(&thr.cf, "getCause", "()Ljava/lang/Throwable;");
    assert!(!gc.access_flags.is_native(), "Throwable.getCause 须为真字节码");

    let mut vm = VmThread::new(registry);

    // 4) 三法逐一断言:1=自动抛出 detailMessage 回填,2=用户 <init>(String),3=<init>(String,Throwable)。
    assert_eq!(run_static_int(&mut vm, "Tm", "autoMessage").unwrap(), 1, "自动抛出 ArithmeticException 的 getMessage 须为 \"/ by zero\"");
    assert_eq!(run_static_int(&mut vm, "Tm", "userMessage").unwrap(), 2, "用户抛出 RuntimeException(\"boom\") 的 getMessage 须为 \"boom\"");
    assert_eq!(run_static_int(&mut vm, "Tm", "userCause").unwrap(), 3, "用户包裹 RuntimeException(\"boom\",root) 的 getCause 须 == root");
}
