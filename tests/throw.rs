//! 集成闸门(Layer 4.7):javac 编 try/catch/finally 的真实 Java,由 rustj 执行,
//! 验证 `athrow` + 异常表分派 + 跨帧(invoke)异常传播与 JVM 一致。需 `javac`(无则跳过)。
//!
//! 范围:(1) 用户 `athrow` 抛出的异常,经本帧或调用者帧异常表捕获;
//! (2) **JVM 抛出**的运行时异常(NPE/ArithmeticException/AIOOBE/CCE)被 javac 编的
//! try/catch 捕获——验证 Stage B 将其统一为 `ThrownException` 后与 javac 的 catch 表一致;
//! (3) `catch` 超类型(`Exception`/`Throwable`)经引导桩层次 `is_instance` 命中子类。
//! 异常根类(`Throwable`/`Exception`/`NullPointerException` 等)由引导桩(Stage A)装好。

use rustj::testkit::*;

const SOURCE: &str = r#"
public class ThrowGate {
    // 用 RuntimeException(非受检)派生:javac 不强制 catch/throws 声明,故本闸门可
    // 自由构造"未抛出的 catch""仅 finally 无 catch"等场景。派生类本身均**已加载**,
    // 故 is_instance 的 exact/超类型判定不受未加载根类影响(catch RuntimeException/
    // Throwable 需加载根类,留待 4.7b)。
    static class BaseExc extends RuntimeException {}
    static class SubExc extends BaseExc {}
    static class OtherExc extends RuntimeException {}

    // 1. 同帧精确捕获:抛 SubExc,catch SubExc
    public static int caughtExact() {
        try {
            throw new SubExc();
        } catch (SubExc e) {
            return 1;
        }
    }

    // 2. 同帧超类型捕获:抛 SubExc,catch BaseExc(已加载 → is_instance 命中)
    public static int caughtSuper() {
        try {
            throw new SubExc();
        } catch (BaseExc e) {
            return 2;
        }
    }

    // 3. 不匹配 catch 被跳过,后续匹配 catch 命中(表内顺序即优先级)
    public static int caughtAfterSkip() {
        try {
            throw new SubExc();
        } catch (OtherExc e) {
            return 10;   // SubExc 不是 OtherExc → 跳过
        } catch (BaseExc e) {
            return 3;    // SubExc 是 BaseExc → 命中
        }
    }

    // 4. 跨帧捕获:调用者 try/catch,被调用者 thrower() 抛出(invoke 异常传播)
    public static int caughtCrossFrame() {
        try {
            thrower();
            return 0;    // 不应到达
        } catch (SubExc e) {
            return 4;
        }
    }
    static void thrower() {
        throw new SubExc();
    }

    // 5. catch-all(catch_type 0)via finally:抛 SubExc,finally 覆盖返回
    public static int caughtFinally() {
        try {
            throw new SubExc();
        } finally {
            return 5;
        }
    }

    // 6. 未捕获:无处理者,异常向上传播出 interpret_with
    public static int uncaught() throws SubExc {
        throw new SubExc();
    }

    // ---- JVM 抛出的运行时异常(Stage B 统一为 ThrownException)被 javac try/catch 捕获 ----
    // 触发 rustj 解释器内部抛出的标准异常,验证其类名与 catch 子句一致(不匹配则传播 → run 失败)。
    static class A {}
    static class B {}

    // 7. arraylength on null → NullPointerException
    public static int catchNpe() {
        int[] a = null;
        try {
            return a.length;
        } catch (NullPointerException e) {
            return 1;
        }
    }

    // 8. 整数除零 → ArithmeticException(数组取值避免 javac 常量折叠)
    public static int catchArithmetic() {
        int[] z = new int[1];
        try {
            return 100 / z[0];
        } catch (ArithmeticException e) {
            return 1;
        }
    }

    // 9. 数组越界 → ArrayIndexOutOfBoundsException
    public static int catchAioobe() {
        int[] a = new int[2];
        try {
            return a[5];
        } catch (ArrayIndexOutOfBoundsException e) {
            return 1;
        }
    }

    // 10. checkcast 失败 → ClassCastException(A 实例强转 B)
    public static int catchCce() {
        Object o = new A();
        try {
            B b = (B) o;
            return 0;
        } catch (ClassCastException e) {
            return 1;
        }
    }

    // 11. catch 超类型:NPE 被 catch(Exception) 捕获(经引导桩层次 is_instance)
    public static int catchNpeAsException() {
        int[] a = null;
        try {
            return a.length;
        } catch (Exception e) {
            return 1;
        }
    }

    // 12. catch 根类型:NPE 被 catch(Throwable) 捕获
    public static int catchNpeAsThrowable() {
        int[] a = null;
        try {
            return a.length;
        } catch (Throwable e) {
            return 1;
        }
    }
}
"#;

#[test]
fn caught_exact_type() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtExact", "()I")), 1);
}

#[test]
fn caught_supertype() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtSuper", "()I")), 2);
}

#[test]
fn caught_after_skipping_non_matching() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtAfterSkip", "()I")), 3);
}

#[test]
fn caught_cross_frame_via_invoke() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtCrossFrame", "()I")), 4);
}

#[test]
fn caught_finally_catch_all() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "caughtFinally", "()I")), 5);
}

#[test]
fn uncaught_propagates_as_thrown_exception() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_is_thrown!(run_err(&reg, "ThrowGate", "uncaught", "()I"));
}

// ---- JVM 抛出的运行时异常(Stage B 统一)被 javac try/catch 捕获 ----

#[test]
fn jvm_thrown_npe_is_caught() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchNpe", "()I")), 1);
}

#[test]
fn jvm_thrown_arithmetic_is_caught() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchArithmetic", "()I")), 1);
}

#[test]
fn jvm_thrown_aioobe_is_caught() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchAioobe", "()I")), 1);
}

#[test]
fn jvm_thrown_cce_is_caught() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchCce", "()I")), 1);
}

#[test]
fn jvm_thrown_npe_caught_by_supertype_exception() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchNpeAsException", "()I")), 1);
}

#[test]
fn jvm_thrown_npe_caught_by_throwable() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "ThrowGate");
    let reg = std::sync::Arc::new(reg);
    assert_eq!(as_int(run(&reg, "ThrowGate", "catchNpeAsThrowable", "()I")), 1);
}
