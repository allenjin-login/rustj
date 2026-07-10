//! 集成闸门(Layer 4.10v):`aastore` 的 **ArrayStoreException**。
//!
//! 引用数组存入不可赋元素时,rustj 须按 HotSpot `objArrayKlass` 抛
//! `java/lang/ArrayStoreException`(可捕获)。默认 javac 编译,预载真 `String` 闭包。
//!
//! - `mismatch()`:`Object[] a = new String[1]` 后存 `int[]` → 运行期 `String[]` 拒收 → ASE → 捕获返 1。
//! - `okMatch()`:存 `String` 入 `String[]` → 合法 → 返 1(防检查误杀合法存入)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, Vm, VmError};

fn javac_available() -> bool {
    Command::new("javac")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java")
            .join(ver)
            .join("jmods/java.base.jmod");
        if p.exists() {
            return Some(p);
        }
    }
    std::env::var("JAVA_HOME")
        .ok()
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

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

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-ase-{n}-{}-{public_name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

fn run_int(vm: &mut Vm, name: &str) -> i32 {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("AstoreAse").expect("AstoreAse 须已加载");
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .unwrap();
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => n,
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            panic!("AstoreAse.{name} 抛 Java 异常:{cls}(aastore/ASE 链有缺口)")
        }
        other => panic!("AstoreAse.{name} 应返 int,得 {other:?}"),
    }
}

/// **集成闸门**:aastore 不可赋元素 → ArrayStoreException。
#[test]
fn aastore_array_store_exception() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_dir(SOURCE, "AstoreAse");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("AstoreAse.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载真 String 闭包(String[] 组件 + "x" 字面量须为真 String)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    let mut vm = Vm::new(registry);
    assert_eq!(run_int(&mut vm, "okMatch"), 1, "String 入 String[] 须合法");
    assert_eq!(
        run_int(&mut vm, "mismatch"),
        1,
        "int[] 入 String[] 须抛 ArrayStoreException → 捕获返 1"
    );
}
