//! `jdk/internal/misc/{VM,CDS,Unsafe}` 的 native 桥。语义移植自 `prims/jvm.cpp` 的 `JVM_*`。
//! 由 [`super::dispatch`] 按声明类路由至此。

use crate::oops::Oop;
use crate::runtime::{Reference, Slot, Value, Vm, VmError};

use super::super::throw_exception;

/// `Unsafe` 数组基偏移(`arrayBaseOffset0` 恒返此值)。byte[] 元素 index 的偏移 = 此值 + index,
/// 故 `byte_index` 逆算 `offset - 此值`。与 `arrayBaseOffset0` 返回值同源,内部自洽。
const ARRAY_BYTE_BASE_OFFSET: i64 = 16;

/// `jdk/internal/misc/*` native 分派。未登记 → `UnsatisfiedLinkError`。
pub(super) fn dispatch(
    vm: &mut Vm<'_>,
    class: &str,
    name: &str,
    desc: &str,
    _this: Option<Reference>,
    args: &[Value],
) -> Result<Value, VmError> {
    match (class, name, desc) {
        // jdk.internal.misc.VM.initialize()V —— VM.java:451 私有 native,VM.<clinit> 首调,
        // 做 JDK 启动期一次性引导(保存属性 / 直接内存上限 / …)。rustj 无 launcher 传递的启动态,
        // 此处恒空操作(等价"VM 已初始化,无保存属性"——后续 getSavedProperty 读空表得 null)。
        ("jdk/internal/misc/VM", "initialize", "()V") => Ok(Value::Void),

        // jdk.internal.misc.CDS.initializeFromArchive(Ljava/lang/Class;)V —— CDS.java:130
        // public static native。HotSpot `JVM_InitializeFromArchive`:从 CDS/AOT 归档恢复类的
        // 归档静态状态;无归档(rustj 无 CDS)→ 空操作(归档字段留默认 null)。包装类(Integer/
        // Long/…)$<clinit> 经 `runtimeSetup()` 调之以尝试恢复 archivedCache;空操作后走"新建缓存"
        // 分支,即非 CDS 运行的规范行为。
        ("jdk/internal/misc/CDS", "initializeFromArchive", "(Ljava/lang/Class;)V") => Ok(Value::Void),

        // jdk.internal.misc.CDS.getCDSConfigStatus()I —— CDS.java:95 私有 native,<clinit> 经
        // `configStatus = getCDSConfigStatus()` 调之。HotSpot 返回 CDS 配置位掩码(cdsConfig.hpp:
        // IS_DUMPING_ARCHIVE / IS_USING_ARCHIVE / …);rustj 无 CDS → 恒 0(所有标志关闭),
        // 即 isUsingArchive()/isDumpingArchive()/… 均假——规范的非 CDS 运行。
        ("jdk/internal/misc/CDS", "getCDSConfigStatus", "()I") => Ok(Value::Int(0)),

        // jdk.internal.misc.Unsafe 的数组布局 native —— Unsafe.<clinit> 经
        // `theUnsafe.arrayBaseOffset(X[].class)` / `arrayIndexScale(X[].class)`(皆为**非 native**
        // 字节码包装器)转调私有 native `arrayBaseOffset0` / `arrayIndexScale0`,初始化各
        // ARRAY_*_BASE_OFFSET / _INDEX_SCALE 静态字段(ArraysSupport.<clinit> 读之,进而
        // StringLatin1.hashCode → ArraysSupport.hashCodeOfUnsigned 触发其初始化)。rustj 数组
        // 无真实内存偏移:基偏移取常量、刻度按组件类型大小,仅供偏移算术(mismatch 等);
        // **不参与 String.hashCode 计算**——后者经 `unsignedHashCode` 的朴素 baload 循环。
        ("jdk/internal/misc/Unsafe", "arrayBaseOffset0", "(Ljava/lang/Class;)I") => Ok(Value::Int(16)),
        ("jdk/internal/misc/Unsafe", "arrayIndexScale0", "(Ljava/lang/Class;)I") => {
            let scale = match super::class_arg_name(vm, args).as_deref() {
                Some("[B") | Some("[Z") => 1,
                Some("[C") | Some("[S") => 2,
                Some("[I") | Some("[F") => 4,
                Some("[J") | Some("[D") => 8,
                _ => 1, // 引用数组/未知 → 1(保守;hash 不用此值)
            };
            Ok(Value::Int(scale))
        }

        // jdk.internal.misc.Unsafe 的字节内存原语 —— `DecimalDigits.uncheckedPutCharLatin1`
        // (DecimalDigits.java:440-442)经 `putByte(buf, ARRAY_BYTE_BASE_OFFSET + charPos, (byte)c)`
        // 把数字字节写入 byte[];解锁 StringBuilder.append(int)/Integer.toString 等 int→string 链。
        // `putByte/getByte(Object,long,...)` 为 native(Unsafe.java:219/223)。rustj 无真实偏移内存:
        // byte_index 逆算 `offset - ARRAY_BYTE_BASE_OFFSET`(本模块 arrayBaseOffset0 同源恒 16,
        // byte[] 刻度 1);仅 byte[] 路径(DecimalDigits 唯一用途)。实参每参数一 Value(J=单个 Long)。
        ("jdk/internal/misc/Unsafe", "putByte", "(Ljava/lang/Object;JB)V") => put_byte(vm, args),
        ("jdk/internal/misc/Unsafe", "getByte", "(Ljava/lang/Object;)B") => get_byte(vm, args),

        // 注:addressSize()/pageSize()/isBigEndian()/unalignedAccess() 均为返回常量字段
        // (ADDRESS_SIZE / PAGE_SIZE / BIG_ENDIAN / UNALIGNED_ACCESS)的字节码方法;这些字段在
        // Unsafe.class 中已是字面量初始化(不经 native),故 <clinit> 无更多 native 依赖。

        // 未登记 → UnsatisfiedLinkError。
        _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
}

/// Unsafe byte[] 访问的偏移 → 索引:`ARRAY_BYTE_BASE_OFFSET(16) + index` 的逆算。
/// offset < 16 经 `as usize` 回绕为大值,被越界检查兜住 → AIOOBE。
fn byte_index(offset: i64) -> usize {
    (offset - ARRAY_BYTE_BASE_OFFSET) as usize
}

/// `Unsafe.putByte(Object o, long offset, byte x)` 的 byte[] 实现。
/// 越界 → `ArrayIndexOutOfBoundsException`(HotSpot Unsafe 越界语义);非数组(裸内存/实例)→
/// `InternalError`(rustj 不支持裸内存访问;DecimalDigits 仅传 byte[],不触及)。
/// byte[] 元素为 `Slot::Int`,baload 读时 `(v as i8) as i32` 截断,故存原始 int 即正确。
fn put_byte(vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset, value) = match (args.first(), args.get(1), args.get(2)) {
        (Some(Value::Reference(r)), Some(Value::Long(o)), Some(Value::Int(b))) => (*r, *o, *b),
        _ => return Err(VmError::BadConstant("Unsafe.putByte 参数形状不符")),
    };
    let index = byte_index(offset);
    match vm.heap_mut().get_mut(arr) {
        Some(Oop::Array(a)) => {
            if index >= a.length() {
                return Err(throw_exception(
                    vm,
                    "java/lang/ArrayIndexOutOfBoundsException",
                ));
            }
            a.set_element(index, Slot::Int(value));
            Ok(Value::Void)
        }
        _ => Err(throw_exception(vm, "java/lang/InternalError")),
    }
}

/// `Unsafe.getByte(Object o, long offset)` 的 byte[] 实现(与 put_byte 成对;toString 读
/// byte[] 时需要)。
fn get_byte(vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(r)), Some(Value::Long(o))) => (*r, *o),
        _ => return Err(VmError::BadConstant("Unsafe.getByte 参数形状不符")),
    };
    let index = byte_index(offset);
    match vm.heap().get(arr) {
        Some(Oop::Array(a)) => {
            if index >= a.length() {
                return Err(throw_exception(
                    vm,
                    "java/lang/ArrayIndexOutOfBoundsException",
                ));
            }
            match a.element(index) {
                Slot::Int(v) => Ok(Value::Int(v)),
                _ => Err(VmError::BadConstant("Unsafe.getByte 元素非 int 槽")),
            }
        }
        _ => Err(throw_exception(vm, "java/lang/InternalError")),
    }
}
