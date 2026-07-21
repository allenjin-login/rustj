//! 集成闸门(Layer 4.10w 候选):**真 `java.lang.String` 的 `substring` / `charAt` / `length`
//! 端到端**(经 javac 编的真字节码)。
//!
//! 与仅测 `equals`/`hashCode`/`intern`(4.10i)不同:本闸门驱动 `substring`(经
//! `Arrays.copyOfRange` → `System.arraycopy` 分配新 String + Latin1 字节复制)与
//! `charAt`(Latin1 单字节读取)真字节码。预载真 String 闭包(String + Arrays + System 等)。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};
use rustj::testkit::*;

const SOURCE: &str = r#"
public class StrOps {
    // substring(0,5) of "hello world" → "hello" → 长度 5。
    public static int subLen() {
        String s = "hello world";
        String t = s.substring(0, 5);
        return t.length();
    }
    // charAt 累加:'j'(106) + 'a'(97) = 203。
    public static int charCode() {
        String s = "java";
        return s.charAt(0) + s.charAt(1);
    }
}
"#;

fn run_int(vm: &mut VmThread, name: &str) -> Result<i32, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("StrOps 须已加载");
    let lc = reg.get("StrOps").expect("StrOps 须已加载");
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == name);
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .unwrap();
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table)
        .with_identity(lc.name(), name);
    match interp.interpret_with(&mut frame, vm)? {
        Value::Int(n) => Ok(n),
        other => panic!("StrOps.{name} 应返 int,得 {other:?}"),
    }
}

/// **集成闸门**:真 String 的 substring/charAt/length。
#[test]
fn real_string_substring_and_charat() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "StrOps", &[]);
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("StrOps.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载真 String 闭包(substring/charAt 跑真字节码;substring 经 Arrays.copyOfRange
    // → System.arraycopy,故闭包含 String/Arrays/System 等真类)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();

    let mut vm = VmThread::new(registry);
    let char_code = run_int(&mut vm, "charCode").unwrap_or_else(|e| {
        panic!("charCode 运行失败(真 String.charAt 链缺口):{e:?}")
    });
    assert_eq!(char_code, 203, "charAt(0)+charAt(1) = 'j'+'a' = 106+97");
    let sub_len = run_int(&mut vm, "subLen").unwrap_or_else(|e| {
        panic!("subLen 运行失败(真 String.substring 链缺口):{e:?}")
    });
    assert_eq!(sub_len, 5, "substring(0,5) of \"hello world\" 长度 5");
}
