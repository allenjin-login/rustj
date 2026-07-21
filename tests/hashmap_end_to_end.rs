//! 集成闸门(Layer 4.10z):**真 `java.util.HashMap` 端到端**(经真 java.base 字节码)。
//!
//! 承 4.10y(ArrayList):HashMap 的 put/get/size/containsKey 同样全程真字节码,无新实现缺口。
//! HashMap 经哈希表桶 + Node 链 + `Integer.hashCode`/`equals`(真字节码 unbox 比较)+ 扩容
//! (`System.arraycopy`/`arraycopy`)——证明集合族(数组表 + 哈希表两类核心结构)端到端可跑。
//!
//! **关键约束(同 4.10h/4.10y):整段程序须共用同一 `Vm`**——静态字段(`VM.savedProps`)值是 Vm
//! 堆句柄,堆随 Vm 析构失效,故引导与运行同 Vm。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.util.HashMap;
public class HashMapGate {
    // put + get:哈希定位桶 + Node 链 + Integer.hashCode/equals。
    public static int putGetSum() {
        HashMap<Integer,Integer> m = new HashMap<>();
        m.put(1, 100); m.put(2, 200); m.put(3, 300);
        return m.get(1) + m.get(2) + m.get(3);
    }
    // size:字段返回。
    public static int sizeTwo() {
        HashMap<Integer,Integer> m = new HashMap<>();
        m.put(1, 1); m.put(2, 2);
        return m.size();
    }
    // containsKey:遍历桶 + equals。
    public static int containsHit() {
        HashMap<Integer,Integer> m = new HashMap<>();
        m.put(7, 70);
        return m.containsKey(7) ? 1 : 0;
    }
    // overwrite:put 已有键覆盖值。
    public static int overwrite() {
        HashMap<Integer,Integer> m = new HashMap<>();
        m.put(5, 50);
        m.put(5, 500);
        return m.get(5);
    }
}
"#;

const BOOTSTRAP_SRC: &str = r#"
import java.util.HashMap;
class RustjBootstrap {
    static void init() {
        jdk.internal.misc.VM.saveProperties(new HashMap<String, String>());
    }
}
"#;

/// **集成闸门**:真 `java.util.HashMap` 端到端。
#[test]
fn real_hashmap_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "HashMapGate", &[]);
    let bdir = compile_dir(
        BOOTSTRAP_SRC,
        "RustjBootstrap",
        &["--add-exports", "java.base/jdk.internal.misc=ALL-UNNAMED"],
    );
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("HashMapGate.class")).unwrap()).unwrap())
        .unwrap();
    registry
        .load(rustj::classfile::parse(&std::fs::read(bdir.join("RustjBootstrap.class")).unwrap()).unwrap())
        .unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in ["java/util/HashMap", "java/lang/Integer", "java/util/Map", "java/lang/String"] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }

    let mut vm = VmThread::new(registry);
    run_static_in(&mut vm, "RustjBootstrap", "init", "()V").expect("引导不应抛异常");

    assert_eq!(
        run_static_in(&mut vm, "HashMapGate", "putGetSum", "()I").unwrap(),
        Value::Int(600),
        "put(1=100,2=200,3=300) 求和"
    );
    assert_eq!(
        run_static_in(&mut vm, "HashMapGate", "sizeTwo", "()I").unwrap(),
        Value::Int(2),
        "size()"
    );
    assert_eq!(
        run_static_in(&mut vm, "HashMapGate", "containsHit", "()I").unwrap(),
        Value::Int(1),
        "containsKey(7)"
    );
    assert_eq!(
        run_static_in(&mut vm, "HashMapGate", "overwrite", "()I").unwrap(),
        Value::Int(500),
        "put 覆盖已有键"
    );
}
