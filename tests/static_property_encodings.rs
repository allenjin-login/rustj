//! 集成闸门(Layer 4.34):**StaticProperty native.encoding/stdin.encoding 系统属性补全**。
//!
//! 4.33 identityHashCode 越过后,`Path.of`→`FileSystems.getDefault`→`DefaultFileSystemProvider.<clinit>`
//! →`WindowsFileSystemProvider.<init>:52`→`StaticProperty.<clinit>:87` 链阻塞于:`StaticProperty.getProperty`
//! (StaticProperty.java:130)抛 `InternalError("null property: native.encoding")`——`StaticProperty.<clinit>`
//! 读 `native.encoding`/`stdin.encoding`(StaticProperty.java:93/95,**无默认值**,null→InternalError),
//! 而 Phase 1 `populate_launcher_props` 漏装此二键(只装了 file/sun.jnu/stdout/stderr.encoding)。
//!
//! 修法:在 `populate_launcher_props` 增 `native.encoding`/`stdin.encoding`(值同 stdout.encoding=UTF-8)。
//! 解锁 StaticProperty.<clinit> → WindowsFileSystemProvider 初始化 → nio FileSystem 就绪 → `Path.of` 可用。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};

fn javac_available() -> bool {
    Command::new("javac").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}
fn find_javabase_jmod() -> Option<PathBuf> {
    for ver in ["jdk-25.0.2", "jdk-24", "jdk-21", "jdk-17", "jdk-11.0.30"] {
        let p = Path::new("C:/Program Files/Java").join(ver).join("jmods/java.base.jmod");
        if p.exists() { return Some(p); }
    }
    None
}

// Path.of("foo") 触发 FileSystems.getDefault → DefaultFileSystemProvider.<clinit> →
// WindowsFileSystemProvider.<init> → StaticProperty.<clinit>(读 native.encoding)。
const PROBE: &str = r#"
import java.nio.file.Path;
public class PathProbe {
    public static int make() {
        return Path.of("foo") == null ? 0 : 1;
    }
}
"#;

fn run_static_int(vm: &mut VmThread, class: &str, name: &str) -> Result<i32, String> {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表缺失"));
    let lc = reg.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
    let method = lc.cf.methods.iter().find(|m| {
        use rustj::constant_pool::ConstantPoolEntry;
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 {class}.{name}()I"));
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    match interp.interpret_with(&mut frame, vm) {
        Ok(Value::Int(n)) => Ok(n),
        Ok(other) => Err(format!("{class}.{name} 期望 int,得 {other:?}")),
        Err(VmError::ThrownException(r)) => {
            let trace = vm.format_trace(r);
            let head = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("{o:?}"),
            };
            Err(format!("{head}\n{trace}"))
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

/// **集成闸门**(Layer 4.34):StaticProperty.<clinit> 不再因 native.encoding null 抛 InternalError
/// → nio FileSystem 就绪 → `Path.of("foo")` 返非 null。修前抛 ExceptionInInitializerError
/// (cause=InternalError "null property: native.encoding")。
#[test]
fn static_property_encodings_populated_enables_path_of() {
    if !javac_available() { eprintln!("跳过:无 javac"); return; }
    let Some(jmod) = find_javabase_jmod() else { eprintln!("跳过:无 java.base.jmod"); return; };
    let dir = std::env::temp_dir().join(format!("rustj-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("PathProbe.java"), PROBE).unwrap();
    let out = Command::new("javac").arg("-d").arg(&dir).arg(dir.join("PathProbe.java")).output().expect("javac 失败");
    assert!(out.status.success(), "javac 失败:{}", String::from_utf8_lossy(&out.stderr));

    let mut registry = ClassRegistry::new();
    registry.load(rustj::classfile::parse(&std::fs::read(dir.join("PathProbe.class")).unwrap()).unwrap()).unwrap();
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassLoader").unwrap();
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    load_closure(&mut registry, &cp, "java/util/Properties").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    assert_eq!(
        run_static_int(&mut vm, "PathProbe", "make"),
        Ok(1),
        "Path.of 须返非 null(StaticProperty.<clinit> 须成功:native.encoding/stdin.encoding 已装)"
    );
}
