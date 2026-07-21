//! 集成闸门(Layer 4.10l):真 `java/lang/System.arraycopy`(static native)端到端。
//!
//! javac 编一个用 `System.arraycopy` 的真 Java 程序:基本类型拷贝 + 读回求和、
//! 同数组重叠平移(memmove)、越界→`ArrayIndexOutOfBoundsException`。`System` 经闭包
//! 从本机 `java.base.jmod` 预载(其 `arraycopy` 为 ACC_NATIVE → 走内置 native 分派表,
//! 即 `arraycopy::system_arraycopy`)。语义对照 HotSpot
//! `typeArrayKlass::copy_array` / `objArrayKlass::copy_array`。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

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

#[test]
fn system_arraycopy_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "ArrayCopyGate", &[]);
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
    assert_eq!(
        run_static_in(&mut vm, "ArrayCopyGate", "copyAndSum", "()I").unwrap(),
        Value::Int(28),
        "copyAndSum 须为 28(2+3+5+7+11)"
    );

    // 同数组重叠平移(memmove 后向):a[3] = 3。
    assert_eq!(
        run_static_in(&mut vm, "ArrayCopyGate", "overlapShift", "()I").unwrap(),
        Value::Int(3),
        "overlapShift 后 a[3] 须为 3(memmove)"
    );

    // 越界 → AIOOBE。
    assert_throws!(
        run_static_in(&mut vm, "ArrayCopyGate", "outOfBounds", "()V"),
        &mut vm,
        "java/lang/ArrayIndexOutOfBoundsException"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
