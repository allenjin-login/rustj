//! 集成闸门(Layer 4.27):**`WinNTFileSystem.canonicalize0(String)String` native**。
//!
//! `ClassLoader.getSystemClassLoader()` 链越过 `WinNTFileSystem.<init>`(4.26)后,阻塞于:
//! `ClassLoaders.<clinit>:90` → `URLClassPath.<init>:133` → `URLClassPath.toFileURL:241` →
//! `File.getCanonicalFile:638` → `File.getCanonicalPath:619` → `WinNTFileSystem.canonicalize:485` →
//! `canonicalize0`(native,ULE)。`toFileURL("")` 因 `cp=""`(未设 java.class.path)→
//! `new File("").getCanonicalPath()` → `canonicalize0("")` = 当前目录规范路径(HotSpot `wcanonicalize`
//! 经 `currentDirLength` 对空串前置 currentDir)。
//!
//! 修前:canonicalize0 未登记 → `java/io/*` 落 `_ => UnsatisfiedLinkError`(Error,非 IOException,
//! 不被 `getCanonicalPath` 的 catch 捕获)→ 传播出 `cwdLen()`。修后:返 cwd 规范路径长度(>0)。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

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

const CANON_PROBE_SOURCE: &str = r#"
public class CanonProbe {
    // new File("").getCanonicalPath():触发 File.<clinit> → DefaultFileSystem → WinNTFileSystem.<init>
    // (读 file.separator/user.dir 等 props)→ getCanonicalPath → fs.canonicalize → canonicalize0。
    // 返 cwd 规范路径长度(>0)。修前 canonicalize0 ULE(Error,非 IOException)未捕 → 传播。
    public static int cwdLen() throws Throwable {
        String s = new java.io.File("").getCanonicalPath();
        return (s == null) ? -1 : s.length();
    }
}
"#;

/// **集成闸门**(Layer 4.27):`WinNTFileSystem.canonicalize0` 经 `std::fs::canonicalize` 手移植 →
/// `new File("").getCanonicalPath()` 返当前目录规范路径(长度 > 0)。修前抛 UnsatisfiedLinkError。
#[test]
fn canonicalize0_resolves_cwd() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "rustj-canon-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::new(0)
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("CanonProbe.java"), CANON_PROBE_SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(dir.join("CanonProbe.java"))
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
                &std::fs::read(dir.join("CanonProbe.class")).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // File 闭包拉入 FileSystem/DefaultFileSystem/WinNTFileSystem;System/Properties/HashMap 供 Phase 1。
    load_closure(&mut registry, &cp, "java/io/File").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    // 运行 CanonProbe.cwdLen():返 cwd 规范路径长度。修前抛 UnsatisfiedLinkError(canonicalize0 未登记)。
    let reg = vm.registry().expect("类注册表");
    let lc = reg
        .get("CanonProbe")
        .expect("CanonProbe 须加载");
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "cwdLen");
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).expect("须有 cwdLen()I");
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, &mut vm) {
        Ok(Value::Int(n)) => {
            assert!(n > 0, "cwd 规范路径长度须 > 0,得 {n}");
            // 进一步:与 std::env::current_dir 的规范路径长度同量级(剥 \\?\ 前缀后)。
            let cwd = std::fs::canonicalize(".").unwrap_or_default();
            let cwd_str = cwd.display().to_string();
            let cwd_stripped = cwd_str
                .strip_prefix(r"\\?\")
                .unwrap_or(&cwd_str);
            assert_eq!(
                n,
                cwd_stripped.len() as i32,
                "canonicalize0(\"\") 须等于 cwd 规范路径(剥 \\\\?\\ 前缀):{cwd_stripped}"
            );
        }
        Ok(other) => panic!("期望 int,得 {other:?}"),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("{o:?}"),
            };
            panic!("cwdLen 不应抛异常(canonicalize0 须已登记),却抛:{exc_name}");
        }
        Err(e) => panic!("内部错误:{e:?}"),
    }
}
