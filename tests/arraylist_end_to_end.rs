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

/// javac 编译单个类到唯一临时目录,返回该目录。`extra` 追加 javac 参数(如 `--add-exports`)。
fn compile_dir(source: &str, public_name: &str, extra: &[&str]) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rustj-arrlist-{n}-{}-{public_name}",
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

/// 解释执行一个**无参静态方法**,共用调用者传入的 `Vm`。Java 异常时把类名带出便于诊断。
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
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

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
    let mut vm = Vm::new(registry);
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
