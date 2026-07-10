//! 集成闸门(4.10t):Class 镜像 intern(身份相等)+ `Object.getClass()`。
//!
//! 修前每次 `Foo.class`(ldc)/`getPrimitiveClass`/STE.declaringClassObject 都 `alloc` 一个
//! **新** `Oop::Class` → `Foo.class == Foo.class` 为假、`obj.getClass() == Foo.class` 为假
//! —— Class 对象身份不相等(对应 HotSpot 每 `Klass` 应持单一 `_java_mirror`)。
//!
//! 三静态法,成功返 1/2/3,失配返负诊断:
//! - `literalTwice`:`Class<?> a=Cm.class; Class<?> b=Cm.class; a==b` → 1(两次 ldc 须同引用)。
//! - `getClassEq`:`new Cm().getClass() == Cm.class` → 2(getClass native 须返规范镜像)。
//! - `distinct`:`Cm.class == Object.class` → 3(异类须异镜像,无假共享)。
//!
//! `getClass` 是 `Object.java:68 public final native`;引导桩 Object 无此方法条目 → 须预载真
//! Object 闭包使 `invokevirtual getClass` 解析到真 Object 的 native。需 `javac` + jmod;缺一跳过。

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

fn compile_dir(source: &str, public_name: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-cm-{n}-{}-{public_name}", std::process::id()));
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

fn find_method<'a>(lc: &'a rustj::oops::LoadedClass, name: &str, desc: &str) -> &'a rustj::metadata::MethodInfo {
    use rustj::constant_pool::ConstantPoolEntry;
    lc.cf.methods.iter().find(|m| {
        let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
        let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
        n && d
    }).unwrap_or_else(|| panic!("未找到方法 Cm.{name}{desc}"))
}

const SOURCE: &str = r#"
public class Cm {
    public static int literalTwice() {
        Class a = Cm.class;        // raw Class:使 == 不受泛型不变性限制
        Class b = Cm.class;
        return a == b ? 1 : -1;
    }
    public static int getClassEq() {
        Class c = new Cm().getClass();
        Class d = Cm.class;
        return c == d ? 2 : -2;
    }
    public static int distinct() {
        Class a = Cm.class;
        Class b = Object.class;
        return a == b ? -3 : 3;
    }
}
"#;

fn run_int(vm: &mut Vm, name: &str) -> i32 {
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("Cm").expect("Cm 须已加载");
    let m = find_method(lc, name, "()I");
    let code = m.code.as_ref().expect("{name} 须有 Code");
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
            panic!("Cm.{name} 抛 Java 异常:{cls}(Class 镜像 intern / getClass 链有缺口)")
        }
        other => panic!("Cm.{name} 应返 int,得 {other:?}"),
    }
}

/// **集成闸门**:Class 镜像 intern + getClass。
#[test]
fn class_mirrors_are_canonical() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    // 1) javac 编译 Cm;载入注册表。
    let dir = compile_dir(SOURCE, "Cm");
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("Cm.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 2) 预载真 Object 闭包(getClass 是 Object 的 native,引导桩无此方法条目 → 须真 Object)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/Object").unwrap();
    assert!(!registry.get("java/lang/Object").unwrap().is_synthetic_stub(), "Object 须为真类");

    let mut vm = Vm::new(registry);

    // 3) 三法:1=两次 ldc 同引用;2=getClass==字面量;3=异类异镜像。
    assert_eq!(run_int(&mut vm, "literalTwice"), 1, "Cm.class 两次 ldc 须为同一 Class 镜像(intern)");
    assert_eq!(run_int(&mut vm, "getClassEq"), 2, "new Cm().getClass() 须 == Cm.class(规范镜像)");
    assert_eq!(run_int(&mut vm, "distinct"), 3, "Cm.class 须 != Object.class(异类异镜像)");
}
