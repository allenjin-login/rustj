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

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::interpreter::launch::initialize_system_class;
use rustj::runtime::VmThread;
use rustj::testkit::*;

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

/// **集成闸门**(Layer 4.29):`String.startsWith`(prefix len > 7)经 `vectorizedMismatch` →
/// `Unsafe.getLong(byte[], 16)`(8 对齐)做向量化比较。修前 getLong native 未登记 →
/// `UnsatisfiedLinkError` 传出 startsWith。修后:相等同缀返 true、首字节差返 false。
#[test]
fn unsafe_get_long_supports_vectorized_mismatch() {
    require_javac!();
    require_javabase!(jmod);
    let dir = compile_dir(MISMATCH_PROBE_SOURCE, "MismatchProbe", &[]);

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

    let mut vm = VmThread::new(registry);
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
