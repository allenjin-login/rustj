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

fn compile_dir(source: &str, public_name: &str, extra: &[&str]) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-hashmap-{n}-{}-{public_name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join(format!("{public_name}.java"));
    std::fs::write(&src, source).unwrap();
    let out = Command::new("javac")
        .args(extra)
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

fn run_static_in(vm: &mut Vm, class: &str, name: &str, desc: &str) -> Result<Value, String> {
    let reg = vm.registry().unwrap_or_else(|| panic!("类注册表"));
    let lc = reg.get(class).unwrap_or_else(|| panic!("类 {class} 未加载"));
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
        .unwrap_or_else(|| panic!("未找到方法 {class}.{name}{desc}"));
    let code = method.code.as_ref().unwrap_or_else(|| panic!("{name} 应有 Code"));
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
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
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

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

    let mut vm = Vm::new(registry);
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
