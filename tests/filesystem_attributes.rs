//! 集成闸门(Layer 4.28):**`WinNTFileSystem.getBooleanAttributes0(File)I` native**。
//!
//! 4.27 越过 `canonicalize0`+`getFinalPath0` 后,链阻塞于:`File.isDirectory:811` →
//! `FileSystem.hasBooleanAttributes:141` → `WinNTFileSystem.getBooleanAttributes:510` →
//! `getBooleanAttributes0`(native,ULE)。HotSpot `Java_java_io_WinNTFileSystem_getBooleanAttributes0`
//! 经 `getFinalAttributes`(=GetFileAttributesEx)返位掩码:`BA_EXISTS=0x01`(存在)/
//! `BA_REGULAR=0x02`(普通文件)/`BA_DIRECTORY=0x04`(目录)/`BA_HIDDEN=0x08`(隐藏);
//! 不存在(INVALID_FILE_ATTRIBUTES)→ 0。rustj 读 File 实例 `path` 字段后 `std::fs::metadata`。

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

const ATTR_PROBE_SOURCE: &str = r#"
public class AttrProbe {
    // user.dir = cwd,必为目录 → isDirectory()=true(1)。触发 getBooleanAttributes0(cwd) 返 BA_EXISTS|BA_DIRECTORY。
    public static int cwdIsDir() {
        return new java.io.File(System.getProperty("user.dir")).isDirectory() ? 1 : 0;
    }
    // 不存在的路径 → exists()=false(0)。触发 getBooleanAttributes0 返 0(INVALID_FILE_ATTRIBUTES)。
    public static int nonexistentExists() {
        String t = System.getProperty("java.io.tmpdir");
        return new java.io.File(t, "rustj_no_such_file_xyz_999").exists() ? 1 : 0;
    }
    // cwd 作为普通文件?false(0)。证明 BA_DIRECTORY 置位时 BA_REGULAR 不置位。
    public static int cwdIsFile() {
        return new java.io.File(System.getProperty("user.dir")).isFile() ? 1 : 0;
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

/// **集成闸门**(Layer 4.28):`getBooleanAttributes0` 经 `std::fs::metadata` 返属性位掩码 →
/// `File.isDirectory/isFile/exists` 可用。修前抛 UnsatisfiedLinkError。
#[test]
fn get_boolean_attributes0_reports_dir_file_missing() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };
    let dir = std::env::temp_dir().join(format!(
        "rustj-attr-{}-{}",
        std::process::id(),
        std::sync::atomic::AtomicU64::new(0)
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("AttrProbe.java"), ATTR_PROBE_SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(dir.join("AttrProbe.java"))
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
                &std::fs::read(dir.join("AttrProbe.class")).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/io/File").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = Vm::new(&registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    // cwd 是目录:isDirectory=1,isFile=0(BA_DIRECTORY 置位、BA_REGULAR 不置位)。
    assert_eq!(
        run_static_int(&mut vm, "AttrProbe", "cwdIsDir"),
        Ok(1),
        "cwd 须判定为目录"
    );
    assert_eq!(
        run_static_int(&mut vm, "AttrProbe", "cwdIsFile"),
        Ok(0),
        "cwd 须非普通文件"
    );
    // 不存在路径:exists=0(属性为 0,无 BA_EXISTS)。
    assert_eq!(
        run_static_int(&mut vm, "AttrProbe", "nonexistentExists"),
        Ok(0),
        "不存在路径须 exists()=false"
    );
}
