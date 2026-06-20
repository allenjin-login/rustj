//! 类型描述符解析(JVMS §4.3):字段描述符与方法描述符。
//!
//! 例:`Ljava/lang/String;`、`[I`、`(IJLjava/lang/String;)V`。

use std::fmt;
use std::iter::Peekable;
use std::str::Chars;

use crate::classfile::ClassFileError;

fn bad(descriptor: &str) -> ClassFileError {
    ClassFileError::InvalidDescriptor {
        descriptor: descriptor.to_string(),
    }
}

/// 从字符迭代器解析一个字段类型。不消费后续多余字符。
fn parse_field_type(
    chars: &mut Peekable<Chars<'_>>,
    source: &str,
) -> Result<FieldType, ClassFileError> {
    let c = chars.next().ok_or_else(|| bad(source))?;
    Ok(match c {
        'B' => FieldType::Byte,
        'C' => FieldType::Char,
        'D' => FieldType::Double,
        'F' => FieldType::Float,
        'I' => FieldType::Int,
        'J' => FieldType::Long,
        'S' => FieldType::Short,
        'Z' => FieldType::Boolean,
        'L' => {
            let mut name = String::new();
            loop {
                let ch = chars.next().ok_or_else(|| bad(source))?;
                if ch == ';' {
                    break;
                }
                name.push(ch);
            }
            if name.is_empty() {
                return Err(bad(source));
            }
            FieldType::Class(name)
        }
        '[' => {
            let component = parse_field_type(chars, source)?;
            FieldType::Array(Box::new(component))
        }
        _ => return Err(bad(source)),
    })
}

/// 字段类型(基本类型、对象类型、数组类型)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    Byte,
    Char,
    Double,
    Float,
    Int,
    Long,
    Short,
    Boolean,
    /// 对象类型,内含类的内部名(如 `java/lang/String`)。
    Class(String),
    /// 数组类型,内含元素类型。
    Array(Box<FieldType>),
}

/// 返回描述符:某字段类型,或 `void`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnDescriptor {
    Void,
    FieldType(FieldType),
}

/// 方法描述符:形参类型列表 + 返回类型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDescriptor {
    pub parameters: Vec<FieldType>,
    pub return_type: ReturnDescriptor,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Byte => f.write_str("B"),
            Self::Char => f.write_str("C"),
            Self::Double => f.write_str("D"),
            Self::Float => f.write_str("F"),
            Self::Int => f.write_str("I"),
            Self::Long => f.write_str("J"),
            Self::Short => f.write_str("S"),
            Self::Boolean => f.write_str("Z"),
            Self::Class(name) => write!(f, "L{name};"),
            Self::Array(inner) => write!(f, "[{inner}"),
        }
    }
}

/// 解析单个字段描述符;要求整串恰好是一个字段类型。
pub fn parse_field_descriptor(s: &str) -> Result<FieldType, ClassFileError> {
    let mut chars = s.chars().peekable();
    let ty = parse_field_type(&mut chars, s)?;
    // 整串必须被恰好消费一个字段类型,不允许多余字符。
    if chars.next().is_some() {
        return Err(bad(s));
    }
    Ok(ty)
}

/// 解析方法描述符,如 `(IJLjava/lang/String;)V`。
pub fn parse_method_descriptor(s: &str) -> Result<MethodDescriptor, ClassFileError> {
    let mut chars = s.chars().peekable();
    if chars.next() != Some('(') {
        return Err(bad(s));
    }
    let mut parameters = Vec::new();
    while chars.peek() != Some(&')') {
        parameters.push(parse_field_type(&mut chars, s)?);
    }
    // 消费 ')'
    chars.next();
    let return_type = match chars.peek() {
        Some(&'V') => {
            chars.next();
            ReturnDescriptor::Void
        }
        _ => ReturnDescriptor::FieldType(parse_field_type(&mut chars, s)?),
    };
    if chars.next().is_some() {
        return Err(bad(s));
    }
    Ok(MethodDescriptor {
        parameters,
        return_type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_types() {
        assert_eq!(parse_field_descriptor("I").unwrap(), FieldType::Int);
        assert_eq!(parse_field_descriptor("J").unwrap(), FieldType::Long);
        assert_eq!(parse_field_descriptor("Z").unwrap(), FieldType::Boolean);
        assert_eq!(parse_field_descriptor("D").unwrap(), FieldType::Double);
    }

    #[test]
    fn parses_object_type() {
        assert_eq!(
            parse_field_descriptor("Ljava/lang/String;").unwrap(),
            FieldType::Class("java/lang/String".to_string())
        );
    }

    #[test]
    fn parses_single_dimension_array() {
        assert_eq!(
            parse_field_descriptor("[I").unwrap(),
            FieldType::Array(Box::new(FieldType::Int))
        );
    }

    #[test]
    fn parses_multi_dimension_array_of_objects() {
        assert_eq!(
            parse_field_descriptor("[[Ljava/lang/Object;").unwrap(),
            FieldType::Array(Box::new(FieldType::Array(Box::new(FieldType::Class(
                "java/lang/Object".to_string()
            )))))
        );
    }

    #[test]
    fn rejects_non_field_descriptor() {
        assert!(parse_field_descriptor("").is_err());
        assert!(parse_field_descriptor("X").is_err());
        assert!(parse_field_descriptor("Ljava/lang/String").is_err()); // 缺分号
        assert!(parse_field_descriptor("II").is_err()); // 多余字符
    }

    #[test]
    fn parses_method_descriptor_void() {
        let d = parse_method_descriptor("(IJLjava/lang/String;)V").unwrap();
        assert_eq!(
            d.parameters,
            vec![
                FieldType::Int,
                FieldType::Long,
                FieldType::Class("java/lang/String".to_string()),
            ]
        );
        assert_eq!(d.return_type, ReturnDescriptor::Void);
    }

    #[test]
    fn parses_no_arg_returning_int() {
        let d = parse_method_descriptor("()I").unwrap();
        assert!(d.parameters.is_empty());
        assert_eq!(d.return_type, ReturnDescriptor::FieldType(FieldType::Int));
    }

    #[test]
    fn rejects_bad_method_descriptor() {
        assert!(parse_method_descriptor("()").is_err()); // 缺返回
        assert!(parse_method_descriptor("(I").is_err()); // 缺右括号
        assert!(parse_method_descriptor("I").is_err()); // 缺左括号
    }
}
