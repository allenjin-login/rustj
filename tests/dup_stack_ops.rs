//! 集成闸门(Layer 4.10x):**dup/swap 栈操作指令族**经 `javac` 真字节码端到端验证。
//!
//! 这些模式(`a[0]+=v`、链式字段/数组赋值、`a[i]++`)是 javac 日常产出,此前一律
//! `UnsupportedOpcode(DupX1/Dup2/...)`——ArrayList 的 `elementData[size++]=e` 即卡在 dup_x1。
//! 本闸门用默认 javac(无任何 `-XD` 退路)编出覆盖**全部 dup 形式**的方法:
//! - `bumpArr`(`a[0]+=5`)→ **dup2**;`chainArr`(`a[0]=a[1]=7`)→ **dup_x2**;
//! - `chainIntField`(`d.f=(d.f=7)`)→ **dup_x1**;`chainLongField`(`d.f2=(d.f2=5L)`)→ **dup2_x1**;
//! - `chainLongArr`(`g[0]=g[0]=5L`)→ **dup2_x2**;`postInc`(`a[0]++`)→ dup/dup2/dup_x2。
//!
//! 需 PATH 中 `javac`(无则跳过)。`new DupGate()` 经隐式 `<init>` → `Object.<init>`(引导桩),
//! 同 `object_fields` 的 `new Point()` 路径。

use rustj::runtime::Value;
use rustj::testkit::*;

const SOURCE: &str = r#"
public class DupGate {
    int f; long f2;
    // dup2:int[] 复合赋值 a[0]+=5
    public static int bumpArr() {
        int[] a = new int[1];
        a[0] = 10;
        a[0] += 5;
        return a[0];
    }
    // dup_x2:链式 int 数组赋值 a[0]=a[1]=7
    public static int chainArr() {
        int[] a = new int[2];
        a[0] = a[1] = 7;
        return a[0] + a[1];
    }
    // dup_x1:链式 int 字段赋值 d.f=(d.f=7)
    public static int chainIntField() {
        DupGate d = new DupGate();
        d.f = (d.f = 7);
        return d.f;
    }
    // dup2_x1:链式 long 字段赋值 d.f2=(d.f2=5L)
    public static long chainLongField() {
        DupGate d = new DupGate();
        d.f2 = (d.f2 = 5L);
        return d.f2;
    }
    // dup2_x2:链式 long 数组赋值 g[0]=g[0]=5L
    public static long chainLongArr() {
        long[] g = new long[1];
        g[0] = g[0] = 5L;
        return g[0];
    }
    // dup/dup2/dup_x2:数组元素后自增,返回旧值
    public static int postInc() {
        int[] a = new int[]{9};
        return a[0]++;
    }
}
"#;

/// **集成闸门**:dup/swap 族真字节码端到端(全形式)。
#[test]
fn dup_family_real_bytecode() {
    require_javac!();
    let registry = compile_and_load(SOURCE, "DupGate");
    let registry = std::sync::Arc::new(registry);

    assert_eq!(run(&registry, "DupGate", "bumpArr", "()I"), Value::Int(15), "a[0]+=5");
    assert_eq!(run(&registry, "DupGate", "chainArr", "()I"), Value::Int(14), "a[0]=a[1]=7");
    assert_eq!(run(&registry, "DupGate", "chainIntField", "()I"), Value::Int(7), "d.f=(d.f=7)");
    assert_eq!(run(&registry, "DupGate", "chainLongField", "()J"), Value::Long(5), "d.f2=(d.f2=5L)");
    assert_eq!(run(&registry, "DupGate", "chainLongArr", "()J"), Value::Long(5), "g[0]=g[0]=5L");
    assert_eq!(run(&registry, "DupGate", "postInc", "()I"), Value::Int(9), "a[0]++ 返回旧值");
}
