//! 集成闸门(Layer 4.29):**`Unsafe.getLong/getInt/getShort/getByte(Object,long)` 偏移读族(array)**。
//!
//! `ClassLoader.getSystemClassLoader()` 链越过 `getBooleanAttributes0`(4.28)后,阻塞于:
//! `ClassLoaders.<clinit>` → `URLClassPath.<init>` → `toFileURL` → `new URL` →
//! `isBuiltinStreamHandler` → `String.startsWith` → `ArraysSupport.mismatch(byte[],aFromIndex,...)`
//! (length>7)→ `vectorizedMismatch:132`(纯 Java 字节码,`@IntrinsicCandidate` 非 native)→
//! `Unsafe.getLongUnaligned` → `Unsafe.getLong(Object,long)`(native,ULE)。
//!
//! `vectorizedMismatch`(ArraysSupport.java:118)对 byte[] 以 `U.getLongUnaligned(a, aOffset + bi)`
//! (bi = wi<<3)读 8 字节做向量化比较;`aOffset = ARRAY_BYTE_BASE_OFFSET(16) + aFromIndex`,故 8 对齐时
//! `getLongUnaligned` 直调 native `getLong`。rustj 无真实偏移内存:把 ArrayOop 视为扁平小端字节缓冲,
//! 按 byte_offset = offset - 16 取 N 字节、按组件类型序列化、小端打包。**byte[]**(String 紧凑串)
//! 为即时场景;getLongUnaligned/getIntUnaligned 对未对齐 offset 退化到 getInt/getShort/getByte,
//! 故四族均须绑(`(Ljava/lang/Object;J){J,I,S,B}`)。修前 getLong 未登记 → ULE 传出 startsWith。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
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
    None
}

// 前 8 字节 "https://" 相同 → vectorizedMismatch 第一轮 getLong 相等;尾部逐字节比对。
//   startsTrue:  "https://example.com".startsWith("https://exa")  → 全等 → 1
//   startsFalse: "https://example.com".startsWith("https://exb")  → 第 10 字节 'a'/'b' 不匹配 → 0
// 前缀长 11 > 7 → 触发 vectorizedMismatch → getLong(byte[], 16)(8 对齐)。
const MISMATCH_PROBE_SOURCE: &str = r#"
public class MismatchProbe {
    public static int startsTrue() {
        return "https://example.com".startsWith("https://exa") ? 1 : 0;
    }
    public static int startsFalse() {
        return "https://example.com".startsWith("https://exb") ? 1 : 0;
    }
}
"#;

fn run_static_int(vm: &mut Vm<'_>, class: &str, name: &str) -> Result<i32, String> {
    let lc = vm
        .registry()
        .and_then(|r| r.get(class))
        .unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {class}.{name}()I"));
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("{o:?}"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

/// **集成闸门**(Layer 4.29):`String.startsWith`(prefix len > 7)经 `vectorizedMismatch` →
/// `Unsafe.getLong(byte[], 16)`(8 对齐)做向量化比较。修前 getLong native 未登记 →
/// `UnsatisfiedLinkError` 传出 startsWith。修后:相等同缀返 true、首字节差返 false。
#[test]
fn unsafe_get_long_supports_vectorized_mismatch() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "rustj-mismatch-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::new(0)
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("MismatchProbe.java"), MISMATCH_PROBE_SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(dir.join("MismatchProbe.java"))
        .output()
        .expect("javac 失败");
    assert!(
        out.status.success(),
        "javac 失败:{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut registry = ClassRegistry::new();
    registry
        .load(
            rustj::classfile::parse(
                &std::fs::read(dir.join("MismatchProbe.class")).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // String 闭包拉入 StringLatin1/ArraysSupport/Unsafe 等(经 startsWith 字节码内的 CP 引用);
    // System/Properties/HashMap 供 Phase 1 引导。
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = Vm::new(&registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    // 同前缀 → vectorizedMismatch 第一轮 getLong 相等、尾部全等 → startsWith=true(1)。
    assert_eq!(
        run_static_int(&mut vm, "MismatchProbe", "startsTrue"),
        Ok(1),
        "同前缀长串 startsWith 须返 true(getLong 已绑)"
    );
    // 第 10 字节差 → 尾部定位 mismatch → startsWith=false(0)。
    assert_eq!(
        run_static_int(&mut vm, "MismatchProbe", "startsFalse"),
        Ok(0),
        "首差字节 startsWith 须返 false"
    );
}
