//! JVM 标准操作码(JVMS §6.5),判别值即操作码字节(0–202)。
//!
//! 变体按 JVMS 字节顺序声明、配合 `#[repr(u8)]` 的顺序判别值,
//! 使 `opcode as u8` 直接给出操作码字节。助记符与格式由**变体键控**的
//! 单一 `info()` 匹配提供(每个变体与其名称/格式局部成对,杜绝平行数组的错位漂移)。

use std::fmt;

/// 操作码解析错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytecodeError {
    /// 未知/保留的操作码字节(含 254/255 等保留码)。
    UnknownOpcode(u8),
}

impl fmt::Display for BytecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOpcode(b) => write!(f, "unknown/reserved opcode: 0x{b:02X} ({b})"),
        }
    }
}
impl std::error::Error for BytecodeError {}

/// JVM 标准操作码。判别值即 class 文件中的操作码字节。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    // 0x00–0x0f 常量加载
    Nop = 0,
    AconstNull,
    IconstM1,
    Iconst0,
    Iconst1,
    Iconst2,
    Iconst3,
    Iconst4,
    Iconst5,
    Lconst0,
    Lconst1,
    Fconst0,
    Fconst1,
    Fconst2,
    Dconst0,
    Dconst1,
    // 0x10–0x14 常量压栈
    Bipush,
    Sipush,
    Ldc,
    LdcW,
    Ldc2W,
    // 0x15–0x19 load(带 u1 索引)
    Iload,
    Lload,
    Fload,
    Dload,
    Aload,
    // 0x1a–0x2d *load_0..3
    Iload0, Iload1, Iload2, Iload3,
    Lload0, Lload1, Lload2, Lload3,
    Fload0, Fload1, Fload2, Fload3,
    Dload0, Dload1, Dload2, Dload3,
    Aload0, Aload1, Aload2, Aload3,
    // 0x2e–0x35 数组 load
    Iaload, Laload, Faload, Daload, Aaload, Baload, Caload, Saload,
    // 0x36–0x3a store(带 u1 索引)
    Istore, Lstore, Fstore, Dstore, Astore,
    // 0x3b–0x4e *store_0..3
    Istore0, Istore1, Istore2, Istore3,
    Lstore0, Lstore1, Lstore2, Lstore3,
    Fstore0, Fstore1, Fstore2, Fstore3,
    Dstore0, Dstore1, Dstore2, Dstore3,
    Astore0, Astore1, Astore2, Astore3,
    // 0x4f–0x56 数组 store
    Iastore, Lastore, Fastore, Dastore, Aastore, Bastore, Castore, Sastore,
    // 0x57–0x5f 栈操作
    Pop, Pop2, Dup, DupX1, DupX2, Dup2, Dup2X1, Dup2X2, Swap,
    // 0x60–0x77 算术
    Iadd, Ladd, Fadd, Dadd,
    Isub, Lsub, Fsub, Dsub,
    Imul, Lmul, Fmul, Dmul,
    Idiv, Ldiv, Fdiv, Ddiv,
    Irem, Lrem, Frem, Drem,
    Ineg, Lneg, Fneg, Dneg,
    // 0x78–0x83 移位与位运算
    Ishl, Lshl, Ishr, Lshr, Iushr, Lushr, Iand, Land, Ior, Lor, Ixor, Lxor,
    // 0x84 iinc
    Iinc,
    // 0x85–0x93 类型转换
    I2l, I2f, I2d, L2i, L2f, L2d, F2i, F2l, F2d, D2i, D2l, D2f, I2b, I2c, I2s,
    // 0x94–0x98 比较
    Lcmp, Fcmpl, Fcmpg, Dcmpl, Dcmpg,
    // 0x99–0xa8 条件分支与跳转
    Ifeq, Ifne, Iflt, Ifge, Ifgt, Ifle,
    IfIcmpeq, IfIcmpne, IfIcmplt, IfIcmpge, IfIcmpgt, IfIcmple,
    IfAcmpeq, IfAcmpne,
    Goto, Jsr, Ret,
    // 0xaa–0xab switch
    Tableswitch, Lookupswitch,
    // 0xac–0xb1 返回
    Ireturn, Lreturn, Freturn, Dreturn, Areturn, Return,
    // 0xb2–0xb5 字段访问
    Getstatic, Putstatic, Getfield, Putfield,
    // 0xb6–0xba 方法调用
    Invokevirtual, Invokespecial, Invokestatic, Invokeinterface, Invokedynamic,
    // 0xbb–0xbe 对象/数组
    New, Newarray, Anewarray, Arraylength,
    // 0xbf athrow
    Athrow,
    // 0xc0–0xc1 类型检查
    Checkcast, Instanceof,
    // 0xc2–0xc3 监视器
    Monitorenter, Monitorexit,
    // 0xc4 wide
    Wide,
    // 0xc5 multianewarray
    Multianewarray,
    // 0xc6–0xc9 空判断与宽跳转
    Ifnull, Ifnonnull, GotoW, JsrW,
    // 0xca breakpoint
    Breakpoint,
}

/// 指令的操作数布局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 无操作数,长度 1。
    None,
    /// u1 操作数,长度 2(bipush / ldc / newarray / *load #idx / *store #idx / ret)。
    U1,
    /// s2 操作数,长度 3(sipush)。
    S2,
    /// u2 操作数,长度 3(ldc_w、ldc2_w、getstatic、invokevirtual、new 等)。
    U2,
    /// s2 分支,长度 3(if* / goto / jsr)。
    Branch,
    /// s4 分支,长度 5(goto_w / jsr_w)。
    BranchWide,
    /// iinc:u1 局部变量索引 + s1 增量,长度 3。
    Iinc,
    /// multianewarray:u2 + u1 维度,长度 4。
    Multianewarray,
    /// invokeinterface:u2 + u1 + u1,长度 5。
    InvokeInterface,
    /// invokedynamic:u2 + u1 + u1,长度 5。
    InvokeDynamic,
    /// 变长(tableswitch / lookupswitch / wide)。
    Variable,
}

impl Format {
    /// 固定指令长度(含操作码字节);变长指令返回 `None`。
    pub const fn length(self) -> Option<usize> {
        match self {
            Self::None => Some(1),
            Self::U1 => Some(2),
            Self::S2 | Self::U2 | Self::Branch | Self::Iinc => Some(3),
            Self::Multianewarray => Some(4),
            Self::BranchWide | Self::InvokeInterface | Self::InvokeDynamic => Some(5),
            Self::Variable => None,
        }
    }
}

impl Opcode {
    /// 由操作码字节构造;未知/保留字节返回错误。
    pub fn from_u8(b: u8) -> Result<Self, BytecodeError> {
        FROM_BYTE
            .get(usize::from(b))
            .copied()
            .ok_or(BytecodeError::UnknownOpcode(b))
    }

    /// 小写助记符(如 `"iadd"`)。
    pub fn name(self) -> &'static str {
        self.info().0
    }

    /// 操作数布局。
    pub fn format(self) -> Format {
        self.info().1
    }

    /// 固定指令长度;变长指令返回 `None`。
    pub fn length(self) -> Option<usize> {
        self.format().length()
    }

    /// 名称与布局的单一来源:变体键控匹配,杜绝平行数组错位。
    #[rustfmt::skip]
    fn info(self) -> (&'static str, Format) {
        use Format::*;
        match self {
            Opcode::Nop => ("nop", None),
            Opcode::AconstNull => ("aconst_null", None),
            Opcode::IconstM1 => ("iconst_m1", None),
            Opcode::Iconst0 => ("iconst_0", None),
            Opcode::Iconst1 => ("iconst_1", None),
            Opcode::Iconst2 => ("iconst_2", None),
            Opcode::Iconst3 => ("iconst_3", None),
            Opcode::Iconst4 => ("iconst_4", None),
            Opcode::Iconst5 => ("iconst_5", None),
            Opcode::Lconst0 => ("lconst_0", None),
            Opcode::Lconst1 => ("lconst_1", None),
            Opcode::Fconst0 => ("fconst_0", None),
            Opcode::Fconst1 => ("fconst_1", None),
            Opcode::Fconst2 => ("fconst_2", None),
            Opcode::Dconst0 => ("dconst_0", None),
            Opcode::Dconst1 => ("dconst_1", None),
            Opcode::Bipush => ("bipush", U1),
            Opcode::Sipush => ("sipush", S2),
            Opcode::Ldc => ("ldc", U1),
            Opcode::LdcW => ("ldc_w", U2),
            Opcode::Ldc2W => ("ldc2_w", U2),
            Opcode::Iload => ("iload", U1),
            Opcode::Lload => ("lload", U1),
            Opcode::Fload => ("fload", U1),
            Opcode::Dload => ("dload", U1),
            Opcode::Aload => ("aload", U1),
            Opcode::Iload0 => ("iload_0", None),
            Opcode::Iload1 => ("iload_1", None),
            Opcode::Iload2 => ("iload_2", None),
            Opcode::Iload3 => ("iload_3", None),
            Opcode::Lload0 => ("lload_0", None),
            Opcode::Lload1 => ("lload_1", None),
            Opcode::Lload2 => ("lload_2", None),
            Opcode::Lload3 => ("lload_3", None),
            Opcode::Fload0 => ("fload_0", None),
            Opcode::Fload1 => ("fload_1", None),
            Opcode::Fload2 => ("fload_2", None),
            Opcode::Fload3 => ("fload_3", None),
            Opcode::Dload0 => ("dload_0", None),
            Opcode::Dload1 => ("dload_1", None),
            Opcode::Dload2 => ("dload_2", None),
            Opcode::Dload3 => ("dload_3", None),
            Opcode::Aload0 => ("aload_0", None),
            Opcode::Aload1 => ("aload_1", None),
            Opcode::Aload2 => ("aload_2", None),
            Opcode::Aload3 => ("aload_3", None),
            Opcode::Iaload => ("iaload", None),
            Opcode::Laload => ("laload", None),
            Opcode::Faload => ("faload", None),
            Opcode::Daload => ("daload", None),
            Opcode::Aaload => ("aaload", None),
            Opcode::Baload => ("baload", None),
            Opcode::Caload => ("caload", None),
            Opcode::Saload => ("saload", None),
            Opcode::Istore => ("istore", U1),
            Opcode::Lstore => ("lstore", U1),
            Opcode::Fstore => ("fstore", U1),
            Opcode::Dstore => ("dstore", U1),
            Opcode::Astore => ("astore", U1),
            Opcode::Istore0 => ("istore_0", None),
            Opcode::Istore1 => ("istore_1", None),
            Opcode::Istore2 => ("istore_2", None),
            Opcode::Istore3 => ("istore_3", None),
            Opcode::Lstore0 => ("lstore_0", None),
            Opcode::Lstore1 => ("lstore_1", None),
            Opcode::Lstore2 => ("lstore_2", None),
            Opcode::Lstore3 => ("lstore_3", None),
            Opcode::Fstore0 => ("fstore_0", None),
            Opcode::Fstore1 => ("fstore_1", None),
            Opcode::Fstore2 => ("fstore_2", None),
            Opcode::Fstore3 => ("fstore_3", None),
            Opcode::Dstore0 => ("dstore_0", None),
            Opcode::Dstore1 => ("dstore_1", None),
            Opcode::Dstore2 => ("dstore_2", None),
            Opcode::Dstore3 => ("dstore_3", None),
            Opcode::Astore0 => ("astore_0", None),
            Opcode::Astore1 => ("astore_1", None),
            Opcode::Astore2 => ("astore_2", None),
            Opcode::Astore3 => ("astore_3", None),
            Opcode::Iastore => ("iastore", None),
            Opcode::Lastore => ("lastore", None),
            Opcode::Fastore => ("fastore", None),
            Opcode::Dastore => ("dastore", None),
            Opcode::Aastore => ("aastore", None),
            Opcode::Bastore => ("bastore", None),
            Opcode::Castore => ("castore", None),
            Opcode::Sastore => ("sastore", None),
            Opcode::Pop => ("pop", None),
            Opcode::Pop2 => ("pop2", None),
            Opcode::Dup => ("dup", None),
            Opcode::DupX1 => ("dup_x1", None),
            Opcode::DupX2 => ("dup_x2", None),
            Opcode::Dup2 => ("dup2", None),
            Opcode::Dup2X1 => ("dup2_x1", None),
            Opcode::Dup2X2 => ("dup2_x2", None),
            Opcode::Swap => ("swap", None),
            Opcode::Iadd => ("iadd", None),
            Opcode::Ladd => ("ladd", None),
            Opcode::Fadd => ("fadd", None),
            Opcode::Dadd => ("dadd", None),
            Opcode::Isub => ("isub", None),
            Opcode::Lsub => ("lsub", None),
            Opcode::Fsub => ("fsub", None),
            Opcode::Dsub => ("dsub", None),
            Opcode::Imul => ("imul", None),
            Opcode::Lmul => ("lmul", None),
            Opcode::Fmul => ("fmul", None),
            Opcode::Dmul => ("dmul", None),
            Opcode::Idiv => ("idiv", None),
            Opcode::Ldiv => ("ldiv", None),
            Opcode::Fdiv => ("fdiv", None),
            Opcode::Ddiv => ("ddiv", None),
            Opcode::Irem => ("irem", None),
            Opcode::Lrem => ("lrem", None),
            Opcode::Frem => ("frem", None),
            Opcode::Drem => ("drem", None),
            Opcode::Ineg => ("ineg", None),
            Opcode::Lneg => ("lneg", None),
            Opcode::Fneg => ("fneg", None),
            Opcode::Dneg => ("dneg", None),
            Opcode::Ishl => ("ishl", None),
            Opcode::Lshl => ("lshl", None),
            Opcode::Ishr => ("ishr", None),
            Opcode::Lshr => ("lshr", None),
            Opcode::Iushr => ("iushr", None),
            Opcode::Lushr => ("lushr", None),
            Opcode::Iand => ("iand", None),
            Opcode::Land => ("land", None),
            Opcode::Ior => ("ior", None),
            Opcode::Lor => ("lor", None),
            Opcode::Ixor => ("ixor", None),
            Opcode::Lxor => ("lxor", None),
            Opcode::Iinc => ("iinc", Iinc),
            Opcode::I2l => ("i2l", None),
            Opcode::I2f => ("i2f", None),
            Opcode::I2d => ("i2d", None),
            Opcode::L2i => ("l2i", None),
            Opcode::L2f => ("l2f", None),
            Opcode::L2d => ("l2d", None),
            Opcode::F2i => ("f2i", None),
            Opcode::F2l => ("f2l", None),
            Opcode::F2d => ("f2d", None),
            Opcode::D2i => ("d2i", None),
            Opcode::D2l => ("d2l", None),
            Opcode::D2f => ("d2f", None),
            Opcode::I2b => ("i2b", None),
            Opcode::I2c => ("i2c", None),
            Opcode::I2s => ("i2s", None),
            Opcode::Lcmp => ("lcmp", None),
            Opcode::Fcmpl => ("fcmpl", None),
            Opcode::Fcmpg => ("fcmpg", None),
            Opcode::Dcmpl => ("dcmpl", None),
            Opcode::Dcmpg => ("dcmpg", None),
            Opcode::Ifeq => ("ifeq", Branch),
            Opcode::Ifne => ("ifne", Branch),
            Opcode::Iflt => ("iflt", Branch),
            Opcode::Ifge => ("ifge", Branch),
            Opcode::Ifgt => ("ifgt", Branch),
            Opcode::Ifle => ("ifle", Branch),
            Opcode::IfIcmpeq => ("if_icmpeq", Branch),
            Opcode::IfIcmpne => ("if_icmpne", Branch),
            Opcode::IfIcmplt => ("if_icmplt", Branch),
            Opcode::IfIcmpge => ("if_icmpge", Branch),
            Opcode::IfIcmpgt => ("if_icmpgt", Branch),
            Opcode::IfIcmple => ("if_icmple", Branch),
            Opcode::IfAcmpeq => ("if_acmpeq", Branch),
            Opcode::IfAcmpne => ("if_acmpne", Branch),
            Opcode::Goto => ("goto", Branch),
            Opcode::Jsr => ("jsr", Branch),
            Opcode::Ret => ("ret", U1),
            Opcode::Tableswitch => ("tableswitch", Variable),
            Opcode::Lookupswitch => ("lookupswitch", Variable),
            Opcode::Ireturn => ("ireturn", None),
            Opcode::Lreturn => ("lreturn", None),
            Opcode::Freturn => ("freturn", None),
            Opcode::Dreturn => ("dreturn", None),
            Opcode::Areturn => ("areturn", None),
            Opcode::Return => ("return", None),
            Opcode::Getstatic => ("getstatic", U2),
            Opcode::Putstatic => ("putstatic", U2),
            Opcode::Getfield => ("getfield", U2),
            Opcode::Putfield => ("putfield", U2),
            Opcode::Invokevirtual => ("invokevirtual", U2),
            Opcode::Invokespecial => ("invokespecial", U2),
            Opcode::Invokestatic => ("invokestatic", U2),
            Opcode::Invokeinterface => ("invokeinterface", InvokeInterface),
            Opcode::Invokedynamic => ("invokedynamic", InvokeDynamic),
            Opcode::New => ("new", U2),
            Opcode::Newarray => ("newarray", U1),
            Opcode::Anewarray => ("anewarray", U2),
            Opcode::Arraylength => ("arraylength", None),
            Opcode::Athrow => ("athrow", None),
            Opcode::Checkcast => ("checkcast", U2),
            Opcode::Instanceof => ("instanceof", U2),
            Opcode::Monitorenter => ("monitorenter", None),
            Opcode::Monitorexit => ("monitorexit", None),
            Opcode::Wide => ("wide", Variable),
            Opcode::Multianewarray => ("multianewarray", Multianewarray),
            Opcode::Ifnull => ("ifnull", Branch),
            Opcode::Ifnonnull => ("ifnonnull", Branch),
            Opcode::GotoW => ("goto_w", BranchWide),
            Opcode::JsrW => ("jsr_w", BranchWide),
            Opcode::Breakpoint => ("breakpoint", None),
        }
    }
}

/// 标准操作码总数(0–202)。
pub const STANDARD_OPCODE_COUNT: usize = 203;

/// 字节 → Opcode 表(顺序 = 字节值)。由 `from_byte_table_is_consistent` 锁定正确性。
const FROM_BYTE: [Opcode; STANDARD_OPCODE_COUNT] = [
    Opcode::Nop, Opcode::AconstNull, Opcode::IconstM1, Opcode::Iconst0, Opcode::Iconst1,
    Opcode::Iconst2, Opcode::Iconst3, Opcode::Iconst4, Opcode::Iconst5, Opcode::Lconst0,
    Opcode::Lconst1, Opcode::Fconst0, Opcode::Fconst1, Opcode::Fconst2, Opcode::Dconst0,
    Opcode::Dconst1,
    Opcode::Bipush, Opcode::Sipush, Opcode::Ldc, Opcode::LdcW, Opcode::Ldc2W,
    Opcode::Iload, Opcode::Lload, Opcode::Fload, Opcode::Dload, Opcode::Aload,
    Opcode::Iload0, Opcode::Iload1, Opcode::Iload2, Opcode::Iload3,
    Opcode::Lload0, Opcode::Lload1, Opcode::Lload2, Opcode::Lload3,
    Opcode::Fload0, Opcode::Fload1, Opcode::Fload2, Opcode::Fload3,
    Opcode::Dload0, Opcode::Dload1, Opcode::Dload2, Opcode::Dload3,
    Opcode::Aload0, Opcode::Aload1, Opcode::Aload2, Opcode::Aload3,
    Opcode::Iaload, Opcode::Laload, Opcode::Faload, Opcode::Daload, Opcode::Aaload,
    Opcode::Baload, Opcode::Caload, Opcode::Saload,
    Opcode::Istore, Opcode::Lstore, Opcode::Fstore, Opcode::Dstore, Opcode::Astore,
    Opcode::Istore0, Opcode::Istore1, Opcode::Istore2, Opcode::Istore3,
    Opcode::Lstore0, Opcode::Lstore1, Opcode::Lstore2, Opcode::Lstore3,
    Opcode::Fstore0, Opcode::Fstore1, Opcode::Fstore2, Opcode::Fstore3,
    Opcode::Dstore0, Opcode::Dstore1, Opcode::Dstore2, Opcode::Dstore3,
    Opcode::Astore0, Opcode::Astore1, Opcode::Astore2, Opcode::Astore3,
    Opcode::Iastore, Opcode::Lastore, Opcode::Fastore, Opcode::Dastore, Opcode::Aastore,
    Opcode::Bastore, Opcode::Castore, Opcode::Sastore,
    Opcode::Pop, Opcode::Pop2, Opcode::Dup, Opcode::DupX1, Opcode::DupX2,
    Opcode::Dup2, Opcode::Dup2X1, Opcode::Dup2X2, Opcode::Swap,
    Opcode::Iadd, Opcode::Ladd, Opcode::Fadd, Opcode::Dadd,
    Opcode::Isub, Opcode::Lsub, Opcode::Fsub, Opcode::Dsub,
    Opcode::Imul, Opcode::Lmul, Opcode::Fmul, Opcode::Dmul,
    Opcode::Idiv, Opcode::Ldiv, Opcode::Fdiv, Opcode::Ddiv,
    Opcode::Irem, Opcode::Lrem, Opcode::Frem, Opcode::Drem,
    Opcode::Ineg, Opcode::Lneg, Opcode::Fneg, Opcode::Dneg,
    Opcode::Ishl, Opcode::Lshl, Opcode::Ishr, Opcode::Lshr, Opcode::Iushr, Opcode::Lushr,
    Opcode::Iand, Opcode::Land, Opcode::Ior, Opcode::Lor, Opcode::Ixor, Opcode::Lxor,
    Opcode::Iinc,
    Opcode::I2l, Opcode::I2f, Opcode::I2d, Opcode::L2i, Opcode::L2f, Opcode::L2d,
    Opcode::F2i, Opcode::F2l, Opcode::F2d, Opcode::D2i, Opcode::D2l, Opcode::D2f,
    Opcode::I2b, Opcode::I2c, Opcode::I2s,
    Opcode::Lcmp, Opcode::Fcmpl, Opcode::Fcmpg, Opcode::Dcmpl, Opcode::Dcmpg,
    Opcode::Ifeq, Opcode::Ifne, Opcode::Iflt, Opcode::Ifge, Opcode::Ifgt, Opcode::Ifle,
    Opcode::IfIcmpeq, Opcode::IfIcmpne, Opcode::IfIcmplt, Opcode::IfIcmpge,
    Opcode::IfIcmpgt, Opcode::IfIcmple, Opcode::IfAcmpeq, Opcode::IfAcmpne,
    Opcode::Goto, Opcode::Jsr, Opcode::Ret,
    Opcode::Tableswitch, Opcode::Lookupswitch,
    Opcode::Ireturn, Opcode::Lreturn, Opcode::Freturn, Opcode::Dreturn, Opcode::Areturn,
    Opcode::Return,
    Opcode::Getstatic, Opcode::Putstatic, Opcode::Getfield, Opcode::Putfield,
    Opcode::Invokevirtual, Opcode::Invokespecial, Opcode::Invokestatic,
    Opcode::Invokeinterface, Opcode::Invokedynamic,
    Opcode::New, Opcode::Newarray, Opcode::Anewarray, Opcode::Arraylength,
    Opcode::Athrow,
    Opcode::Checkcast, Opcode::Instanceof,
    Opcode::Monitorenter, Opcode::Monitorexit,
    Opcode::Wide, Opcode::Multianewarray,
    Opcode::Ifnull, Opcode::Ifnonnull, Opcode::GotoW, Opcode::JsrW,
    Opcode::Breakpoint,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_byte_table_is_consistent() {
        // 锁定 FROM_BYTE 的位置正确性:第 i 项的判别值必须等于 i。
        for (i, op) in FROM_BYTE.iter().enumerate() {
            assert_eq!(*op as u8, i as u8, "FROM_BYTE[{i}] 判别值不匹配");
        }
    }

    #[test]
    fn from_u8_round_trips_known_opcodes() {
        assert_eq!(Opcode::from_u8(0x00).unwrap(), Opcode::Nop);
        assert_eq!(Opcode::from_u8(0x60).unwrap(), Opcode::Iadd);
        assert_eq!(Opcode::from_u8(0xa7).unwrap(), Opcode::Goto);
        assert_eq!(Opcode::from_u8(0xb8).unwrap(), Opcode::Invokestatic);
        assert_eq!(Opcode::from_u8(0xb1).unwrap(), Opcode::Return);
        assert_eq!(Opcode::from_u8(0xca).unwrap(), Opcode::Breakpoint);
        assert_eq!(Opcode::Iadd as u8, 0x60);
        assert_eq!(Opcode::Breakpoint as u8, 0xca);
    }

    #[test]
    fn from_u8_rejects_unknown_bytes() {
        assert_eq!(
            Opcode::from_u8(0xcb).unwrap_err(),
            BytecodeError::UnknownOpcode(0xcb)
        );
        assert_eq!(
            Opcode::from_u8(0xfe).unwrap_err(),
            BytecodeError::UnknownOpcode(0xfe)
        );
    }

    #[test]
    fn names_match_jvms_mnemonics() {
        assert_eq!(Opcode::Nop.name(), "nop");
        assert_eq!(Opcode::IconstM1.name(), "iconst_m1");
        assert_eq!(Opcode::Iload0.name(), "iload_0");
        assert_eq!(Opcode::Iadd.name(), "iadd");
        assert_eq!(Opcode::Invokevirtual.name(), "invokevirtual");
        assert_eq!(Opcode::GotoW.name(), "goto_w");
    }

    #[test]
    fn formats_and_lengths_are_correct() {
        // 无操作数
        assert_eq!(Opcode::Nop.format(), Format::None);
        assert_eq!(Opcode::Nop.length(), Some(1));
        assert_eq!(Opcode::Iadd.length(), Some(1));
        // U1:常量与单字节索引(load/store/ret)
        assert_eq!(Opcode::Bipush.format(), Format::U1);
        assert_eq!(Opcode::Bipush.length(), Some(2));
        assert_eq!(Opcode::Ldc.format(), Format::U1);
        assert_eq!(Opcode::Iload.format(), Format::U1);
        assert_eq!(Opcode::Iload.length(), Some(2));
        assert_eq!(Opcode::Istore.format(), Format::U1);
        assert_eq!(Opcode::Ret.format(), Format::U1);
        assert_eq!(Opcode::Newarray.format(), Format::U1);
        // S2 / U2
        assert_eq!(Opcode::Sipush.format(), Format::S2);
        assert_eq!(Opcode::Sipush.length(), Some(3));
        assert_eq!(Opcode::Getstatic.format(), Format::U2);
        assert_eq!(Opcode::Getstatic.length(), Some(3));
        assert_eq!(Opcode::LdcW.format(), Format::U2);
        // Branch / BranchWide
        assert_eq!(Opcode::Ifeq.format(), Format::Branch);
        assert_eq!(Opcode::Goto.length(), Some(3));
        assert_eq!(Opcode::GotoW.format(), Format::BranchWide);
        assert_eq!(Opcode::GotoW.length(), Some(5));
        // Iinc / Multianewarray / 调用
        assert_eq!(Opcode::Iinc.format(), Format::Iinc);
        assert_eq!(Opcode::Iinc.length(), Some(3));
        assert_eq!(Opcode::Multianewarray.length(), Some(4));
        assert_eq!(Opcode::Invokeinterface.format(), Format::InvokeInterface);
        assert_eq!(Opcode::Invokeinterface.length(), Some(5));
        assert_eq!(Opcode::Invokedynamic.length(), Some(5));
        // 变长
        assert_eq!(Opcode::Tableswitch.format(), Format::Variable);
        assert_eq!(Opcode::Tableswitch.length(), None);
        assert_eq!(Opcode::Wide.length(), None);
    }

    #[test]
    fn table_covers_all_standard_bytes() {
        for b in 0u8..=202 {
            assert!(Opcode::from_u8(b).is_ok(), "byte {b:#x} 应可解析");
        }
        for b in 203u8..=253 {
            assert!(Opcode::from_u8(b).is_err(), "byte {b:#x} 应未知");
        }
    }
}
