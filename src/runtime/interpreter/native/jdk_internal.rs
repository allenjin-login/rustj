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

        // jdk.internal.misc.CDS.getRandomSeedForDumping()J —— CDS.java:143 public static native。
        // HotSpot 仅在 `-Xshare:dump` 时返回派生自 JVM 版本的非零种子(可重复生成 CDS 归档);
        // 非 dump 运行(rustj 永不 dump)→ 恒 0。调用方 `ImmutableCollections.<clinit>`(line 89)
        // 得 0 后回退 `System.nanoTime()`(已绑定)算 SALT——即规范的运行时随机路径。
        ("jdk/internal/misc/CDS", "getRandomSeedForDumping", "()J") => Ok(Value::Long(0)),

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

        // jdk.internal.misc.Unsafe.objectFieldOffset1(Ljava/lang/Class;Ljava/lang/String;)J
        // —— Unsafe.java:1100 `objectFieldOffset(Class, String)`(jmod 内部转调 `objectFieldOffset1`,
        // 注意 jdk-master 源码名为 `knownObjectFieldOffset0`,版本错位——以 jmod 为准)经
        // `Class$Atomic.<clinit>` 调:`reflectionDataOffset = unsafe.objectFieldOffset(Class.class,
        // "reflectionData")`。HotSpot 返字段在对象布局中的真实字节偏移;rustj **无真实内存偏移**,
        // 改返字段在声明类扁平实例布局中的**序号**(ord)——后续 `compareAndSetReference` 用同一 ord
        // 索引实例槽位,内部自洽。第 1 参 = 声明类的 Class 镜像(如 Class.class 镜像),第 2 参 =
        // 字段名(真 String 实例)。未找到字段 → 返 -1(public 包装器据 `< 0` 抛 InternalError)。
        ("jdk/internal/misc/Unsafe", "objectFieldOffset1", "(Ljava/lang/Class;Ljava/lang/String;)J") => {
            object_field_offset(vm, args)
        }

        // jdk.internal.misc.Unsafe.compareAndSetReference(O,J,expected,x)Z —— Unsafe.java:1453
        // native。`Class$Atomic.casReflectionData` 经 `reflectionData()`/`newReflectionData` 的
        // `while(true)` 重试调之:单线程首 CAS **必须成功**否则死循环。HotSpot 做真实引用 CAS;
        // rustj 用 ord(由 `objectFieldOffset1` 给出)读实例当前槽,与 expected 比较引用身份
        // (同 id 或同 null)→ 相等则写新、返 true,否则 false。仅 Slot::Reference 字段
        // (reflectionData/annotationType/annotationData 均引用字段);非引用槽 → false。
        (
            "jdk/internal/misc/Unsafe",
            "compareAndSetReference",
            "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z",
        ) => compare_and_set_reference(vm, args),

        // jdk.internal.misc.Unsafe.compareAndSetInt(O,J,expected,x)Z —— Unsafe.java:1514 native,
        // @IntrinsicCandidate(C11 atomic_compare_exchange_strong)。并发山起手:CHM 的 sizeCtl/
        // cellsBusy/lockState 等 volatile int 字段经之 CAS(initTable/transfer/counterCells)。
        // rustj 单线程:ord = offset;读实例当前 int 槽 == expected → 写 x 返 true,否则 false。
        // 仅 Slot::Int 字段;非 int 槽/非 Instance → false(不抛)。镜像 compareAndSetReference 之 int 版。
        (
            "jdk/internal/misc/Unsafe",
            "compareAndSetInt",
            "(Ljava/lang/Object;JII)Z",
        ) => compare_and_set_int(vm, args),

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

/// `Unsafe.objectFieldOffset1(Class c, String name)J` —— 返字段在声明类扁平实例布局中的
/// **序号**(ord;rustj 无真实内存偏移,以 ord 代之,内部自洽)。未找到 → -1(public 包装器
/// 据 `< 0` 抛 InternalError)。声明类由 Class 镜像反查;字段名读真 String。
fn object_field_offset(vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    let (class_mirror, name_ref) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(c)), Some(Value::Reference(n))) => (*c, *n),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let internal = vm
        .mirror_internal_name(class_mirror)
        .ok_or(VmError::BadConstant("objectFieldOffset1:非 Class 镜像"))?
        .to_string();
    let field_name = match super::super::string::read_text(vm, name_ref)? {
        Some(t) => t,
        None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // 借注册表(§6:'a 不绑 &self)查扁平布局序号;不触堆,出块即释放。
    let ord = vm.registry().and_then(|reg| {
        reg.get(&internal).and_then(|lc| {
            reg.flattened_instance_fields(lc)
                .iter()
                .position(|f| f.name == field_name)
        })
    });
    Ok(Value::Long(ord.map(|o| o as i64).unwrap_or(-1)))
}

/// `Unsafe.compareAndSetReference(Object o, long offset, Object expected, Object x)Z` ——
/// ord = offset;读 o 实例当前槽,与 expected 比引用身份(同 id 或同 null)→ 相等写 x 返 true,
/// 否则 false。仅 Slot::Reference 字段;非引用槽/非 Instance → false(不抛)。
fn compare_and_set_reference(vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, expected, x) = match (args.first(), args.get(1), args.get(2), args.get(3)) {
        (
            Some(Value::Reference(o)),
            Some(Value::Long(off)),
            Some(Value::Reference(e)),
            Some(Value::Reference(x)),
        ) => (*o, *off, *e, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let ord = offset as usize;
    // 读当前槽(不可变借堆);匹配标志出块后释放,再 &mut 写。
    let matches = match vm.heap().get(o) {
        Some(Oop::Instance(i)) => matches!(
            i.field(ord),
            Slot::Reference(cur) if cur == expected
        ),
        _ => false,
    };
    if matches {
        if let Some(Oop::Instance(i)) = vm.heap_mut().get_mut(o) {
            i.set_field(ord, Slot::Reference(x));
        }
        Ok(Value::Int(1))
    } else {
        Ok(Value::Int(0))
    }
}

/// `Unsafe.compareAndSetInt(Object o, long offset, int expected, int x)Z` ——
/// ord = offset;读 o 实例当前 int 槽,与 expected 比较相等 → 写 x 返 true,否则 false。
/// 仅 Slot::Int 字段;非 int 槽/非 Instance → false(不抛)。镜像 compareAndSetReference 的 int 版
/// (Unsafe.java:1514;CHM.initTable 的 sizeCtl CAS 走此)。单线程下当 expected 匹配恒成功。
fn compare_and_set_int(vm: &mut Vm<'_>, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, expected, x) = match (args.first(), args.get(1), args.get(2), args.get(3)) {
        (
            Some(Value::Reference(o)),
            Some(Value::Long(off)),
            Some(Value::Int(e)),
            Some(Value::Int(x)),
        ) => (*o, *off, *e, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let ord = offset as usize;
    // 读当前槽(不可变借堆);匹配标志出块后释放,再 &mut 写。
    let matches = match vm.heap().get(o) {
        Some(Oop::Instance(i)) => matches!(i.field(ord), Slot::Int(cur) if cur == expected),
        _ => false,
    };
    if matches {
        if let Some(Oop::Instance(i)) = vm.heap_mut().get_mut(o) {
            i.set_field(ord, Slot::Int(x));
        }
        Ok(Value::Int(1))
    } else {
        Ok(Value::Int(0))
    }
}

#[cfg(test)]
mod tests {
    use crate::oops::ClassRegistry;
    use crate::runtime::class_loader::class_path::ClassPath;
    use crate::runtime::class_loader::loader::load_closure;
    use crate::runtime::{Slot, Value, Vm};

    use std::path::{Path, PathBuf};

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
            .map(|jh| PathBuf::from(jh).join("jmods/java.base.jmod"))
            .filter(|p| p.exists())
    }

    /// **RED→GREEN**:`Unsafe.compareAndSetInt` 单线程 CAS 语义(Unsafe.java:1514 native)。
    ///
    /// `objectFieldOffset1`(已绑定)返字段 ord;`compareAndSetInt(o, ord, expected, x)` 读 ord 槽,
    /// 当前==expected → 写 x 返 true(1),否则返 false(0)。Integer 实例 `value` 字段(默认 0):
    /// CAS(0→42) 成功 + 字段变 42;CAS(0→99) 失败(当前 42≠0)+ 字段不变。
    #[test]
    fn compare_and_set_int_cas_semantics() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

        let mut vm = Vm::new(&registry);

        // Integer 实例(default-init,value=0)。§6 块模式:new_instance 出块后 heap_mut 独占。
        let inst_ref = {
            let reg = vm.registry().expect("须注册表");
            let lc = reg.get("java/lang/Integer").expect("Integer 须加载");
            let inst = reg.new_instance(lc);
            vm.heap_mut().alloc(crate::oops::Oop::Instance(inst))
        };

        // objectFieldOffset1(Integer.class, "value") → ord(已绑定 native)。
        let int_mirror = vm.intern_class_mirror("java/lang/Integer");
        let name_ref =
            crate::runtime::interpreter::string::intern(&mut vm, "value").expect("intern value");
        let ord_val = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "objectFieldOffset1",
            "(Ljava/lang/Class;Ljava/lang/String;)J",
            None,
            &[Value::Reference(int_mirror), Value::Reference(name_ref)],
        )
        .expect("objectFieldOffset1 须返 ord");
        let Value::Long(ord) = ord_val else {
            panic!("objectFieldOffset1 须返 Long,得 {ord_val:?}");
        };
        assert!(ord >= 0, "value 字段须找到,得 ord={ord}");

        // CAS(0→42):当前 value=0==expected → 成功返 1。
        let r1 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndSetInt",
            "(Ljava/lang/Object;JII)Z",
            None,
            &[Value::Reference(inst_ref), Value::Long(ord), Value::Int(0), Value::Int(42)],
        )
        .expect("compareAndSetInt 须返 Z,非抛异常");
        assert_eq!(r1, Value::Int(1), "CAS(0→42) 须成功返 1(当前 0==expected)");

        // 字段现为 42。
        let v = match vm.heap().get(inst_ref) {
            Some(crate::oops::Oop::Instance(i)) => i.field(ord as usize),
            _ => panic!("须 Instance"),
        };
        assert!(matches!(v, Slot::Int(42)), "CAS 后字段须为 42,得 {v:?}");

        // CAS(0→99):当前 42≠0 → 失败返 0,字段不变。
        let r2 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndSetInt",
            "(Ljava/lang/Object;JII)Z",
            None,
            &[Value::Reference(inst_ref), Value::Long(ord), Value::Int(0), Value::Int(99)],
        )
        .expect("compareAndSetInt 须返 Z,非抛异常");
        assert_eq!(r2, Value::Int(0), "CAS(0→99) 须失败返 0(当前 42≠expected 0)");
        let v2 = match vm.heap().get(inst_ref) {
            Some(crate::oops::Oop::Instance(i)) => i.field(ord as usize),
            _ => panic!("须 Instance"),
        };
        assert!(
            matches!(v2, Slot::Int(42)),
            "失败 CAS 不得改字段,须仍 42,得 {v2:?}"
        );
    }

    /// 收尾:确使未登记路径仍抛 ULE(防 dispatch 误吞)。
    #[test]
    fn unbound_unsafe_native_throws_ule() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(&registry);
        let err = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "unknownNative",
            "()V",
            None,
            &[],
        )
        .unwrap_err();
        match err {
            crate::runtime::VmError::ThrownException(r) => match vm.heap().get(r) {
                Some(crate::oops::Oop::Instance(i)) => {
                    assert_eq!(i.class_name(), "java/lang/UnsatisfiedLinkError")
                }
                o => panic!("须 Instance,得 {o:?}"),
            },
            e => panic!("须 ThrownException,得 {e:?}"),
        }
    }
}