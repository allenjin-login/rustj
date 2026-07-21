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

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::VmThread;
use rustj::testkit::*;

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

/// **集成闸门**(Layer 4.34):StaticProperty.<clinit> 不再因 native.encoding null 抛 InternalError
/// → nio FileSystem 就绪 → `Path.of("foo")` 返非 null。修前抛 ExceptionInInitializerError
/// (cause=InternalError "null property: native.encoding")。
#[test]
fn static_property_encodings_populated_enables_path_of() {
    require_javac!();
    require_javabase!(jmod);
    let dir = compile_dir(PROBE, "PathProbe", &[]);

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
