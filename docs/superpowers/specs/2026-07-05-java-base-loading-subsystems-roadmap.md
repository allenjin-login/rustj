# 加载完整 java.base 的五大子系统 —— 路线图设计

> 北极星:**完整加载 java.base**。本路线图覆盖五大子系统(模块系统 / 类加载器 /
> 层加载器 / 反射 / 动态 dll 加载),按依赖顺序拆成 7 个递增层。每层独立走
> brainstorm→spec→TDD(红→绿)→javac 闸门→commit。

## 0. 现状(2026-07-05 快照)

rustj 已能:`ClassPath`(jmod/jar)+ `load_closure`(急切预载引用闭包)+ 真 `Integer`/
`String`/`ArrayList`/`HashMap`/完整 invokedynamic 端到端运行。`ClassRegistry` 为不可变借用,
`Vm` 构造前急切灌入整个闭包。`Class.getClassLoader0()` native 返 null(把所有类当引导类);
`Class.getModule()` 返 null;`VM.isModuleSystemInited()` 返 false;反射几乎为零;native 全为编译期表。

## 1. 五大子系统的依赖序与层次分解

| 层 | 子系统 | 交付 | 边界度 |
|---|---|---|---|
| **4.11** | 模块系统(解析) | `module-info.class` 的 Module 属性 → `ModuleDescriptor` | 小(纯解析) |
| **4.12** | 类加载器(身份) | `LoadedClass.loader: LoaderId` + `getClassLoader0` native | 小 |
| **4.13a** | 模块系统(集成) | `java/lang/Module` 对象 + `Class.getModule()` native | 中 |
| **4.13b** | 层加载器 | `ModuleLayer.boot()` + `VM.initLevel=2` | 中 |
| **4.14a** | 反射(类元数据) | `Class.forName0` + `getDeclared*0` native | 中 |
| **4.14b** | 反射(调用) | `Method.invoke0` / `Constructor.newInstance` 复用解释器 | 中 |
| **4.15** | 动态库加载 | `JVM_LoadLibrary`/`FindLibraryEntry` + `NativeLibraries.load` + `JNI_OnLoad` | 大 |

依赖:`4.11 → 4.12 → 4.13a → 4.13b`(模块/层);`4.14*` 依赖 `4.13b`(反射访问检查需模块);
`4.15` 独立。反射与 DLL 可与 4.13 并行,但本路线图按用户列举顺序串行推进。

## 2. 架构决策(贯穿全五子系统)

**保留单 `ClassRegistry` + 急切 `load_closure` 架构,不重写为真实惰性委托类加载器。**

理由:
1. **java.base 由 bootstrap(null)加载** —— `getClassLoader0()` 忠实返 null,无需建模委托链。
2. **不可变借用模型** —— `Vm` 以 `&'a ClassRegistry` 借注册表;改惰性需 &mut 注册表,牵动全 crate。
3. **ClassLoader 身份建模为 `LoaderId` 标签** —— `LoadedClass.loader: LoaderId`(Bootstrap/Platform/App)
   足以服务 java.base 完整加载与未来用户类。真委托顺延。

**第三方 crate / unsafe 政策**(CLAUDE.md §1-2):`4.15` 的 `LoadLibrary`/`GetProcAddress`/
`dlopen`/`dlsym` 经具体 item 上 `#[allow(unsafe_code)]` 开窗;不引 `libloading` crate(直接 FFI,
沿用既有 libc 政策「真正需 OS 调用时才引」——此处直接系统调用即可,libloading 非必要)。

**JNIEnv 边界**(§3 源码核验):HotSpot `JNIEnv`(`struct JNINativeInterface_`,`jni.cpp:3164`)的
~200 函数表 ABI 绑死其内部。**任意 JNI 库的完整 JNIEnv 忠实重写**是巨型工程,**顺延**;
`4.15` 聚焦:`System.loadLibrary` 链 + 加载/符号查找 + `JNI_OnLoad` 生命周期 + `Java_*` 符号
派发(扩展编译期 native 表为运行期注册表)。java.base 自身 native 已由编译期表覆盖,故加载
java.base 不依赖完整 JNIEnv。

## 3. 各层详设(进入实现时各自再起独立 spec)

### Layer 4.11 — module-info.class 解析 + ModuleDescriptor(当前层)

- **Module 属性**(JVMS §4.7.25)解码:`Module_descriptor{name_index,flags,version_index,
  requires[],exports[],opens[],uses[],provides[]}`。常量池 `Module`(19)/`Package`(20)标签
  与 `ConstantPoolEntry::{Module,Package}`(entry.rs:62/64)已支持,可直接解析名。
- 新增 `parse_module_attribute(info: &[u8]) -> Result<ModuleAttribute, ClassFileError>`
  (镜像 `parse_bootstrap_methods`:cp 无关纯解码,属性名识别在调用方经 cp 做)。
- 新增 `metadata::ModuleDescriptor` 高层结构(owned 解析后的 requires/exports/opens/uses/provides,
  名为 `String`)+ `from_class_file(&ClassFile)` 取 Module 属性并解码(经 cp 解 Module/Package 名)。
- **TDD 红**:手编 Module 属性字节 → 解码断言;**javac 闸门**:真 java.base.jmod 的
  `module-info` 解码出 `name=="java.base"`、`requires` 含 `java.base` 自身无、有 `requires mandated java.base`?

  实际 java.base module-info:`module java.base { exports java.lang; exports java.util; ... requires java.base? }`
  ——java.base 不 requires 自己;断言 `exports` 含 `java/lang`、`java/util` 包,`requires` 仅(可能)transitive。
- 纯解析层,零 VM 改动。

### Layer 4.12 — **退役 `Oop::Class` → 真 `java/lang/Class` Instance**(修订:探针发现)

> **修订原因(2026-07-05 探针)**:JDK 25 的 `Class.getClassLoader()`(Class.java:982)、
> `getModule()`(:1005)、`getName()`(:959)、`isArray()`(:817)、`isPrimitive()`(:860)、
> `getComponentType()`(:1303) **全是真字节码字段读**(`return classLoader;` / `return name != null ? name : initClassName();` / `return componentType != null;` / `return primitive;`)。
> 但 rustj 的 `Oop::Class` 镜像在 `invokevirtual/interface` 收者为镜像时**整体路由到固定 native 表**
> (invoke.rs:867/985),从不回落真 Class 字节码;native 表又只含若干桩(`getModule`→null、
> `getClassLoader0`→null、`desiredAssertionStatus`→0),`public getClassLoader`/`getName`/…
> 不在表 → `UnsatisfiedLinkError`。**这是「完整加载 java.base + 反射 + 模块系统」的真地基阻塞。**

- **表示变更**:移除 `Oop::Class(ClassOop)` 变体与 `ClassOop` 结构(`oops/class_oop.rs`)。
  Class 镜像改为真 `Oop::Instance`(`java/lang/Class`)。`java/lang/Class` 已被 `load_closure`
  传递预载(loader.rs:256-260,Object.getClass 返回类型)→ **无新闭包风险**;其 `<clinit>`
  仅 `runtimeSetup()`→`registerNatives()`(空操作,Class.java:232-241)→ **安全**。
- **`intern_class_mirror(name)`**:取 `java/lang/Class` 的 `LoadedClass` → `new_instance` →
  经 `instance_field(lc,name,desc)` 动态查序号,置 VM 字段:`name`(外部形:类 `/`→`.`、
  原语如 `int`、数组 `[I`/`[Ljava.lang.String;`)、`componentType`(数组→组件镜像,否则 null)、
  `primitive`(原语→true);`classLoader`/`module` 默认 null(Bootstrap,4.13a 才填 module)。
  缓存双向:`class_mirrors: name→Reference`(既有)+ 新 `mirror_class: Reference→internal-name`
  (供 native 反查镜像所表示的类)。
- **分派路径清理**:移除 `invoke_virtual/interface` 的 `Oop::Class` 早分支(镜像成 Instance →
  正常类链分派;真 Class 字节码运行,ACC_NATIVE 法经既有 native 表 keyed on `java/lang/Class`)。
  移除 `getfield/putfield`(field.rs:181/221)、`type_check`、`array`、`heap`、`exception`、
  `arraycopy`、`native/mod.rs:95`(Instance.class_name()=="java/lang/Class" 自洽)的 `Oop::Class` 臂。
- **新增/调整 Class native**(java_lang.rs):
  - `registerNatives()V` — 空操作(既有策略)。
  - `initClassName()Ljava/lang/String;` — 防御:置并返 `name`(预置则 getName 不调;经
    `mirror_class` 取内部名→外部形)。
  - `isInstance(Ljava/lang/Object;)Z` — `registry.is_instance(镜像类, 实参类)`。
  - `isAssignableFrom(Ljava/lang/Class;)Z` — `registry.is_instance(实参镜像类, 本镜像类)`。
  - `getSuperclass()Ljava/lang/Class;` — 镜像类的 `super_class_name` → 镜像;接口/原语→null。
  - `desiredAssertionStatus0(Ljava/lang/Class;)Z` — 0(替换原 `desiredAssertionStatus` 桩,
    该法现由真字节码 `return desiredAssertionStatus0(this)` 进入)。
  - 移除原 `getClassLoader0`/`getModule`/`desiredAssertionStatus` 桩——现由真字节码字段读覆盖
    (`getClassLoader` 读 `classLoader`=null;`getModule` 读 `module`=null)。
  - `getPrimitiveClass` 既有保留。
- **ClassLoader 身份折入**:`getClassLoader()` 经真字节码读 `classLoader` 字段(null=Bootstrap)
  即忠实;`LoaderId` 标签顺延(无委托链需求)。`findLoadedClass0`/`findBootstrapClass`/`defineClass1`
  顺延到真有用户类加载器时。
- **闸门**(javac + jmod):真 java.base 程序 `Integer.class.getName()`="java.lang.Integer"、
  `.getSuperclass()`=Number、`Number.class.isAssignableFrom(Integer.class)`=true、
  `int[].class.isArray()`=true、`int.class.isPrimitive()`=true、`.getClassLoader()`=null、
  `.getModule()`=null;**既有 `class_mirror.rs` 身份相等(literalTwice/getClassEq/distinct)仍绿**。

### Layer 4.13a — Module 对象模型 + Class.getModule()

- `Module` 表:`module_name → { descriptor, packages, loader }`。从 `module-info.class` 构建
  (4.11)+ 从模块的 `exports` 推导包集合。
- 包→模块映射:类的内部名剥最后 `/` 后段 = 包名;查表得模块。
- 新 `Oop`?**否**——`Module` 是普通 `Oop::Instance`(真 `java/lang/Module` 类从 jmod 载,字段
  `name/loader, ...`)。需预载真 `java/lang/Module` 闭包。
- Native `Class.getModule()Ljava/lang/Module;` → 按类包查模块,返其 `Module` 实例镜像。
- Native `Module.defineModule0`/`addReads0`/`addExports0` 等作注册桥(经 (类,名) 特判收集,
  形同 LambdaMetafactory 决策)。

### Layer 4.13b — ModuleLayer.boot + 模块系统初始化

- `VM.initLevel = MODULE_SYSTEM_INITED(2)`(VM.java:43-48):引导阶段置 2,使
  `isModuleSystemInited()`(VM.java:92 `initLevel >= 2`)→ true。修 4.10r 的 `initLevel` 默认 0 假。
- `ModuleLayer.boot()Ljava/lang/ModuleLayer;` → 返引导层对象(真 `java/lang/ModuleLayer` 实例)。
- 引导层:`Configuration` 仅 java.base(解析后单模块)、bootstrap loader、modules=[java.base Module]。
- `ModuleLayer`/`Configuration` 真类预载 + 桥 native(`defineModulesWithOneLoader` 等)。

### Layer 4.14a — 反射类元数据 native

- `Class.forName0(LString;ZLjava/lang/ClassLoader;)Ljava/lang/Class;`(jvm.cpp):按名查注册表,
  `init=true` 触发 `ensure_class_initialized`,返类镜像;未找到→`ClassNotFoundException`。
- `getDeclaredFields0(Z)`/`getDeclaredMethods0(Z)`/`getDeclaredConstructors0(Z)` native →
  遍历 `LoadedClass.cf.{fields,methods}`,构造真 `java.lang.reflect.{Field,Method,Constructor}[]`
  (从 jmod 载真 reflect 类;`slot` = 字段/方法在本类的序)。
- `getSuperclass`/`getInterfaces0`/`getModifiers`/`isAssignableFrom` 多为字节码(java.base 自实现);
  仅 native 桥缺失者补。

### Layer 4.14b — 反射调用

- `jdk/internal/reflect/DirectMethodHandleAccessor.invoke0(LMethod;LObject;[LObject;)LObject;`
  → 经 `Method` 的 `clazz`+`slot` 取回 `MethodInfo` → 复用 `run_callee`(静态/实例分派)→ 回填。
- `Constructor.newInstance` 类似(分配 + `<init>`)。
- `Field.get/set` 经 `slot` 取字段序号 → getfield/putfield 通路(或 Unsafe 路径)。
- 访问检查:publicOnly 参数过滤 + 顺延完整 module 访问。

### Layer 4.15 — 动态库加载

- `os::dll_load`/`dll_lookup` Rust 化:`LoadLibraryW`/`GetProcAddress`(win32)/
  `dlopen`/`dlsym`(posix),`#[allow(unsafe_code)]` 开窗,跨平台 `#[cfg]`。
- Native 桥:`jdk/internal/loader/NativeLibraries.load`(`NativeLibraryImpl`,`name`,`isBuiltin`,
  `throwExceptionIfFail`)→ 返 handle/成败;`findBuiltinLib`;`unload`。
- `JNI_OnLoad`/`JNI_OnUnload` 生命周期:加载后查 `JNI_OnLoad` 符号,有则调(返 JNI 版本)。
- `Java_*` 符号派发:运行期注册表扩展编译期 native 表(`JVM_RegisterNatives` → 名→符号→handler)。
  完整 JNIEnv(~200 函数)顺延;本层仅支持「库提供 `Java_*` 符号 + 我们已知的 JNIEnv 子集」。

## 4. 验收标准(整体)

**能完整加载 java.base**:从 `java.base.jmod` 载入 java.base 模块,其 `module-info` 解析为
`ModuleDescriptor`,引导层 `ModuleLayer.boot()` 含 java.base,`Class.getModule()`/`getClassLoader()`
经真字节码运行返正确值,反射 `Class.forName`/`getDeclaredMethods` 在真 java.base 程序中可用,
`System.loadLibrary` 链不抛 `UnsatisfiedLinkError`(加载 + OnLoad 成功)。

## 5. 已知顺延(明确不在本路线图)

- 真实惰性委托类加载器(`URLClassLoader`、用户自定义 `ClassLoader.loadClass` 全语义)。
- 完整 JNIEnv(~200 函数)的任意 JNI 库支持。
- `MethodHandle` 直接调用(`invokeexact`/`invoke`,见既有候选 g)。
- 模块服务加载器(`ServiceLoader` 与 `provides`/`uses` 完整运行时解析)。
- 真实 `defineClass0/1` 动态字节码定义(隐藏类、运行期生成)。
