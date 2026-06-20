//! rustj demo:解析一个 `.class` 文件并打印结构概览。
//!
//! 用法:`cargo run -- <path/to/Foo.class>`

use std::env;
use std::process::ExitCode;

use rustj::classfile::parse;
use rustj::constant_pool::ConstantPoolEntry;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let path = match args.get(1) {
        Some(p) => p,
        None => {
            eprintln!(
                "用法: {} <Foo.class>",
                args.first().map(String::as_str).unwrap_or("rustj")
            );
            return ExitCode::from(2);
        }
    };

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("无法读取 {path}: {e}");
            return ExitCode::from(1);
        }
    };

    match parse(&bytes) {
        Ok(cf) => print_summary(&cf),
        Err(e) => {
            eprintln!("解析失败: {e}");
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

fn print_summary(cf: &rustj::metadata::ClassFile) {
    println!(
        "版本:        major={} minor={}",
        cf.major_version, cf.minor_version
    );
    println!("访问标志:    0x{:04X}", cf.access_flags.bits());
    println!("本类:        {}", cf.this_class_name().unwrap_or("?"));
    println!("父类:        {}", cf.super_class_name().unwrap_or("(none)"));
    println!("接口数:      {}", cf.interfaces.len());
    println!("常量池条目:  {}", cf.constant_pool.len());
    println!("字段数:      {}", cf.fields.len());
    println!("方法数:      {}", cf.methods.len());

    for (i, m) in cf.methods.iter().enumerate() {
        let name = utf8_at(cf, m.name_index);
        let desc = utf8_at(cf, m.descriptor_index);
        let code_info = match &m.code {
            Some(c) => format!(
                "max_stack={} max_locals={} code={}B",
                c.max_stack,
                c.max_locals,
                c.code.len()
            ),
            None => "(无 Code)".to_string(),
        };
        println!(
            "  方法[{i}] {name}{desc}  flags=0x{:04X}  {code_info}",
            m.access_flags.bits()
        );
    }
}

/// 取常量池中某索引处的 Utf8 字符串(用于展示)。
fn utf8_at(cf: &rustj::metadata::ClassFile, index: u16) -> String {
    match cf.constant_pool.get(index) {
        Ok(ConstantPoolEntry::Utf8(s)) => s.clone(),
        _ => format!("#{index}?"),
    }
}
