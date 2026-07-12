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
    vm: &mut Vm,
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

        // jdk.internal.misc.Unsafe 的偏移读族 —— `getLong/getInt/getShort/getByte(Object,long)`
        //(Unsafe.java:243/164/227/219 均 `public native`,描述符 `(Ljava/lang/Object;J){J,I,S,B}`)。
        // 解锁 `String.startsWith`(prefix len>7)→ `ArraysSupport.mismatch` → `vectorizedMismatch`
        //(纯 Java 字节码,`@IntrinsicCandidate` 非 native;HotSpot 内联 SIMD,rustj 跑其 Java 体)。
        // `vectorizedMismatch`(ArraysSupport.java:118)对数组以 `getLongUnaligned/getIntUnaligned`
        //(Unsafe.java:3563/3602,纯 Java)按 offset 对齐度**委派**到下列 native:8 对齐→getLong、
        // 4 对齐→getInt、2 对齐→getShort、奇对齐→getByte。故四族均须绑,缺一则在未对齐 offset 落 ULE。
        // **另**:`putByte(Object,long,byte)`(Unsafe.java:219 native)经 `DecimalDigits.uncheckedPutCharLatin1`
        // 把数字字节写入 byte[],解锁 StringBuilder.append(int)/Integer.toString 等 int→string 链。
        //
        // rustj 无真实偏移内存:把 ArrayOop 视为**扁平小端字节缓冲**,按 byte_offset = offset - ABASE
        //(ABASE 与 `arrayBaseOffset0` 同源恒 16)取 N 字节、按组件类型序列化、小端打包/拆包。
        // byte[](String 紧凑串)为即时场景;`array_le_bytes` 通吃 byte/char/short/int/long/float/double
        //(见下)。实参:第 0 = 数组 Reference,第 1 = offset Long(单个 category-2 槽,JVM 级单参)。
        ("jdk/internal/misc/Unsafe", "putByte", "(Ljava/lang/Object;JB)V") => put_byte(vm, args),
        ("jdk/internal/misc/Unsafe", "getByte", "(Ljava/lang/Object;J)B") => get_byte(vm, args),
        ("jdk/internal/misc/Unsafe", "getChar", "(Ljava/lang/Object;J)C") => get_char(vm, args),
        ("jdk/internal/misc/Unsafe", "getShort", "(Ljava/lang/Object;J)S") => get_short(vm, args),
        ("jdk/internal/misc/Unsafe", "getInt", "(Ljava/lang/Object;J)I") => get_int(vm, args),
        ("jdk/internal/misc/Unsafe", "getLong", "(Ljava/lang/Object;J)J") => get_long(vm, args),

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

        // jdk.internal.misc.Unsafe volatile 读写原语族(Layer 4.22)——并发山正式入场。CHM 的
        // `Node[] table` 经 `tabAt`(CHM.java:771)→ `getReferenceAcquire`(非 native 委派)→
        // `getReferenceVolatile`(Unsafe.java:2117 native)按 byte 偏移读引用槽;`initTable` 的 sizeCtl
        // 经 `compareAndSetInt`(已绑),`transfer` 的 transferIndex(volatile long)经 `compareAndSetLong`
        // (Unsafe.java:2061)。acquire/release/weak/getAndAddInt 均为**非 native 字节码委派**(转调下列
        // native),故仅需绑这 6 个 native。**单线程下 volatile=plain**(无重排/内存屏障):read/write/CAS
        // 退化为普通槽访问。共用 offset→slot 模型:Instance=ord(同 compareAndSetInt),Array=(offset-ABASE)/scale
        // 索引(对象数组 scale=1,故 index=offset-16;CHM.tabAt 的 offset=(i<<ASHIFT)+ABASE,ASHIFT=0)。
        (
            "jdk/internal/misc/Unsafe",
            "getReferenceVolatile",
            "(Ljava/lang/Object;J)Ljava/lang/Object;",
        ) => get_reference_volatile(vm, args),
        (
            "jdk/internal/misc/Unsafe",
            "putReferenceVolatile",
            "(Ljava/lang/Object;JLjava/lang/Object;)V",
        ) => put_reference_volatile(vm, args),
        (
            "jdk/internal/misc/Unsafe",
            "getIntVolatile",
            "(Ljava/lang/Object;J)I",
        ) => get_int_volatile(vm, args),
        (
            "jdk/internal/misc/Unsafe",
            "putIntVolatile",
            "(Ljava/lang/Object;JI)V",
        ) => put_int_volatile(vm, args),
        // jdk.internal.misc.Unsafe.getLongVolatile(Object,long)J —— Unsafe.java:2439 native。
        // **Phase B.4a**:o=null + NEXT_THREAD_ID_OFFSET → 堆外「下一 tid」计数器
        //(ThreadIdentifiers.next()→getAndAddLong 路径);否则 Instance=ord 读 long 槽(单线程 volatile=plain)。
        ("jdk/internal/misc/Unsafe", "getLongVolatile", "(Ljava/lang/Object;J)J") => {
            get_long_volatile(vm, args)
        }
        (
            "jdk/internal/misc/Unsafe",
            "compareAndSetLong",
            "(Ljava/lang/Object;JJJ)Z",
        ) => compare_and_set_long(vm, args),
        (
            "jdk/internal/misc/Unsafe",
            "compareAndExchangeInt",
            "(Ljava/lang/Object;JII)I",
        ) => compare_and_exchange_int(vm, args),

        // jdk.internal.misc.Unsafe.ensureClassInitialized0(Ljava/lang/Class;)V —— Unsafe.java:3878
        // `private native`(jmod javap 实测确认;Unsafe.java:1190 字节码 `ensureClassInitialized`
        // null-check 后委派本 native)。强制目标类跑 <clinit>(JVMS-5.5;等价
        // `JVM_EnsureClassInitialization`)。取 Class 镜像内部名转调既有 `ensure_class_initialized`
        //(clinit.rs:幂等 Done/InProgress→返、Failed→NoClassDefFoundError、NotStarted→超类先+本类
        // clinit,异常包 ExceptionInInitializerError)。null 参 → NPE(契约同 ensureClassInitialized)。
        // 解锁 URLClassPath.<clinit> → SharedSecrets.getJavaNetURLAccess → ensureClassInitialized(URL)
        // → MethodHandles$Lookup.ensureInitialized 链。
        ("jdk/internal/misc/Unsafe", "ensureClassInitialized0", "(Ljava/lang/Class;)V") => {
            ensure_class_initialized_0(vm, args)
        }

        // jdk.internal.misc.Unsafe.shouldBeInitialized0(Ljava/lang/Class;)Z —— Unsafe.java:3877
        // `private native`(Unsafe.java:1172 字节码 `shouldBeInitialized` 委派本 native)。返 true =
        // 类尚未初始化(调 ensureClassInitialized 会有效果)。取 Class 镜像名 → 查 `init_state`;
        // 非 Done 即需初始化。解锁 DirectMethodHandle.shouldBeInitialized → DMH ensureInitialized 判定。
        ("jdk/internal/misc/Unsafe", "shouldBeInitialized0", "(Ljava/lang/Class;)Z") => {
            should_be_initialized_0(vm, args)
        }

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
fn put_byte(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset, value) = match (args.first(), args.get(1), args.get(2)) {
        (Some(Value::Reference(r)), Some(Value::Long(o)), Some(Value::Int(b))) => (*r, *o, *b),
        _ => return Err(VmError::BadConstant("Unsafe.putByte 参数形状不符")),
    };
    let index = byte_index(offset);
    // 持 heap guard 校验 + 写;throw 释 guard 后再抛(B.2.3b drop-before-recurse)。
    let outcome: Result<Value, &str> = match vm.heap_mut().get_mut(arr) {
        Some(Oop::Array(a)) => {
            if index >= a.length() {
                Err("java/lang/ArrayIndexOutOfBoundsException")
            } else {
                a.set_element(index, Slot::Int(value));
                Ok(Value::Void)
            }
        }
        _ => Err("java/lang/InternalError"),
    };
    match outcome {
        Ok(v) => Ok(v),
        Err(cls) => Err(throw_exception(vm, cls)),
    }
}

/// `Unsafe.getByte(Object o, long offset)B`(Unsafe.java:219 native):读偏移处 1 字节,有符号返回
///(`(b as i8) as i32`)。经 `getLongUnaligned`/`getIntUnaligned` 的奇对齐分支(逐字节拼装)及
/// `DecimalDigits` 读 byte[] 调用。委托 [`array_le_bytes`] 取 1 字节。
fn get_byte(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(r)), Some(Value::Long(o))) => (*r, *o),
        _ => return Err(VmError::BadConstant("Unsafe.getByte 参数形状不符")),
    };
    let bytes = array_le_bytes(vm, arr, byte_index(offset), 1)?;
    Ok(Value::Int((bytes[0] as i8) as i32))
}

/// `Unsafe.getChar(Object o, long offset)C`(Unsafe.java native):读 2 字节,小端拼为**无符号** char
/// (0..65535,不符号扩展)。被 `getCharUnaligned(o,off,bigEndian)`(纯 Java 字节码)经 `convEndian`
/// 委派——`RawBytecodeHelper.getU2Unchecked`(StackMapGenerator 读生成字节码的 u2 操作数)传
/// `bigEndian=true`,故 getChar 先 LE 读再由调用方翻 BE。解锁 Class-File API 物种方法的栈映射生成。
fn get_char(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(r)), Some(Value::Long(o))) => (*r, *o),
        _ => return Err(VmError::BadConstant("Unsafe.getChar 参数形状不符")),
    };
    let bytes = array_le_bytes(vm, arr, byte_index(offset), 2)?;
    let v = (bytes[0] as u16) | ((bytes[1] as u16) << 8);
    Ok(Value::Int(v as i32))
}

/// `Unsafe.getShort(Object o, long offset)S`(Unsafe.java:227 native):读 2 字节,小端拼为有符号 short。
/// 经 `getLongUnaligned`(2 对齐分支)/`getIntUnaligned`(2 对齐分支)调用。
fn get_short(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(r)), Some(Value::Long(o))) => (*r, *o),
        _ => return Err(VmError::BadConstant("Unsafe.getShort 参数形状不符")),
    };
    let bytes = array_le_bytes(vm, arr, byte_index(offset), 2)?;
    let v = (bytes[0] as u16) | ((bytes[1] as u16) << 8);
    Ok(Value::Int((v as i16) as i32))
}

/// `Unsafe.getInt(Object o, long offset)I`(Unsafe.java:164 native):读 4 字节,小端拼为 int。
/// 经 `getLongUnaligned`(4 对齐分支)/`getIntUnaligned`(4 对齐分支)及 vectorizedMismatch 尾部调用。
fn get_int(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(r)), Some(Value::Long(o))) => (*r, *o),
        _ => return Err(VmError::BadConstant("Unsafe.getInt 参数形状不符")),
    };
    let bytes = array_le_bytes(vm, arr, byte_index(offset), 4)?;
    let mut v: u32 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        v |= (b as u32) << (8 * i);
    }
    Ok(Value::Int(v as i32))
}

/// `Unsafe.getLong(Object o, long offset)J`(Unsafe.java:243 native):读 8 字节,小端拼为 long。
/// 经 `getLongUnaligned`(8 对齐分支)→ `vectorizedMismatch` 主循环调用——byte[] 的向量化比较核心。
fn get_long(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (arr, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(r)), Some(Value::Long(o))) => (*r, *o),
        _ => return Err(VmError::BadConstant("Unsafe.getLong 参数形状不符")),
    };
    let bytes = array_le_bytes(vm, arr, byte_index(offset), 8)?;
    let mut v: i64 = 0;
    for (i, &b) in bytes.iter().enumerate() {
        v |= (b as i64) << (8 * i);
    }
    Ok(Value::Long(v))
}

/// ArrayOop 组件类型的字节刻度(class_name 形如 `[B`/`[I`/`[Ljava/lang/Object;`)。引用数组及未知 → 1
///(`vectorizedMismatch` 仅用于基本类型数组,引用数组走 `equals`,不及本路径,保守取 1 不崩溃)。
fn component_scale(class_name: &str) -> usize {
    match class_name.as_bytes().get(1) {
        Some(b'B') | Some(b'Z') => 1,
        Some(b'C') | Some(b'S') => 2,
        Some(b'I') | Some(b'F') => 4,
        Some(b'J') | Some(b'D') => 8,
        _ => 1,
    }
}

/// 把 ArrayOop 在 `[byte_offset, byte_offset+n)` 字节区间的**扁平小端**表示读出。
/// 按组件类型刻度序列化覆盖到的每个元素(scale 字节 LE),再切片取目标 n 字节区间(支持未对齐
/// byte_offset,如 getLongUnaligned 的奇对齐分支)。越界 → `ArrayIndexOutOfBoundsException`;
/// 非数组(裸内存/实例)→ `InternalError`(rustj 不支持裸内存访问)。
///
/// 这是 rustj 对 HotSpot "按偏移读原始内存" 的等价物:ArrayOop 不是连续内存,但元素按组件类型
/// 有确定 LE 字节表示,故可逐元素序列化后按字节切片。byte[](String 紧凑串)scale=1 时退化为逐字节。
fn array_le_bytes(
    vm: &mut Vm,
    arr: Reference,
    byte_offset: usize,
    n: usize,
) -> Result<Vec<u8>, VmError> {
    if n == 0 {
        return Ok(Vec::new());
    }
    // 持 heap guard 读 + 收集;throw 释 guard 后再抛(B.2.3b drop-before-recurse)。
    let outcome: Result<Vec<u8>, &str> = (|| {
        let heap = vm.heap();
        let a = match heap.get(arr) {
            Some(Oop::Array(a)) => a,
            _ => return Err("java/lang/InternalError"),
        };
        let scale = component_scale(a.class_name());
        let first = byte_offset / scale;
        // n>=1 故 byte_offset+n-1 不下溢;若 byte_offset+n 越数组末端,last ≥ length → AIOOBE。
        let last = byte_offset.saturating_add(n - 1) / scale;
        if last >= a.length() {
            return Err("java/lang/ArrayIndexOutOfBoundsException");
        }
        let mut buf = Vec::with_capacity((last - first + 1) * scale);
        for ei in first..=last {
            buf.extend_from_slice(&element_le_bytes(a.element(ei), scale));
        }
        let start = byte_offset - first * scale; // = byte_offset % scale
        Ok(buf[start..start + n].to_vec())
    })();
    match outcome {
        Ok(v) => Ok(v),
        Err(cls) => Err(throw_exception(vm, cls)),
    }
}

/// 单个元素 → `scale` 字节小端表示。Int 槽覆盖 byte/char/short/int(取低 `scale` 字节);
/// Long/Float/Double 按其位模式;Reference/Top/ReturnAddress(引用数组等)→ 全 0(保守,mismatch 不及)。
fn element_le_bytes(slot: Slot, scale: usize) -> Vec<u8> {
    match slot {
        Slot::Int(v) => {
            let u = v as u32;
            (0..scale).map(|b| ((u >> (8 * b)) & 0xFF) as u8).collect()
        }
        Slot::Long(v) => (0..8).map(|b| (((v as u64) >> (8 * b)) & 0xFF) as u8).collect(),
        Slot::Float(f) => f.to_bits().to_le_bytes().to_vec(),
        Slot::Double(d) => d.to_bits().to_le_bytes().to_vec(),
        _ => vec![0u8; scale],
    }
}

/// `Unsafe.objectFieldOffset1(Class c, String name)J` —— 返字段在声明类扁平实例布局中的
/// **序号**(ord;rustj 无真实内存偏移,以 ord 代之,内部自洽)。未找到 → -1(public 包装器
/// 据 `< 0` 抛 InternalError)。声明类由 Class 镜像反查;字段名读真 String。
fn object_field_offset(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
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
/// Unsafe.java:1453 native。经共用 offset→slot 模型读当前槽(Instance=ord / Array=(offset-ABASE)/scale
/// 索引),与 expected 比引用身份(同 id 或同 null)→ 相等则 [`write_slot`] 写新、返 true,否则 false。
/// 仅 Slot::Reference 字段/元素;非引用槽 → false。**Array 路径**(Layer 4.22 收尾)解锁 CHM `casTabAt`
/// (Node[] 上的 CAS)——修前仅 Instance,致 `putIfAbsent` 的 `while(true) casTabAt` 死循环。
fn compare_and_set_reference(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, expected, x) = match (args.first(), args.get(1), args.get(2), args.get(3)) {
        (
            Some(Value::Reference(o)),
            Some(Value::Long(off)),
            Some(Value::Reference(e)),
            Some(Value::Reference(x)),
        ) => (*o, *off, *e, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // 经共用模型读当前槽(单线程 volatile=plain);匹配 → write_slot 写新(同模型,出块后 &mut 独占)。
    let matches = matches!(read_slot(vm, o, offset), Some(Slot::Reference(cur)) if cur == expected);
    if matches {
        write_slot(vm, o, offset, Slot::Reference(x))?;
        Ok(Value::Int(1))
    } else {
        Ok(Value::Int(0))
    }
}

/// `Unsafe.compareAndSetInt(Object o, long offset, int expected, int x)Z` ——
/// 经共用 offset→slot 模型读当前 int 槽(Instance=ord / Array=(offset-ABASE)/scale 索引),
/// 与 expected 比较相等 → [`write_slot`] 写 x 返 true,否则 false。镜像 `compare_and_set_reference`
/// 的 int 版(Unsafe.java:1514;CHM.initTable 的 sizeCtl CAS 走此)。仅 Slot::Int;非 int 槽 → false
/// (不抛)。单线程下当 expected 匹配恒成功。
fn compare_and_set_int(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, expected, x) = match (args.first(), args.get(1), args.get(2), args.get(3)) {
        (
            Some(Value::Reference(o)),
            Some(Value::Long(off)),
            Some(Value::Int(e)),
            Some(Value::Int(x)),
        ) => (*o, *off, *e, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // 经共用模型读当前槽(单线程 volatile=plain);匹配 → write_slot 写新。
    let matches = matches!(read_slot(vm, o, offset), Some(Slot::Int(cur)) if cur == expected);
    if matches {
        write_slot(vm, o, offset, Slot::Int(x))?;
        Ok(Value::Int(1))
    } else {
        Ok(Value::Int(0))
    }
}

/// 数组组件 → 元素刻度(同 `arrayIndexScale0`):`[B`/`[Z`→1,`[C`/`[S`→2,`[I`/`[F`→4,
/// `[J`/`[D`→8,引用数组(`[L…`/`[[…`)→1。volatile 数组访问按此把 byte 偏移换算为元素索引。
fn array_scale(desc: &str) -> i64 {
    let rest = desc.strip_prefix('[').unwrap_or(desc);
    match rest.chars().next() {
        Some('B') | Some('Z') => 1,
        Some('C') | Some('S') => 2,
        Some('I') | Some('F') => 4,
        Some('J') | Some('D') => 8,
        _ => 1, // 引用数组 / 未知 → 1
    }
}

/// Array 偏移 → 元素索引:`(offset - ARRAY_BYTE_BASE_OFFSET) / scale`。CHM.tabAt 的 offset =
/// `(i << ASHIFT) + ABASE`;对象数组 scale=1 → ASHIFT=0 → offset = i+16,故 index = offset-16。
/// 与 [`byte_index`] 同构但按刻度泛化(供 volatile 引用/long/int 数组访问)。
fn array_index_from_offset(desc: &str, offset: i64) -> usize {
    ((offset - ARRAY_BYTE_BASE_OFFSET) / array_scale(desc)) as usize
}

/// offset → 槽位读(共用模型,单线程 volatile=plain):Instance = ord(`offset as usize`);
/// Array = `(offset-ABASE)/scale` 索引。越界 / 非堆对象 → None。供 getReferenceVolatile/
/// getIntVolatile 与 CAS/exchange 的"读当前"步共用(返回 owned Slot,借即释)。
fn read_slot(vm: &Vm, o: Reference, offset: i64) -> Option<Slot> {
    match vm.heap().get(o) {
        Some(Oop::Instance(i)) => Some(i.field(offset as usize)),
        Some(Oop::Array(a)) => {
            let idx = array_index_from_offset(a.class_name(), offset);
            (idx < a.length()).then(|| a.element(idx))
        }
        _ => None,
    }
}

/// offset → 槽位写(共用模型,单线程 volatile=plain):Instance = ord;Array = `(offset-ABASE)/scale`
/// 索引。越界 → `ArrayIndexOutOfBoundsException`(HotSpot Unsafe 越界语义);非堆对象 → `InternalError`。
/// 供 putReferenceVolatile/putIntVolatile 与 CAS/exchange 的"写新"步共用。B.2.3b:heap 为 Mutex,
/// 持 guard 期间不能 `&mut vm` 抛异常 → 先在锁内校验+写并收 `Result<(), &str>` 标记,释 guard 后再抛。
fn write_slot(vm: &mut Vm, o: Reference, offset: i64, slot: Slot) -> Result<(), VmError> {
    let outcome: Result<(), &str> = match vm.heap_mut().get_mut(o) {
        Some(Oop::Instance(i)) => {
            i.set_field(offset as usize, slot);
            Ok(())
        }
        Some(Oop::Array(a)) => {
            let idx = array_index_from_offset(a.class_name(), offset);
            if idx >= a.length() {
                Err("java/lang/ArrayIndexOutOfBoundsException")
            } else {
                a.set_element(idx, slot);
                Ok(())
            }
        }
        _ => Err("java/lang/InternalError"),
    };
    match outcome {
        Ok(()) => Ok(()),
        Err(cls) => Err(throw_exception(vm, cls)),
    }
}

/// `Unsafe.getReferenceVolatile(Object o, long offset)Object` —— Unsafe.java:2117 native。
/// 单线程 volatile=plain:经 [`read_slot`] 取槽;`Slot::Reference` → 返该引用;其余(越界/非引用槽)→
/// null。CHM.tabAt 经 `getReferenceAcquire`(非 native 委派)转调此读 `Node[] table` 槽。
fn get_reference_volatile(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(o)), Some(Value::Long(off))) => (*o, *off),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    Ok(match read_slot(vm, o, offset) {
        Some(Slot::Reference(r)) => Value::Reference(r),
        _ => Value::Reference(Reference::null()),
    })
}

/// `Unsafe.putReferenceVolatile(Object o, long offset, Object x)V` —— Unsafe.java:2124 native。
/// 单线程 volatile=plain:经 [`write_slot`] 写 `Slot::Reference`(Instance=ord,Array=(offset-ABASE)/scale)。
fn put_reference_volatile(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, x) = match (args.first(), args.get(1), args.get(2)) {
        (Some(Value::Reference(o)), Some(Value::Long(off)), Some(Value::Reference(x))) => {
            (*o, *off, *x)
        }
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    write_slot(vm, o, offset, Slot::Reference(x))?;
    Ok(Value::Void)
}

/// `Unsafe.getIntVolatile(Object o, long offset)I` —— Unsafe.java:2128 native。
/// 单线程 volatile=plain:经 [`read_slot`] 取槽;`Slot::Int` → 返;其余 → 0。
fn get_int_volatile(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(o)), Some(Value::Long(off))) => (*o, *off),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    Ok(match read_slot(vm, o, offset) {
        Some(Slot::Int(i)) => Value::Int(i),
        _ => Value::Int(0),
    })
}

/// `Unsafe.putIntVolatile(Object o, long offset, int x)V` —— Unsafe.java:2132 native。
/// 单线程 volatile=plain:经 [`write_slot`] 写 `Slot::Int`。
fn put_int_volatile(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, x) = match (args.first(), args.get(1), args.get(2)) {
        (Some(Value::Reference(o)), Some(Value::Long(off)), Some(Value::Int(x))) => (*o, *off, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    write_slot(vm, o, offset, Slot::Int(x))?;
    Ok(Value::Void)
}

/// `Unsafe.getLongVolatile(Object o, long offset)J` —— Unsafe.java:2439 native。单线程 volatile=plain。
/// **o=null + [`NEXT_THREAD_ID_OFFSET`]** → 堆外「下一线程 tid」计数器(ThreadIdentifiers.next 路径,
/// 由 `Thread.getNextThreadIdOffset` 返回此哨兵;HotSpot 该计数器 off-heap,rustj 以
/// `ThreadManager.next_tid` 承载);否则 Instance=ord 读 long 槽(经 [`read_slot`]),非 long 槽 → 0。
/// 数组 long 读取走 `getLong`(array_le_bytes);本原生仅实例/静态-哨兵(B.4a 仅此路径被构造器触及)。
fn get_long_volatile(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset) = match (args.first(), args.get(1)) {
        (Some(Value::Reference(o)), Some(Value::Long(off))) => (*o, *off),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    if o.is_null() && offset == crate::runtime::vm::NEXT_THREAD_ID_OFFSET {
        return Ok(Value::Long(vm.read_next_thread_id()));
    }
    Ok(match read_slot(vm, o, offset) {
        Some(Slot::Long(v)) => Value::Long(v),
        _ => Value::Long(0),
    })
}

/// `Unsafe.compareAndSetLong(Object o, long offset, long expected, long x)Z` —— Unsafe.java:2061
/// native。经 [`read_slot`] 读 long 槽 == expected → [`write_slot`] 写 x 返 true,否则 false。
/// 镜像 `compare_and_set_int` 的 long 版(CHM.transfer 的 transferIndex CAS 走此)。仅 `Slot::Long`;
/// 非 long 槽 → false(不抛)。
fn compare_and_set_long(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, expected, x) = match (args.first(), args.get(1), args.get(2), args.get(3)) {
        (
            Some(Value::Reference(o)),
            Some(Value::Long(off)),
            Some(Value::Long(e)),
            Some(Value::Long(x)),
        ) => (*o, *off, *e, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    // 堆外「下一 tid」哨兵(getAndAddLong 循环的 CAS 步;ThreadIdentifiers.next 路径)。
    if o.is_null() && offset == crate::runtime::vm::NEXT_THREAD_ID_OFFSET {
        return Ok(Value::Int(if vm.cas_next_thread_id(expected, x) { 1 } else { 0 }));
    }
    let matches = matches!(read_slot(vm, o, offset), Some(Slot::Long(cur)) if cur == expected);
    if matches {
        write_slot(vm, o, offset, Slot::Long(x))?;
        Ok(Value::Int(1))
    } else {
        Ok(Value::Int(0))
    }
}

/// `Unsafe.compareAndExchangeInt(Object o, long offset, int expected, int x)I` —— Unsafe.java:1519
/// native。语义:读当前;== expected → 写 x 返**旧值**(=expected);否则不写返当前值。区别于
/// `compare_and_set_int`(返 Z):exchange 返旧值。经 [`read_slot`]/[`write_slot`](单线程 volatile=plain)。
fn compare_and_exchange_int(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let (o, offset, expected, x) = match (args.first(), args.get(1), args.get(2), args.get(3)) {
        (
            Some(Value::Reference(o)),
            Some(Value::Long(off)),
            Some(Value::Int(e)),
            Some(Value::Int(x)),
        ) => (*o, *off, *e, *x),
        _ => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    match read_slot(vm, o, offset) {
        Some(Slot::Int(cur)) if cur == expected => {
            write_slot(vm, o, offset, Slot::Int(x))?;
            Ok(Value::Int(cur))
        }
        Some(Slot::Int(cur)) => Ok(Value::Int(cur)),
        _ => Ok(Value::Int(0)),
    }
}

/// `Unsafe.ensureClassInitialized0(Class)V` native 实现(Unsafe.java:3878 `private native`)。
/// 强制目标类跑 `<clinit>`(JVMS-5.5)。取 Class 镜像内部名 → 转调 `ensure_class_initialized`
/// (clinit.rs)。`class_arg_name` 借 `&vm` 返 owned String,出 match 即释放 → 后续
/// `throw_exception(&mut vm)` / `ensure_class_initialized(&mut vm, ..)` 无借用冲突。null/非镜像 → NPE。
fn ensure_class_initialized_0(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let internal = match super::class_arg_name(vm, args) {
        Some(n) => n,
        None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    crate::runtime::interpreter::clinit::ensure_class_initialized(vm, &internal)?;
    Ok(Value::Void)
}

/// `Unsafe.shouldBeInitialized0(Class)Z` native 实现(Unsafe.java:3877 `private native`)。
/// 返 `init_state != Done`(类尚未初始化则 true)。null/非镜像 → NPE;类未注册 → false
/// (不可达:Class 镜像必对应已加载类,保守返 false 即"无需初始化")。
fn should_be_initialized_0(vm: &mut Vm, args: &[Value]) -> Result<Value, VmError> {
    let internal = match super::class_arg_name(vm, args) {
        Some(n) => n,
        None => return Err(throw_exception(vm, "java/lang/NullPointerException")),
    };
    let needs_init = match vm.registry() {
        Some(reg) => reg
            .get(&internal)
            .is_some_and(|lc| lc.init_state() != crate::oops::InitState::Done),
        None => false,
    };
    Ok(Value::Int(if needs_init { 1 } else { 0 }))
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

        let mut vm = Vm::new(registry);

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

    /// **RED→GREEN**(Layer 4.22):`Unsafe.getReferenceVolatile`/`putReferenceVolatile` 的**对象数组**
    /// 路径——CHM.tabAt(CHM.java:771)经 `getReferenceAcquire`(非 native 委派)→
    /// `getReferenceVolatile`(Unsafe.java:2117 native)按 byte 偏移读 `Node[] table` 槽。
    /// rustj 单线程下 volatile=plain;offset→slot:对象数组 scale=1,故 index = offset - ABASE(16)。
    ///
    /// `[Ljava/lang/Object;` 4 元素(默认 null):putReferenceVolatile(arr,18,x) 写元素 2;
    /// getReferenceVolatile(arr,18) 读回 x;getReferenceVolatile(arr,16) 读元素 0 = null。
    /// **无 jmod 依赖**(数组直接 ArrayOop::new 构造;非空引用用空数组的 alloc 句柄)。
    #[test]
    fn get_put_reference_volatile_object_array() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(registry);

        // 对象数组 [Ljava/lang/Object;(CHM Node[] table 同型),4 元素默认 null。
        let arr = vm.heap_mut().alloc(crate::oops::Oop::Array(
            crate::oops::ArrayOop::new(
                "[Ljava/lang/Object;".to_string(),
                vec![Slot::Reference(crate::runtime::Reference::null()); 4],
            ),
        ));
        // 非空引用:alloc 句柄恒非 0(用空对象数组充当一个非空 Object,避免拉入 String 等)。
        let x = vm.heap_mut().alloc(crate::oops::Oop::Array(
            crate::oops::ArrayOop::new(
                "[Ljava/lang/Object;".to_string(),
                Vec::new(),
            ),
        ));
        assert!(!x.is_null(), "alloc 句柄须非 null");

        // putReferenceVolatile(arr, 16+2, x) → 写元素 2(offset = ABASE + i*scale = 16 + 2*1)。
        super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "putReferenceVolatile",
            "(Ljava/lang/Object;JLjava/lang/Object;)V",
            None,
            &[Value::Reference(arr), Value::Long(18), Value::Reference(x)],
        )
        .expect("putReferenceVolatile 须返 Void");

        // getReferenceVolatile(arr, 18) → 读元素 2 == x。
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "getReferenceVolatile",
            "(Ljava/lang/Object;J)Ljava/lang/Object;",
            None,
            &[Value::Reference(arr), Value::Long(18)],
        )
        .expect("getReferenceVolatile 须返 ref");
        assert_eq!(r, Value::Reference(x), "offset 18(元素 2)须读回写入的 x");

        // getReferenceVolatile(arr, 16) → 元素 0 仍 null(默认)。
        let r0 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "getReferenceVolatile",
            "(Ljava/lang/Object;J)Ljava/lang/Object;",
            None,
            &[Value::Reference(arr), Value::Long(16)],
        )
        .expect("getReferenceVolatile 须返 ref");
        assert_eq!(
            r0,
            Value::Reference(crate::runtime::Reference::null()),
            "offset 16(元素 0)须为 null"
        );
    }

    /// **RED→GREEN**(Layer 4.22 收尾):`Unsafe.compareAndSetReference` 的**对象数组**路径
    /// (casTabAt,CHM.java `casTabAt(tab,i,c,v)` 同型)。修前仅 Instance 路径(4.18),casTabAt 在
    /// `Node[]` 上恒 false → CHM `putIfAbsent` 的 `while(true) casTabAt` 死循环(探针证实)。
    /// 对象数组 4 元素(默认 null):
    /// CAS(arr,18,null,x) → 元素 2 当前 null==expected → 写 x 返 true;
    /// CAS(arr,18,null,y) → 当前 x≠null → 失败返 0,不变。
    #[test]
    fn compare_and_set_reference_object_array() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(registry);

        let arr = vm.heap_mut().alloc(crate::oops::Oop::Array(
            crate::oops::ArrayOop::new(
                "[Ljava/lang/Object;".to_string(),
                vec![Slot::Reference(crate::runtime::Reference::null()); 4],
            ),
        ));
        let x = vm.heap_mut().alloc(crate::oops::Oop::Array(
            crate::oops::ArrayOop::new(
                "[Ljava/lang/Object;".to_string(),
                Vec::new(),
            ),
        ));

        // CAS(arr, 18, null, x):元素 2 当前 null==expected → 写 x 返 true(单线程首 CAS 必成功)。
        let r1 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndSetReference",
            "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z",
            None,
            &[
                Value::Reference(arr),
                Value::Long(18),
                Value::Reference(crate::runtime::Reference::null()),
                Value::Reference(x),
            ],
        )
        .expect("CAS 须返 Z");
        assert_eq!(r1, Value::Int(1), "CAS(null→x) 须成功(元素 2 默认 null)");

        // 读回元素 2 = x(经 getReferenceVolatile,已支持数组)。
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "getReferenceVolatile",
            "(Ljava/lang/Object;J)Ljava/lang/Object;",
            None,
            &[Value::Reference(arr), Value::Long(18)],
        )
        .expect("getReferenceVolatile 须返 ref");
        assert_eq!(r, Value::Reference(x), "CAS 后元素 2 须为 x");

        // CAS(arr, 18, null, x):元素 2 当前 x≠null(expected) → 失败返 0,不变。
        let r2 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndSetReference",
            "(Ljava/lang/Object;JLjava/lang/Object;Ljava/lang/Object;)Z",
            None,
            &[
                Value::Reference(arr),
                Value::Long(18),
                Value::Reference(crate::runtime::Reference::null()),
                Value::Reference(x),
            ],
        )
        .expect("CAS 须返 Z");
        assert_eq!(r2, Value::Int(0), "CAS(null→x) 须失败(当前 x≠expected null)");
    }

    /// **RED→GREEN**(Layer 4.22):`Unsafe.getIntVolatile`/`putIntVolatile` 的**实例字段**路径
    /// (Unsafe.java:2128/2132 native)。offset = ord(objectFieldOffset1 给出);读/写实例 int 槽。
    /// Integer.value(默认 0):写前 getIntVolatile=0;putIntVolatile(o,ord,42) → 读回 42。
    #[test]
    fn get_put_int_volatile_instance() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

        let mut vm = Vm::new(registry);

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
        let Value::Long(ord) = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "objectFieldOffset1",
            "(Ljava/lang/Class;Ljava/lang/String;)J",
            None,
            &[Value::Reference(int_mirror), Value::Reference(name_ref)],
        )
        .expect("objectFieldOffset1 须返 ord")
        else {
            unreachable!()
        };
        assert!(ord >= 0, "value 字段须找到");

        // 写前:getIntVolatile(o,ord) = 0(默认)。
        let v0 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "getIntVolatile",
            "(Ljava/lang/Object;J)I",
            None,
            &[Value::Reference(inst_ref), Value::Long(ord)],
        )
        .expect("getIntVolatile 须返 I");
        assert_eq!(v0, Value::Int(0), "写前 value 须为 0");

        // putIntVolatile(o,ord,42)。
        super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "putIntVolatile",
            "(Ljava/lang/Object;JI)V",
            None,
            &[Value::Reference(inst_ref), Value::Long(ord), Value::Int(42)],
        )
        .expect("putIntVolatile 须返 Void");

        // 读回 = 42。
        let v1 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "getIntVolatile",
            "(Ljava/lang/Object;J)I",
            None,
            &[Value::Reference(inst_ref), Value::Long(ord)],
        )
        .expect("getIntVolatile 须返 I");
        assert_eq!(v1, Value::Int(42), "写后 value 须为 42");
    }

    /// **RED→GREEN**(Layer 4.22):`Unsafe.compareAndSetLong` 的实例 long 字段路径(Unsafe.java:2061
    /// native)。offset = ord;读实例 long 槽 == expected → 写 x 返 true。Long.value(long,默认 0L):
    /// CAS(0L→42L) 成功 + 字段变 42L;CAS(0L→99L) 失败(当前 42L≠0)+ 字段不变。
    #[test]
    fn compare_and_set_long_instance() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Long").unwrap();

        let mut vm = Vm::new(registry);

        // Long 实例(default-init,value=0L)。
        let inst_ref = {
            let reg = vm.registry().expect("须注册表");
            let lc = reg.get("java/lang/Long").expect("Long 须加载");
            let inst = reg.new_instance(lc);
            vm.heap_mut().alloc(crate::oops::Oop::Instance(inst))
        };

        // objectFieldOffset1(Long.class, "value") → ord。
        let long_mirror = vm.intern_class_mirror("java/lang/Long");
        let name_ref =
            crate::runtime::interpreter::string::intern(&mut vm, "value").expect("intern value");
        let Value::Long(ord) = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "objectFieldOffset1",
            "(Ljava/lang/Class;Ljava/lang/String;)J",
            None,
            &[Value::Reference(long_mirror), Value::Reference(name_ref)],
        )
        .expect("objectFieldOffset1 须返 ord")
        else {
            unreachable!()
        };
        assert!(ord >= 0, "Long.value 字段须找到");

        // CAS(0L→42L):当前 0L==expected → 成功返 1。
        let r1 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndSetLong",
            "(Ljava/lang/Object;JJJ)Z",
            None,
            &[
                Value::Reference(inst_ref),
                Value::Long(ord),
                Value::Long(0),
                Value::Long(42),
            ],
        )
        .expect("compareAndSetLong 须返 Z");
        assert_eq!(r1, Value::Int(1), "CAS(0L→42L) 须成功");

        // 字段现为 42L。
        let v = match vm.heap().get(inst_ref) {
            Some(crate::oops::Oop::Instance(i)) => i.field(ord as usize),
            _ => panic!("须 Instance"),
        };
        assert!(matches!(v, Slot::Long(42)), "CAS 后字段须为 42L,得 {v:?}");

        // CAS(0L→99L):当前 42L≠0 → 失败返 0,字段不变。
        let r2 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndSetLong",
            "(Ljava/lang/Object;JJJ)Z",
            None,
            &[
                Value::Reference(inst_ref),
                Value::Long(ord),
                Value::Long(0),
                Value::Long(99),
            ],
        )
        .expect("compareAndSetLong 须返 Z");
        assert_eq!(r2, Value::Int(0), "CAS(0L→99L) 须失败(当前 42L≠expected 0)");
    }

    /// **RED→GREEN**(Layer 4.22):`Unsafe.compareAndExchangeInt` 的实例 int 字段路径(Unsafe.java:1519
    /// native)。语义:读当前值;若 == expected 则写 x 并返**旧值**(=expected);否则不写,返当前值。
    /// 区别于 compareAndSetInt(返布尔):exchange 返旧值。Integer.value(默认 0):
    /// exchange(o,ord,0,42) → 当前 0==expected → 写 42,返旧值 0;
    /// exchange(o,ord,0,99) → 当前 42≠0 → 不写,返当前 42。
    #[test]
    fn compare_and_exchange_int_instance() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

        let mut vm = Vm::new(registry);
        let inst_ref = {
            let reg = vm.registry().expect("须注册表");
            let lc = reg.get("java/lang/Integer").expect("Integer 须加载");
            let inst = reg.new_instance(lc);
            vm.heap_mut().alloc(crate::oops::Oop::Instance(inst))
        };
        let int_mirror = vm.intern_class_mirror("java/lang/Integer");
        let name_ref =
            crate::runtime::interpreter::string::intern(&mut vm, "value").expect("intern value");
        let Value::Long(ord) = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "objectFieldOffset1",
            "(Ljava/lang/Class;Ljava/lang/String;)J",
            None,
            &[Value::Reference(int_mirror), Value::Reference(name_ref)],
        )
        .expect("objectFieldOffset1 须返 ord")
        else {
            unreachable!()
        };

        // exchange(o,ord,0,42):当前 0==expected → 写 42,返旧值 0。
        let r1 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndExchangeInt",
            "(Ljava/lang/Object;JII)I",
            None,
            &[
                Value::Reference(inst_ref),
                Value::Long(ord),
                Value::Int(0),
                Value::Int(42),
            ],
        )
        .expect("compareAndExchangeInt 须返 I");
        assert_eq!(r1, Value::Int(0), "exchange(0→42) 须返旧值 0");
        let v1 = match vm.heap().get(inst_ref) {
            Some(crate::oops::Oop::Instance(i)) => i.field(ord as usize),
            _ => panic!("须 Instance"),
        };
        assert!(matches!(v1, Slot::Int(42)), "exchange 后字段须为 42,得 {v1:?}");

        // exchange(o,ord,0,99):当前 42≠0 → 不写,返当前 42。
        let r2 = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "compareAndExchangeInt",
            "(Ljava/lang/Object;JII)I",
            None,
            &[
                Value::Reference(inst_ref),
                Value::Long(ord),
                Value::Int(0),
                Value::Int(99),
            ],
        )
        .expect("compareAndExchangeInt 须返 I");
        assert_eq!(r2, Value::Int(42), "exchange(0→99) 须返当前 42(不匹配,不写)");
        let v2 = match vm.heap().get(inst_ref) {
            Some(crate::oops::Oop::Instance(i)) => i.field(ord as usize),
            _ => panic!("须 Instance"),
        };
        assert!(
            matches!(v2, Slot::Int(42)),
            "不匹配 exchange 不得改字段,得 {v2:?}"
        );
    }

    /// **RED→GREEN**(Layer 4.24):`Unsafe.ensureClassInitialized0(Class)V` native 强制目标类跑
    /// `<clinit>`(JVMS-5.5;等价 `JVM_EnsureClassInitialization`)。`Unsafe.ensureClassInitialized`
    /// (Unsafe.java:1190,字节码)null-check 后委派本 native(jmod javap 确认 `private native ...
    /// (Ljava/lang/Class;)V`)。rustj 既有 `ensure_class_initialized`(clinit.rs)幂等实现,本 native
    /// 取 Class 镜像内部名转调之。验证:`load_closure` 不跑 `<clinit>`(Integer=NotStarted)→
    /// 调本 native → Integer=Done(IntegerCache 已建)。
    #[test]
    fn ensure_class_initialized_0_runs_clinit() {
        let Some(jmod) = find_javabase_jmod() else {
            eprintln!("跳过:无 java.base.jmod");
            return;
        };
        let mut registry = ClassRegistry::new();
        let bytes = std::fs::read(&jmod).unwrap();
        let mut cp = ClassPath::new();
        cp.add("java.base.jmod", &bytes).unwrap();
        load_closure(&mut registry, &cp, "java/lang/Integer").unwrap();

        let mut vm = Vm::new(registry);
        // 前置:<clinit> 须未跑(NotStarted)——证明后续 Done 是本 native 的功劳。
        // `.map` 嵌在 `and_then(|r| …)` 内:`r`(owned Arc)仅闭包内活,`&LoadedClass` 借之;
        // 内层在 `r` 存活时产 owned InitState,避免返引用悬垂(B.3.0 Arc 局部寿命)。
        let pre = vm
            .registry()
            .and_then(|r| r.get("java/lang/Integer").map(|lc| lc.init_state()))
            .expect("Integer 须已加载");
        assert!(
            matches!(pre, crate::oops::InitState::NotStarted),
            "前置 Integer 须 NotStarted,得 {pre:?}"
        );

        let mirror = vm.intern_class_mirror("java/lang/Integer");
        let r = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "ensureClassInitialized0",
            "(Ljava/lang/Class;)V",
            None,
            &[Value::Reference(mirror)],
        )
        .expect("ensureClassInitialized0 应返 void,非抛异常");
        assert!(matches!(r, Value::Void), "须返 void,得 {r:?}");

        let post = vm
            .registry()
            .and_then(|r| r.get("java/lang/Integer").map(|lc| lc.init_state()))
            .expect("Integer 须已加载");
        assert!(
            matches!(post, crate::oops::InitState::Done),
            "ensureClassInitialized0 后 Integer 须 Done,得 {post:?}"
        );
    }

    /// **RED→GREEN**(Layer 4.24):null 参 → NullPointerException(对应 `Unsafe.ensureClassInitialized`
    /// Unsafe.java:1192 `if (c == null) throw new NullPointerException()` 契约;native 防御性同)。
    #[test]
    fn ensure_class_initialized_0_null_arg_throws_npe() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(registry);
        let err = super::super::invoke(
            &mut vm,
            "jdk/internal/misc/Unsafe",
            "ensureClassInitialized0",
            "(Ljava/lang/Class;)V",
            None,
            &[Value::Reference(crate::runtime::Reference::null())],
        )
        .unwrap_err();
        match err {
            crate::runtime::VmError::ThrownException(r) => match vm.heap().get(r) {
                Some(crate::oops::Oop::Instance(i)) => {
                    assert_eq!(i.class_name(), "java/lang/NullPointerException")
                }
                o => panic!("须 Instance,得 {o:?}"),
            },
            e => panic!("须 ThrownException,得 {e:?}"),
        }
    }

    /// 收尾:确使未登记路径仍抛 ULE(防 dispatch 误吞)。
    #[test]
    fn unbound_unsafe_native_throws_ule() {
        let registry = ClassRegistry::new();
        let mut vm = Vm::new(registry);
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