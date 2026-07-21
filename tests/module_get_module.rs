//! 集成闸门(Layer 4.14a):**`java/lang/Module` 对象模型 + `Class.getModule()`**。
//!
//! 模块系统对象层地基(对应 `java.lang.Module` + `Class.getModule()`)。真 JVM 由
//! `ModuleBootstrap.boot()`(Phase 2,4.14b)为每个命名模块建 `Module` 实例并关联到类;
//! `Class.getModule()` 读 `module` 字段(`Class.java:1011`,`getModule` 仅 `return module`)。
//!
//! 本层最小但忠实:加载真 `java/lang/Module`;按「容器→模块」从 jmod 的 `module-info.class`
//! 推导模块归属(java.base.jmod 的所有类 → java.base 模块);`Class` 镜像 `module` 字段填对应
//! `Module` 实例。验证:`Integer`/`String` 同属 java.base(同一 Module 引用)、
//! `getModule().getName()`=="java.base"、用户类 `Mod`(非模块源)属无名模块。
//!
//! 需 `javac` + 本机 jmod;缺一跳过。

use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::VmThread;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class Mod {
    // Integer 与 String 同属 java.base → getModule() 返回同一 Module 引用。
    public static int sameModule() {
        return Integer.class.getModule() == String.class.getModule() ? 1 : 0;
    }
    // java.base 模块的 getName() == "java.base"。
    public static int baseName() {
        java.lang.Module m = Integer.class.getModule();
        return (m != null && m.getName().equals("java.base")) ? 1 : 0;
    }
    // 用户类 Mod 来自非模块源(编译 .class)→ 无名模块:getModule() 非 null 且 !isNamed()。
    public static int userClassUnnamed() {
        java.lang.Module m = Mod.class.getModule();
        return (m != null && !m.isNamed()) ? 1 : 0;
    }
}
"#;

/// **集成闸门**:Layer 4.14a 真 java/lang/Module + Class.getModule()。
#[test]
fn class_get_module_returns_real_module() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编 Mod;载入注册表(非模块源 → 无名模块)。
    let mut registry = compile_and_load(SOURCE, "Mod");

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 Module(对象模型)+ Integer/String(测试体)
    //    + HashMap(String clinit 链路用)。容器→模块由 load_closure 从 module-info 推导。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/Module").unwrap();
    load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();

    let mut vm = VmThread::new(registry);
    // Phase 1(savedProps 引导,4.13)→ Integer 等 <clinit> 可跑。
    initialize_system_class(&mut vm).expect("Phase 1 引导应成功");

    // 3) Integer/String 同一 java.base Module 引用(模块实例 intern)。
    assert_eq!(run_static_int(&mut vm, "Mod", "sameModule"), Ok(1), "Integer/String 须同属 java.base 模块");
    // 4) java.base 模块名 == "java.base"(Module.name 字段填充 + getName 字节码 + String.equals)。
    assert_eq!(run_static_int(&mut vm, "Mod", "baseName"), Ok(1), "java.base 模块名须为 java.base");
    // 5) 用户类 Mod 属无名模块(非命名)。
    assert_eq!(run_static_int(&mut vm, "Mod", "userClassUnnamed"), Ok(1), "用户类须属无名模块");
}
