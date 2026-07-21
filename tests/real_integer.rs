//! 集成闸门(4.10h,转绿):`load_closure` 预载 `java/lang/Integer` 的整个引用闭包
//! (真类覆盖桩),先跑 `RustjBootstrap.init()` 引导 `VM.savedProps`(等价 launcher 的
//! `System.initializeSystemClass` 片段),再跑 `Integer.valueOf(42).intValue()` → 42——
//! 端到端验证**真实 java.base 类的 `<clinit>`(IntegerCache)+ 静态方法 + 构造器 + 实例字段**
//! 全链经解释器执行。
//!
//! 选 Integer(而非 String):Integer 是普通类(无 `Oop::String` 那种特殊变体冲突),`valueOf`/
//! `intValue` 均为真字节码,直击「加载并运行真实 java.base 类」北极星。
//!
//! **4.10h 解锁链(Step 0 源码核对):** `Integer$IntegerCache.<clinit>` 调
//! `VM.getSavedProperty` —— `VM.getSavedProperty`(VM.java:209)于 `savedProps==null` 抛
//! `IllegalStateException("Not yet initialized")`。引导 = `VM.saveProperties(new HashMap<>())`
//! (VM.java:237):`savedProps=props` 后,`directMemory = Runtime.getRuntime().maxMemory()`
//! (Runtime.<clinit>=`new Runtime()`,`<init>` 空体;maxMemory native),`pageAlignDirectMemory =
//! "true".equals(props.get(...))`(空表 `HashMap.get` 经 `table==null` 短路返 null,故仅
//! `String.hashCode`+`String.equals(null)`;String 脚手架见 native.rs)。Runtime 已在
//! Integer 闭包内(经 VM.saveProperties 字节码引用)传递性载入。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::constant_pool::ConstantPoolEntry;
use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
public class IntegerGate {
    // Integer.valueOf(42):命中 IntegerCache(-128..127)→ 缓存实例;intValue() 返回 value 字段 → 42。
    // 全程用真 java.base 的 Integer(Number 子类、Comparable 实现)+ 其 <clinit>(IntegerCache)。
    public static int run() {
        return Integer.valueOf(42).intValue();
    }
}
"#;

/// 系统属性引导(等价 launcher 的 `System.initializeSystemClass` 片段):在用户代码前设
/// `VM.savedProps` 为空 `HashMap`——否则 `VM.getSavedProperty` 抛 `IllegalStateException`,
/// `Integer$IntegerCache.<clinit>` 即失败。需 `--add-exports` 才能编译期访问 `jdk.internal.misc`。
const BOOTSTRAP_SRC: &str = r#"
import java.util.HashMap;
class RustjBootstrap {
    static void init() {
        jdk.internal.misc.VM.saveProperties(new HashMap<String, String>());
    }
}
"#;

/// **集成闸门**:真 `java/lang/Integer`(经闭包预载)的 `<clinit>` + `valueOf` + 构造器 + `intValue`
/// 端到端。先运行 `RustjBootstrap.init()` 设 `savedProps`,再跑 `IntegerGate.run()` → 42。
#[test]
fn real_integer_valueof_intvalue_runs() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编译 IntegerGate + RustjBootstrap;载入注册表。
    let dir = compile_dir(SOURCE, "IntegerGate", &[]);
    let bdir = compile_dir(
        BOOTSTRAP_SRC,
        "RustjBootstrap",
        &["--add-exports", "java.base/jdk.internal.misc=ALL-UNNAMED"],
    );
    let mut registry = ClassRegistry::new();
    let ng = rustj::classfile::parse(&std::fs::read(dir.join("IntegerGate.class")).unwrap()).unwrap();
    registry.load(ng).unwrap();
    let bsc = rustj::classfile::parse(&std::fs::read(bdir.join("RustjBootstrap.class")).unwrap()).unwrap();
    registry.load(bsc).unwrap();

    // 2) 真 java.base.jmod 加入 ClassPath;闭包预载 Integer(及其引用:Number/Comparable/Object/
    //    Integer$IntegerCache/jdk.internal.misc.VM/…,真类覆盖合成桩);并显式预载 HashMap
    //    (RustjBootstrap 的 `new HashMap` 与 saveProperties 的 `Map` 形参类型所需)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    let loaded = load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();
    assert!(loaded >= 1, "闭包应载入 Integer 本身,实际:{loaded}");
    load_closure(&mut registry, &cp, "java/util/HashMap").unwrap();
    assert!(registry.get("java/util/HashMap").is_some(), "HashMap 须已预载");
    // 真 String 须预载(4.10i):引导链 VM.saveProperties → HashMap.get(键 String.hashCode)
    // 与 "true".equals(...) 现走真 String 字节码(分派 StringLatin1),非 4.10h 的 native 桩。
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    assert!(registry.get("java/lang/String").is_some(), "String 须已预载");

    let registry = std::sync::Arc::new(registry);

    // 3) 真 Integer.intValue 须**非** native(桩无此法 → 证覆盖成功)。
    let int_lc = registry.get("java/lang/Integer").expect("Integer 须已注册");
    let int_value = int_lc
        .cf
        .methods
        .iter()
        .find(|m| {
            let n = matches!(int_lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "intValue");
            let d = matches!(int_lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .expect("真 Integer 须有 intValue()I");
    assert!(!int_value.access_flags.is_native(), "真 Integer.intValue 须非 native(真字节码)");

    // 4) 系统属性引导 + 5) IntegerGate.run() 须共用同一 Vm:静态字段(VM.savedProps)存于
    //    注册表,但其值是 Vm 堆句柄——堆随 Vm 析构失效,故引导与运行同 Vm(对应真实 JVM 单一
    //    全局堆贯穿整个程序的约定)。
    let mut vm = VmThread::new(std::sync::Arc::clone(&registry));
    // 系统属性引导:跑 RustjBootstrap.init()(= VM.saveProperties(new HashMap<>()),等价 launcher
    // 的 System.initializeSystemClass 片段)。savedProps 一旦非 null,后续 IntegerCache.<clinit>
    // 的 VM.getSavedProperty 即不再抛 IllegalStateException。抛异常即明确暴露下一缺口(类名)。
    if let Err(exc) = run_static_in(&mut vm, "RustjBootstrap", "init", "()V") {
        panic!("引导 RustjBootstrap.init 抛异常:{exc}");
    }

    // 5) 跑 IntegerGate.run():Integer.<clinit>→ IntegerCache.<clinit>(VM.getSavedProperty 现可读
    //    savedProps)→ valueOf(42)(命中缓存)→ intValue()→ 42。复用同一 Vm(同上堆约束)。
    assert_eq!(
        run_static_in(&mut vm, "IntegerGate", "run", "()I").unwrap(),
        Value::Int(42),
        "valueOf(42).intValue() 须为 42"
    );
}
