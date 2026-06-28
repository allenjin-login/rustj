# Layer 4.10:native 方法绑定 + 容器加载(jar/jmod) — 设计

> 状态:spec · 北极星路线图步骤 3 & 4(用户要求同组推进)。前置:4.9(`<clinit>`)。
> 目标:从真实 JDK 容器(jar/jmod)**按需惰性加载**类字节 + 为 `ACC_NATIVE` 方法提供**内置 native
> 分派**,使引用真实 `java.base` 类(其 `<clinit>` 多调 `registerNatives`)的最小程序可加载运行——
> 为退役 Stage A 合成引导桩(最大可清除债)铺最后两块基石。

## 1. 问题

当前两块缺口共同阻塞北极星(加载真实 java.base):

1. **类只能由 javac 一锅端逐文件 `load`**(见 `tests/*` 闸门 helper)——无真实 `ClassLoader`,
   无容器;真实 `java.base` 在 jmod/jar 容器内,无法取出。
2. **`invoke*` 解析到无 `Code` 的方法(ACC_NATIVE / 抽象)一律 `BadConstant("无 Code")`**——
   真实 java.base 的 `Object.hashCode`/`System.currentTimeMillis`/各类 `registerNatives` 全是
   native,根本无法执行;真实 `<clinit>` 一调 native 即崩。

## 2. 源码依据(Step 0:已读 VM 源码,主要;JDK 仅辅助)

**VM(主要,`src/hotspot/`):**

- `prims/nativeLookup.cpp:308-406` `NativeLookup::lookup_entry`/`lookup_base`:native 方法经 JNI
  符号 `Java_<class>_<method>`(短)/`__<sig>`(长)解析;`registerNatives` 显式注册优先;
  落空 → `UnsatisfiedLinkError`(`nativeLookup.cpp:400-405`)。
- `prims/jvm.cpp`:`JVM_IHashCode`(→`ObjectSynchronizer::FastHashCode`)、`JVM_Clone`、
  `JVM_CurrentTimeMillis`(→`os::javaTimeMillis`)、`JVM_NanoTime`、`JVM_ArrayCopy`、
  `JVM_InternString`——`jvm.cpp` 是 VM 的 native 桥(VM 本体,非 JDK 库)。
- `runtime/synchronizer.cpp:602-640` `get_next_hash`:hashCode 模式 0–5;**mode 4 = 原始对象地址**,
  mode 3 = 顺序计数器,mode 5(默认)= Marsaglia xor-shift。对零 GC、u32 句柄堆的 VM,
  **mode 4(用 u32 句柄 idx)** 最简且忠实于一个真实 HotSpot 模式。
- `classfile/classLoader.cpp` `ClassPathZipEntry`(声明 `classLoader.hpp:87-99`、实现
  `classLoader.cpp:361-434`):`open_stream(name)` → `ZipLibrary::find_entry`(中心目录)→
  `read_entry`(DEFLATE 则解压)→ `ClassFileStream`;类名 → `name + ".class"`
  (`file_name_for_class_name`,`classLoader.cpp:987-1003`)。**惰性按需**(仅 `load_class` 时取字节)。
- jimage(`ClassPathImageEntry`/`lib/modules`,完美哈希自定义格式)**非 zip**,本层**顺延**(独立层)。

**JDK(辅助,少看,仅证映射与格式):**

- `src/java.base/share/native/libjava/{System,Object,Class}.c` 的 `JNINativeMethod methods[]` 表:
  证 `currentTimeMillis`/`nanoTime`/`arraycopy` → 对应 `JVM_*`(仅查映射,VM 语义仍看 `jvm.cpp`)。
- `src/java.base/share/native/libzip/zlib/inflate.c`(zlib 第三方,vendored):DEFLATE 解压参考实现
  (算法以 RFC 1951 为权威,zlib 仅参考)。

**实证(本机):** `C:\Program Files\Java\jdk-25.0.2\jmods\java.base.jmod` 中
`classes/java/lang/Object.class` 的 zip method = **8(DEFLATED)**(python `zipfile` 核验)→
真实 jmod 类字节**须 DEFLATE 解压**;`jar -0` 可造 STORED jar(闸门免解压路径用)。

## 3. 范围(分四子增量,各自 TDD + 闸门)

**做:**

- **4.10a DEFLATE 解压**(`inflate` 纯函数,RFC 1951):零依赖、零 unsafe、纯 `&[u8] -> Vec<u8>`。
  无 VM 耦合,独立 TDD(对已知压缩流断言)。**容器半的地基。**
- **4.10b zip 容器读取**(`ZipReader`):EOCD → 中心目录 → 条目索引(name/method/offset/crc);
  `read(name)` 按 method STORED 直读 / DEFLATE 调 4.10a。jmod 跳 4 字节 magic 前缀;
  条目名规范化(jar 直接;`jmod classes/foo` → `foo`)。
- **4.10c native 方法绑定**:`invoke*` 解析到 ACC_NATIVE(无 Code)→ `native::invoke(vm,class,name,desc,args)`
  内置表查 → 调 Rust 实现 → 按返回描述符回填 `Value`;未注册 → `UnsatisfiedLinkError`(ThrownException)。
  最小集:`registerNatives()V`(空实现,最高杠杆,解锁真实 `<clinit>`)、`Object.hashCode()I`
  (=句柄 idx,mode 4)、`System.currentTimeMillis()J`/`nanoTime()J`(真实时间)。
- **4.10d ClassPath + 惰性 ClassLoader + 闸门**:容器路径列表 + `load_class(name) -> Option<ClassFile>`
  (`name+".class"`,逐容器查,strip `classes/`);**惰性**加载入注册表(真 ClassLoader 雏形);
  闸门:从真 `java.base.jmod` 加载 `java/lang/Object`,断言加载名匹配(+ 若能跑其 `<clinit>`/
  一最小 native 调用则更佳)。

**顺延(明确不做,记路线图):**

- jimage 读取器(`lib/modules`,自定义完美哈希格式)——独立层,北极星可先用 jmod 绕过。
- 完整校验器(JDK 类已被 JDK 校验 → 宽松;仅解析+准备+`<clinit>` 触发,沿用 4.9)。
- JNI/dlopen 动态 native(name 派生解析外部库)——**仅内置表**(非完整 JNI)。
- 更全 native 集:`System.arraycopy`(复杂)、`Object.clone`、`getClass`/反射(须先有
  `java.lang.Class` 实例化,顺延)、Thread/`Unsafe` 等——按需逐个加。
- `ConstantValue` 属性、异常 `cause` 链(沿用 4.9 顺延)。

## 4. native 绑定设计(4.10c)

- **触发**:各 `invoke_*` 在 `find_*method` 取到方法后,若方法为 native(`code.is_none()` 且
  非抽象 = ACC_NATIVE;抽象仍报错)→ 走 `native::invoke`,不再 `BadConstant`。
- **内置表**(编译期 `match`,镜像 libjava `JNINativeMethod[]` 之并集;VM 语义依 `jvm.cpp`):
  ```text
  invoke_native(vm, class, name, desc, args:&[Value]) -> Result<Value,VmError>
    match (class, name) {
      ("java/lang/Object"|"java/lang/System"|"java/lang/Class", "registerNatives") => Ok(Void),
      ("java/lang/Object", "hashCode") => Ok(Int(objref_handle(args[0]))),   // mode 4
      ("java/lang/System","currentTimeMillis") => Ok(Long(real_ms())),
      ("java/lang/System","nanoTime")          => Ok(Long(real_nanos())),
      _ => Err(throw_exception(vm, "java/lang/UnsatisfiedLinkError")),
    }
  ```
  (`args` 对虚方法含 this-ref 在前,static 无 this;按返回描述符构造 `Value`。)
- **identity hash**:用既有 u32 句柄(`Reference` 内 idx)直接 `idx as i32`——对应 HotSpot **mode 4**
  (原始对象地址),**零新增状态**。
- **借用**:沿用 `'a` 借用技巧(native 实现需 `&mut vm` 抛异常/读堆/写静态)。

## 5. 容器加载设计(4.10a/b/d)

- **4.10a inflate**:`pub fn inflate(compressed:&[u8]) -> Result<Vec<u8>, InflateError>`(RFC 1951:
  bit 读取器 + 固定/动态 Huffman + LZ77 回溯)。模块 `runtime::zip::inflate`。单元测试对数个已知
  DEFLATE 流(含 stored / fixed / dynamic Huffman 块)断言解压输出。
- **4.10b ZipReader**:`ZipReader::new(bytes) -> Result<Self>`(从尾部 EOCD 倒找 → 中心目录);
  `has(name)` / `read(name) -> Option<Vec<u8>>`(STORED 切片 / DEFLATE 调 `inflate`)。jmod 首 4 字节
  magic 前缀无害(EOCD 在尾部倒找)。条目名:`classes/foo` → `foo`。
- **4.10d ClassPath**:`ClassPath { containers: Vec<ZipReader> }`,`load_class(name) -> Option<ClassFile>`
  (`name+".class"`,逐容器查)。**惰性**:首次引用某未加载类时加载入注册表(真 ClassLoader 雏形;
  与 4.9 `ensure_class_initialized` 协同)。
- **闸门**:读真 `java.base.jmod`,`load_class("java/lang/Object")` 非空 + 解析成功 + 类名匹配 +
  `0xCAFEBABE` 魔数;另以 `jar -0` 造 STORED jar 验免解压路径。

## 6. 测试

- **4.10a** 单元(无 javac):已知 DEFLATE 流解压(stored/fixed/dynamic 各一)。
- **4.10b** 单元:`jar -0`/手造 STORED zip 列条目 + `read`;对真实 `java.base.jmod` 列
  `classes/java/lang/Object.class` + `read`(走 DEFLATE)非空且首 4 字节 `0xCAFEBABE`。
- **4.10c** 单元:合成 native 类(ACC_NATIVE 无 Code)→ `invoke` 命中内置表回填;未注册 →
  `UnsatisfiedLinkError`(断言异常类名)。
- **4.10d** 集成(需 JDK jmod,无则跳过):从 `java.base.jmod` 惰性加载 `Object` + 跑一个调
  `registerNatives`/`hashCode` 的最小真实类(若可行)。

## 7. 债务影响

- **解锁**:真实 java.base 容器加载 + native 执行 = 退役 Stage A 引导桩的最后前置(4.10 全完即可
  尝试增量加载真类,删 15 条桩 + `install_bootstrap`)。
- **净增技术债**:DEFLATE/zip 为标准格式(非一次性桩);native 内置表随用随扩(显式注册,HotSpot 亦如此)。
- **不触动**:既有 invoke/field/异常/`<clinit>` 路径结构(仅 native 分支新增);`#![deny(unsafe_code)]`
  保持(inflate/zip/native 全 safe Rust,零依赖)。

## 8. 子增量依赖

```
4.10a inflate ──► 4.10b ZipReader ──► 4.10d ClassPath+ClassLoader+闸门
4.10c native 绑定(独立,可与 a/b 并行;闸门 d 可选纳入 native 调用)
```

每个子增量各自走 brainstorm(已并入本 spec)→ plan → TDD(红→绿)→ 闸门 → 提交。
