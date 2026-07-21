//! 探测闸门(下一个真实 java.base 缺口):`java.lang.StringBuilder`。
//!
//! 若绿:证明真实 StringBuilder(String append + 长度)可端到端跑;若红:首个失败即下一实现层。
//! 需 `javac` + 本机 `java.base.jmod`;缺一则跳过。

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Value, VmThread, VmError};
use rustj::testkit::*;

const SOURCE: &str = r#"
public class SbOps {
    // StringBuilder.append(String) ×2 → length 6。
    public static int appendLen() {
        StringBuilder sb = new StringBuilder();
        sb.append("foo");
        sb.append("bar");
        return sb.length();
    }
    // StringBuilder.append(int) → "42" → length 2。
    public static int appendIntLen() {
        StringBuilder sb = new StringBuilder();
        sb.append(42);
        return sb.length();
    }
}
"#;

fn run_int(vm: &mut VmThread, name: &str) -> Result<i32, VmError> {
    use rustj::constant_pool::ConstantPoolEntry;
    let reg = vm.registry().expect("类注册表");
    let lc = reg.get("SbOps").expect("SbOps 须已加载");
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
        other => panic!("SbOps.{name} 应返 int,得 {other:?}"),
    }
}

/// **探测**:真 StringBuilder append/length。
#[test]
fn real_string_builder() {
    require_javac!();
    require_javabase!(jmod);

    let dir = compile_dir(SOURCE, "SbOps", &[]);
    let mut registry = ClassRegistry::new();
    let cf = rustj::classfile::parse(&std::fs::read(dir.join("SbOps.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    // 预载 StringBuilder 闭包(连带 AbstractStringBuilder/Arrays/System/Integer 等)。
    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    load_closure(&mut registry, &cp, "java/lang/StringBuilder").unwrap();

    let mut vm = VmThread::new(registry);
    let len = run_int(&mut vm, "appendLen").unwrap_or_else(|e| panic!("appendLen 失败:{e:?}"));
    assert_eq!(len, 6, "append(\"foo\")+append(\"bar\") → length 6");
    let ilen = run_int(&mut vm, "appendIntLen").unwrap_or_else(|e| panic!("appendIntLen 失败:{e:?}"));
    assert_eq!(ilen, 2, "append(42) → \"42\" → length 2");
}
