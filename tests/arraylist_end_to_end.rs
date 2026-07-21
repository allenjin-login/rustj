//! 集成闸门(Layer 4.10y):**真 `java.util.ArrayList` 端到端**(经真 java.base 字节码)。
//!
//! 解锁链:dup_x1/栈操作族(4.10x)消除了 `elementData[size++]=e` 的 `UnsupportedOpcode(DupX1)`;
//! 加上系统属性引导(4.10h 的 `RustjBootstrap.init()`→`VM.saveProperties`,供 autoboxing 的
//! `Integer.valueOf`→`IntegerCache.<clinit>` 读 `savedProps`),`ArrayList<Integer>` 的
//! `add`/`get`/`size`/`indexOf`/`remove` 全程跑真字节码(经 `System.arraycopy`、`equals` 等)。
//!
//! **关键约束(同 4.10h):整段程序须共用同一 `Vm`**——静态字段(`VM.savedProps`)值是 Vm 堆句柄,
//! 堆随 Vm 析构失效,故引导与运行同 Vm(对应真实 JVM 单一全局堆贯穿整个程序)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Value, VmThread};
use rustj::testkit::*;

const SOURCE: &str = r#"
import java.util.ArrayList;
public class ArrListGate {
    // add + get:autoboxing(Integer.valueOf 命中 IntegerCache)+ 数组扩容 + 越界检查。
    public static int addGetSum() {
        ArrayList<Integer> list = new ArrayList<>();
        list.add(10); list.add(20); list.add(30);
        return list.get(0) + list.get(1) + list.get(2);
    }
    // size:字段返回。
    public static int sizeThree() {
        ArrayList<Integer> list = new ArrayList<>();
        list.add(1); list.add(2); list.add(3);
        return list.size();
    }
    // indexOf:遍历 + Integer.equals(真字节码 unbox 比较)。
    public static int indexOfMiddle() {
        ArrayList<Integer> list = new ArrayList<>();
        list.add(7); list.add(42); list.add(7);
        return list.indexOf(42);
    }
    // remove(int):经 System.arraycopy 左移 + size--。
    public static int removeReturnsVictim() {
        ArrayList<Integer> list = new ArrayList<>();
        list.add(1); list.add(2); list.add(3);
        return list.remove(1);  // 返回被删的 2
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

/// **集成闸门**:真 `java.util.ArrayList` 端到端。
#[test]
fn real_arraylist_end_to_end() {
    require_javac!();
    require_javabase!(jmod);

    // 1) javac 编 ArrListGate + RustjBootstrap;载入注册表。
    let dir = compile_dir(SOURCE, "ArrListGate", &[]);
    let bdir = compile_dir(
        BOOTSTRAP_SRC,
        "RustjBootstrap",
        &["--add-exports", "java.base/jdk.internal.misc=ALL-UNNAMED"],
    );
    let mut registry = ClassRegistry::new();
    registry
        .load(rustj::classfile::parse(&std::fs::read(dir.join("ArrListGate.class")).unwrap()).unwrap())
        .unwrap();
    registry
        .load(rustj::classfile::parse(&std::fs::read(bdir.join("RustjBootstrap.class")).unwrap()).unwrap())
        .unwrap();

    // 2) 真 java.base.jmod 入 ClassPath;闭包预载 ArrayList(及其引用)+ autoboxing/bootstrap 依赖。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    for cls in [
        "java/util/ArrayList",
        "java/lang/Integer",
        "java/util/HashMap",
        "java/lang/String",
    ] {
        load_closure(&mut registry, &cp, cls).unwrap();
    }

    // 3) 引导 + 运行须共用同一 Vm(静态字段值是 Vm 堆句柄)。
    let mut vm = VmThread::new(registry);
    run_static_in(&mut vm, "RustjBootstrap", "init", "()V").expect("引导不应抛异常");

    assert_eq!(
        run_static_in(&mut vm, "ArrListGate", "addGetSum", "()I").unwrap(),
        Value::Int(60),
        "add(10,20,30) 求和"
    );
    assert_eq!(
        run_static_in(&mut vm, "ArrListGate", "sizeThree", "()I").unwrap(),
        Value::Int(3),
        "size()"
    );
    assert_eq!(
        run_static_in(&mut vm, "ArrListGate", "indexOfMiddle", "()I").unwrap(),
        Value::Int(1),
        "indexOf(42)"
    );
    assert_eq!(
        run_static_in(&mut vm, "ArrListGate", "removeReturnsVictim", "()I").unwrap(),
        Value::Int(2),
        "remove(1) 返回被删元素"
    );
}
