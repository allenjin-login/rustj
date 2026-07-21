//! 集成闸门(多实参 invoke 槽位顺序):证 `invokevirtual`/`invokespecial` 把多个实参
//! **按声明顺序**写入被调用者局部变量(arg0→local1、arg1→local2 …)。
//!
//! 背景:`invoke_*` 自调用者栈逆序弹实参 → 须 `reverse()` 翻回正序再写入。
//! `invoke_static` 早已翻转,但 virtual/special/interface 三路曾漏 `reverse()` →
//! 多实参实例/构造器调用的局部变量槽位整体倒置。单实参路径(equals(Object)、length()
//! 等)对此无感,故长期潜伏;直到真 `String.getBytes([BIB)V`(3 实参)以
//! `Frame(TypeMismatch)` 暴露。
//!
//! 本闸门用两个**不可交换**运算门钉死槽位顺序:错排即值错,非崩溃。
//! 需 `javac`(PATH);缺则跳过。

use rustj::testkit::*;



const SOURCE: &str = r#"
public class MultiArg {
    int x;
    int y;

    // invokespecial <init>(II):两个 int 实参 → x=a、y=b。
    // 错排(无 reverse)→ x=b、y=a。
    MultiArg(int a, int b) {
        this.x = a;
        this.y = b;
    }

    // invokevirtual combine(III):三 int 实参,不可交换编码 p*100+q*10+r。
    // 正序 (100,30,5) → 10305;错排(局部变量倒置)→ 900。
    int combine(int p, int q, int r) {
        return p * 100 + q * 10 + r;
    }

    // 构造器实参顺序门:new (100,30) → x-y = 70;错排 → -70。
    public static int ctorOrder() {
        MultiArg m = new MultiArg(100, 30);
        return m.x - m.y;
    }

    // 实例方法实参顺序门:combine(100,30,5) → 10305。
    public static int callOrder() {
        MultiArg m = new MultiArg(0, 0);
        return m.combine(100, 30, 5);
    }
}
"#;

#[test]
fn invokespecial_two_args_keep_order() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "MultiArg");
    let reg = std::sync::Arc::new(reg);
    // 正确 70;若 invokespecial 漏 reverse → ctor 把 b 写入 x、a 写入 y → -70。
    assert_eq!(as_int(run(&reg, "MultiArg", "ctorOrder", "()I")), 70);
}

#[test]
fn invokevirtual_three_args_keep_order() {
    require_javac!();
    let reg = compile_and_load(SOURCE, "MultiArg");
    let reg = std::sync::Arc::new(reg);
    // 正确 10305;若 invokevirtual 漏 reverse → 实参倒置入局部变量 → 900。
    assert_eq!(as_int(run(&reg, "MultiArg", "callOrder", "()I")), 10305);
}
