//! 测试断言辅助:Value 取值 + 类型/值断言宏 + 异常断言宏(集成测试用)。
//!
//! `as_int` 提取 Int(其他类型 as_long/as_double/as_float 未建——普查显示 tests/ 无用户,
//! 直接用 `assert_eq!(..., Value::Long(n))` 或本模块的 assert_long!/assert_double!/assert_float!)。
//! 浮点断言用固定容差:double 1e-9、float 1e-6(标准化 tests/ 现有内联值)。

use crate::runtime::Value;

/// 取 `Value::Int` 的 i32;非 Int 则 panic。
pub fn as_int(v: Value) -> i32 {
    match v {
        Value::Int(x) => x,
        other => panic!("期望 int,得 {other:?}"),
    }
}

/// 断言 `$v` 为 `Value::Int($n)`。
#[macro_export]
macro_rules! assert_int {
    ($v:expr, $n:expr) => {{
        let v = $v;
        match v {
            $crate::runtime::Value::Int(got) => assert_eq!(got, $n),
            other => panic!("assert_int!:期望 Int({}), 得 {:?}", $n, other),
        }
    }};
}

/// 断言 `$v` 为 `Value::Long($n)`。
#[macro_export]
macro_rules! assert_long {
    ($v:expr, $n:expr) => {{
        let v = $v;
        match v {
            $crate::runtime::Value::Long(got) => assert_eq!(got, $n),
            other => panic!("assert_long!:期望 Long({}), 得 {:?}", $n, other),
        }
    }};
}

/// 断言 `$v` 为 `Value::Double($n)`(容差 1e-9)。
#[macro_export]
macro_rules! assert_double {
    ($v:expr, $n:expr) => {{
        let v = $v;
        match v {
            $crate::runtime::Value::Double(got) => {
                assert!(
                    (got - $n).abs() < 1e-9,
                    "assert_double!:期望 {} 得 {}",
                    $n,
                    got
                );
            }
            other => panic!("assert_double!:期望 Double({}), 得 {:?}", $n, other),
        }
    }};
}

/// 断言 `$v` 为 `Value::Float($n)`(容差 1e-6)。
#[macro_export]
macro_rules! assert_float {
    ($v:expr, $n:expr) => {{
        let v = $v;
        match v {
            $crate::runtime::Value::Float(got) => {
                assert!(
                    (got - $n).abs() < 1e-6,
                    "assert_float!:期望 {} 得 {}",
                    $n,
                    got
                );
            }
            other => panic!("assert_float!:期望 Float({}), 得 {:?}", $n, other),
        }
    }};
}

/// 断言 `$err` 为 `VmError::ThrownException(_)`。
#[macro_export]
macro_rules! assert_is_thrown {
    ($err:expr) => {{
        let err = $err;
        assert!(
            matches!(err, $crate::runtime::VmError::ThrownException(_)),
            "assert_is_thrown!:期望 ThrownException, 得 {:?}",
            err
        );
    }};
}

/// 断言 `$result` 为 `Err(VmError::ThrownException(r))`,且堆上 `r` 指向的异常实例
/// 类名 == `$expected`(内部名,如 "java/lang/ArithmeticException")。
/// `$vm` 用于读堆(`vm.heap().get(r)`)。
#[macro_export]
macro_rules! assert_throws {
    ($result:expr, $vm:expr, $expected:expr) => {{
        let result = $result;
        let exc = match result {
            Err($crate::runtime::VmError::ThrownException(r)) => r,
            other => panic!("assert_throws!:期望 Err(ThrownException({})), 得 {:?}", $expected, other),
        };
        match $vm.heap().get(exc) {
            Some($crate::oops::Oop::Instance(i)) => {
                assert_eq!(i.class_name(), $expected, "异常类名不符")
            }
            o => panic!("assert_throws!:异常应为 Instance, 得 {:?}", o),
        }
    }};
}
