//! 集成闸门(Layer 4.10l):真 `java/lang/System.arraycopy`(static native)端到端。
//!
//! javac 编一个用 `System.arraycopy` 的真 Java 程序:基本类型拷贝 + 读回求和、
//! 同数组重叠平移(memmove)、越界→`ArrayIndexOutOfBoundsException`。`System` 经闭包
//! 从本机 `java.base.jmod` 预载(其 `arraycopy` 为 ACC_NATIVE → 走内置 native 分派表,
//! 即 `arraycopy::system_arraycopy`)。语义对照 HotSpot
//! `typeArrayKlass::copy_array` / `objArrayKlass::copy_array`。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
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
    std::env::var("JAVA_HOME")
        .ok()
        .map(|jh| Path::new(&jh).join("jmods/java.base.jmod"))
        .filter(|p| p.exists())
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// javac 编 `ArrayCopyGate` 到唯一临时目录,返回该目录。
fn compile_gate() -> PathBuf {
    let s = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rustj-arrcopy-{}-{s}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("ArrayCopyGate.java");
    std::fs::write(&src, SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 编译失败:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir
}

const SOURCE: &str = r#"
public class ArrayCopyGate {
    // 基本类型拷贝 + 读回求和:arraycopy(src,0,dst,0,5) 后 dst = {2,3,5,7,11} → 和 28。
    public static int copyAndSum() {
        int[] src = {2, 3, 5, 7, 11};
        int[] dst = new int[5];
        System.arraycopy(src, 0, dst, 0, 5);
        return dst[0] + dst[1] + dst[2] + dst[3] + dst[4];
    }

    // 同数组重叠平移(memmove):{1,2,3,4} 从 0 拷 3 到 1 → {1,1,2,3};读 dst[3] = 3。
    public static int overlapShift() {
        int[] a = {1, 2, 3, 4};
        System.arraycopy(a, 0, a, 1, 3);
        return a[3];
    }

    // 越界:length 超出 dst → ArrayIndexOutOfBoundsException。
    public static void outOfBounds() {
        int[] src = {1, 2};
        int[] dst = new int[1];
        System.arraycopy(src, 0, dst, 0, 2);
    }
}
"#;

/// 跑 `ArrayCopyGate.<name>`,返回 `Ok(Value)` 或 `Err(异常内部名)`(供异常用例)。
fn run_static(vm: &mut VmThread, name: &str, desc: &str) -> Result<Value, String> {
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("ArrayCopyGate").expect("ArrayCopyGate 须已注册");
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            use rustj::constant_pool::ConstantPoolEntry;
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == desc);
            n && d
        })
        .unwrap_or_else(|| panic!("未找到 ArrayCopyGate.{name}{desc}"));
    let code = method.code.as_ref().expect("须有 Code");
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    match interp.interpret_with(&mut frame, vm) {
        Ok(v) => Ok(v),
        Err(VmError::ThrownException(r)) => {
            let exc_name = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance Oop:{o:?})"),
            };
            Err(exc_name)
        }
        Err(e) => Err(format!("内部错误:{e:?}")),
    }
}

#[test]
fn system_arraycopy_end_to_end() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = compile_gate();
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("ArrayCopyGate.class")).unwrap())
        .expect("解析应成功");
    registry.load(cf).expect("加载应成功");

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // 预载 System(其 arraycopy 为 ACC_NATIVE → 内置 native 分派)及其引用闭包。
    load_closure(&mut registry, &cp, "java/lang/System").unwrap();
    assert!(registry.get("java/lang/System").is_some(), "System 须已预载");

    let mut vm = VmThread::new(registry);

    // 基本 int 拷贝 + 读回求和 → 28。
    match run_static(&mut vm, "copyAndSum", "()I") {
        Ok(Value::Int(n)) => assert_eq!(n, 28, "copyAndSum 须为 28(2+3+5+7+11)"),
        other => panic!("copyAndSum 意外:{other:?}"),
    }

    // 同数组重叠平移(memmove 后向):a[3] = 3。
    match run_static(&mut vm, "overlapShift", "()I") {
        Ok(Value::Int(n)) => assert_eq!(n, 3, "overlapShift 后 a[3] 须为 3(memmove)"),
        other => panic!("overlapShift 意外:{other:?}"),
    }

    // 越界 → AIOOBE。
    match run_static(&mut vm, "outOfBounds", "()V") {
        Ok(v) => panic!("outOfBounds 应抛 AIOOBE,却返回 {v:?}"),
        Err(cls) => assert_eq!(
            cls, "java/lang/ArrayIndexOutOfBoundsException",
            "outOfBounds 应抛 AIOOBE"
        ),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
