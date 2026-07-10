//! 集成闸门(Layer 4.10m):真 `java.lang.String` 端到端后的**字符串拼接**闸门。
//!
//! `s + s`(s 为非常量局部变量 → javac 编 `StringBuilder.append/toString`,预 JDK9 风格,
//! 经 `-XDstringConcat=inline` 强制)。"abc"+"abc" = "abcabc" → length() = 6。
//! 串联 4.10l(`System.arraycopy`)与 4.10m(`Float`/`Double` 位转换 native,解锁
//! `Math.<clinit>` → `Arrays.copyOfRange` → `String.<init>`)。曾为研究探针(逐缺口下移),
//! 现 **String concat 链全绿** → 转为回归闸门:任何抛出即拼接链回归。
//!
//! 需 `javac`(PATH)与本机 `java.base.jmod`;缺一则跳过。

use std::path::{Path, PathBuf};
use std::process::Command;

use rustj::oops::ClassRegistry;
use rustj::runtime::class_loader::class_path::ClassPath;
use rustj::runtime::class_loader::loader::load_closure;
use rustj::runtime::{Frame, Interpreter, Vm, VmError};

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

const SOURCE: &str = r#"
public class StringConcat {
    // s 非常量(局部变量)→ javac 编为 StringBuilder;s+s = "abcabc" → 6。
    public static int selfConcatLength() {
        String s = "abc";
        return (s + s).length();
    }
}
"#;

#[test]
fn string_concat_end_to_end() {
    if !javac_available() {
        eprintln!("跳过:无 javac");
        return;
    }
    let Some(jmod) = find_javabase_jmod() else {
        eprintln!("跳过:无 java.base.jmod");
        return;
    };

    let dir = std::env::temp_dir().join(format!(
        "rustj-concat-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("StringConcat.java");
    std::fs::write(&src, SOURCE).unwrap();
    let out = Command::new("javac")
        .arg("-XDstringConcat=inline") // 强制 StringBuilder(预 JDK9 风格),避开 invokedynamic
        .arg("-d")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("javac 执行失败");
    assert!(
        out.status.success(),
        "javac 失败:{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let mut registry = ClassRegistry::new();
    let cf =
        rustj::classfile::parse(&std::fs::read(dir.join("StringConcat.class")).unwrap()).unwrap();
    registry.load(cf).unwrap();

    let bytes = std::fs::read(&jmod).unwrap();
    let mut cp = ClassPath::new();
    cp.add("java.base.jmod", &bytes).unwrap();
    // 预载拼接链核心类(load_closure 拉入引用闭包)。
    load_closure(&mut registry, &cp, "java/lang/String").unwrap();
    load_closure(&mut registry, &cp, "java/lang/StringBuilder").unwrap();

    let registry = std::sync::Arc::new(registry);
    let lc = registry.get("StringConcat").unwrap();
    let method = lc
        .cf
        .methods
        .iter()
        .find(|m| {
            use rustj::constant_pool::ConstantPoolEntry;
            let n = matches!(lc.cf.constant_pool.get(m.name_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "selfConcatLength");
            let d = matches!(lc.cf.constant_pool.get(m.descriptor_index), Ok(ConstantPoolEntry::Utf8(s)) if s == "()I");
            n && d
        })
        .unwrap();
    let code = method.code.as_ref().unwrap();
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    let interp =
        Interpreter::new(&code.code, &lc.cf.constant_pool).with_exception_table(&code.exception_table);
    let mut vm = Vm::new(std::sync::Arc::clone(&registry));

    match interp.interpret_with(&mut frame, &mut vm) {
        Ok(rustj::runtime::Value::Int(n)) => {
            assert_eq!(n, 6, "\"abc\"+\"abc\".length() 应为 6");
        }
        Ok(other) => panic!("concat 闸门意外返回:{other:?}(期望 Int 6)"),
        Err(VmError::ThrownException(r)) => {
            let cls = match vm.heap().get(r) {
                Some(rustj::oops::Oop::Instance(i)) => i.class_name().to_string(),
                o => format!("(非 Instance:{o:?})"),
            };
            // 拼接链已全绿;抛出即回归(见 4.10l arraycopy / 4.10m Float/Double 位转换)。
            panic!("concat 闸门回归:拼接链抛出 {cls}");
        }
        Err(e) => panic!("concat 闸门内部错误:{e:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}