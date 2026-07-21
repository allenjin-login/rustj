//! 集成闸门(Layer 4.15a):**反射类元数据 native — `Class.forName0`**。
//!
//! `Class.forName(String, boolean, ClassLoader)`(Class.java:557)经 `validateClassNameLength`
//! 调私有 static native `forName0`(Class.java:565)。rustj 移植 jvm.cpp `JVM_FindClassFromCaller`
//! 语义:按名(点形)查注册表 → `init=true` 触发 `ensure_class_initialized` → 返类镜像;
//! 未找到 → `ClassNotFoundException`。本层先做 `forName0`(反射地基);`getDeclared*0`
//! (Field[]/Method[]/Constructor[] 构造)顺延本层后续部分。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::{bootstrap_module_system, initialize_system_class};
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class Reflect {
    // Class.forName(name, true, null) → forName0 → 按名查注册表返镜像 == Integer.class。
    public static int forNameInteger() {
        try {
            Class<?> c = Class.forName("java.lang.Integer", true, null);
            return c == Integer.class ? 1 : -1;
        } catch (ClassNotFoundException e) {
            return -100;
        }
    }
    // 未知名 → forName0 抛 ClassNotFoundException → catch 返 2。
    public static int forNameUnknownThrows() {
        try {
            Class.forName("no.such.Class", true, null);
            return -1; // 不应到达
        } catch (ClassNotFoundException e) {
            return 2;
        }
    }
    // initialize=false:不触发 <clinit>,仍返同一镜像 == String.class。
    public static int forNameNoInit() {
        try {
            Class<?> c = Class.forName("java.lang.String", false, null);
            return c == String.class ? 3 : -3;
        } catch (ClassNotFoundException e) {
            return -100;
        }
    }
}
"#;

/// **集成闸门**:Layer 4.15a Class.forName0 反射类查找。
#[test]
fn class_for_name0_resolves_by_name() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编 Reflect;载入注册表。
    let dir = compile_dir(SOURCE, "Reflect", &[]);
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("Reflect.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 Class(forName 所在)+ Integer/String(测试体)
    //    + ClassNotFoundException(catch 引用)+ ClassLoader(forName 描述符)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/Class").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/ClassNotFoundException").unwrap();

    let mut vm = VmThread::new(registry);
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");
    bootstrap_module_system(&mut vm).expect("Phase 2 引导应成功");

    // 3) forName0 按名查注册表返镜像(== Integer.class,同一 intern)。
    assert_eq!(run_static_int(&mut vm, "Reflect", "forNameInteger"), Ok(1), "forName0 须返 Integer 镜像");
    // 4) 未知名 → ClassNotFoundException(catch 命中)。
    assert_eq!(
        run_static_int(&mut vm, "Reflect", "forNameUnknownThrows"),
        Ok(2),
        "forName0 未知名须抛 ClassNotFoundException"
    );
    // 5) initialize=false 仍返正确镜像(不触发 <clinit>)。
    assert_eq!(run_static_int(&mut vm, "Reflect", "forNameNoInit"), Ok(3), "forName0(init=false) 须返 String 镜像");
}
