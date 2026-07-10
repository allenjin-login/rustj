//! VM 运行时初始化(Layer 4.13)—— 对应 HotSpot native launcher 在 `Threads::create_vm` 后
//! 调用的 `System.initPhase1-3`(`System.java:1724/1929/1952`)启动序列。
//!
//! Phase 1 的核心 [`initialize_system_class`]:引导 `jdk.internal.misc.VM.savedProps`,使
//! `VM.getSavedProperty`(`VM.java:209`)不再抛 `IllegalStateException("Not yet initialized")`
//! ——凡读 savedProps 的 `<clinit>`(Integer/Long/Boolean/… 的缓存)都依赖此先跑。修前由测试用
//! `RustjBootstrap` Java 辅助类手动调 `VM.saveProperties(new HashMap<>())` 充数;本模块收编为
//! VM 原生能力,使任何用户程序开跑前 `savedProps` 已就绪。

use crate::constant_pool::ConstantPoolEntry;
use crate::metadata::MethodInfo;
use crate::oops::{LoadedClass, Oop};
use crate::runtime::{Frame, Interpreter, Reference, Vm, VmError};

/// **VM 运行时初始化 Phase 1**(`System.initPhase1` 的等价最小子集,`System.java:1720-1836`)。
///
/// 在 `Vm::new` 后、用户代码前调用:构造初始系统属性表(当前空 `HashMap`,等价旧
/// `RustjBootstrap`;后续可逐项补真 launcher 属性)→ 经解释器 `invokestatic
/// jdk/internal/misc/VM.saveProperties(Ljava/util/Map;)V`(`VM.java:237` 真字节码,置
/// `savedProps`、算 `directMemory=Runtime.maxMemory()`、`pageAlignDirectMemory`)→
/// `invokestatic VM.initLevel(I)V` 置 1。
///
/// **前置**:注册表须已闭包预载 `jdk/internal/misc/VM` + `java/util/HashMap`
/// (Integer 等闭包会传递性载入 VM;HashMap 须显式预载,4.10h/real_integer.rs 同此)。
pub fn initialize_system_class(vm: &mut Vm) -> Result<(), VmError> {
    // 构造空 HashMap 实例:table=null 默认,HashMap.get 经 `table==null` 短路返 null
    //(等价旧 RustjBootstrap 的 `new HashMap<>()` 不跑 <init>;saveProperties 仅 .get 键)。
    let map_ref = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("Phase 1 引导需要类注册表"))?;
        let hm_lc = reg
            .get("java/util/HashMap")
            .ok_or(VmError::BadConstant("Phase 1 须预载 java/util/HashMap"))?;
        vm.heap_mut()
            .alloc(Oop::Instance(reg.new_instance(hm_lc)))
    };

    // invokestatic VM.saveProperties(Ljava/util/Map;)V —— 真字节码:置 savedProps=map、
    // directMemory=Runtime.maxMemory()(native 已支持)、pageAlignDirectMemory=false(空表)。
    invoke_static_void(vm, "jdk/internal/misc/VM", "saveProperties", |frame| {
        frame.locals.set_reference(0, map_ref)
    })?;

    // invokestatic VM.initLevel(I)V —— 置 1(VM.java:61;`value>initLevel && value<=SHUTDOWN`
    // 单调上行校验通过)。Phase 1 完成,后续 Phase 2(模块引导)可上行至 2。
    invoke_static_void(vm, "jdk/internal/misc/VM", "initLevel", |frame| {
        frame.locals.set_int(0, 1)
    })?;

    // System.props = 真 Properties 实例(对应 `System.initPhase1`:1798 `props = createProperties(...)`)。
    // 真 `createProperties` 跑 `Properties.<init>` → `map = new ConcurrentHashMap<>(...)`,后者拉入
    // CHM.<clinit> 的 `Unsafe.arrayIndexScale` 等并发原语(顺延:并发山)。rustj 单线程下,Properties
    // 的 `get`/`put`/`remove` 全委派 `map` 字段(Properties.java:1336+),HashMap 是合法单线程后盾
    //(HashMap.get 空 table 短路返 null、HashMap.put 首次 resize 分配 table),故置空 HashMap Instance
    // 即功能等价 —— 使 `System.getProperty` 返 null(非 NPE),解锁 `ClassLoaders.<clinit>:85`。
    // 条件:`java/util/Properties` + `java/lang/System` 均已预载(否则跳过,保旧测试兼容)。
    install_system_props(vm)?;

    // System.setJavaLangAccess()V —— 安装 SharedSecrets.javaLangAccess(Layer 4.30)。真 JDK 由
    // `System.initPhase1`(`System.java:1778`)首步调之;rustj 抽为独立步。置 javaLangAccess 字段后,
    // `AbstractClassLoaderValue.map` → `JLA.createOrGetClassLoaderValueMap` 不再 NPE,解锁
    // `ClassLoaders.<clinit>` → `getSystemClassLoader()` 整链。须在 props 之后(安装可能触发 System.<clinit>)。
    install_java_lang_access(vm)?;

    Ok(())
}

/// 安装 `SharedSecrets.javaLangAccess`(Layer 4.30)—— 经 `invokestatic
/// java/lang/System.setJavaLangAccess()V`(`System.java:1995`,**私有静态**;rustj 不查访问控制,
/// `find_static_method` 遍历全部方法不滤 access)。真 JDK 由 `System.initPhase1`(`System.java:1778`)
/// 首步调之;rustj 的 `initialize_system_class` 是 initPhase1 最小子集,故单独抽出此步。
///
/// `setJavaLangAccess` 体 = `SharedSecrets.setJavaLangAccess(new JavaLangAccess(){...})`:分配
/// `System$1` 匿名 `JavaLangAccess` 实例(**~80 方法体安装期不跑**,仅按需惰性调用)→ 置
/// `SharedSecrets.javaLangAccess` 静态字段。置后 `AbstractClassLoaderValue.map`(AbstractClassLoaderValue.java:266)
/// 的 `JLA.createOrGetClassLoaderValueMap(cl)` 不再 NPE → `ClassLoaders.<clinit>` →
/// `ArchivedClassLoaders.archive` → `ServicesCatalog` 全链通 → `getSystemClassLoader()` 返非 null。
///
/// **前置**:`java/lang/System` 已预载(由 `install_system_props` 保证;其内已 `get("java/lang/System")`)。
/// System 未预载 → 静默跳过(保旧测试兼容,同 `install_system_props` 防御)。
fn install_java_lang_access(vm: &mut Vm) -> Result<(), VmError> {
    let has_system = vm
        .registry()
        .map(|r| r.get("java/lang/System").is_some())
        .unwrap_or(false);
    if !has_system {
        return Ok(());
    }
    invoke_static_void(vm, "java/lang/System", "setJavaLangAccess", |_| Ok(()))
}

/// 构造真 `java.util.Properties` 实例并写 `System.props` 静态字段(Phase 1 收尾,Layer 4.20),
/// 再装入真 launcher 系统属性(Layer 4.26)。
///
/// `Properties` Instance 默认初始化(`map`=null)→ `getProperty` 会 `map.get(key)` NPE。故:
/// 1. 分配空 `HashMap` Instance(table=null 默认;`HashMap.get` 短路返 null、`HashMap.put` 首次
///    `resize` 分配 table —— 单线程下功能完整)。
/// 2. 分配 `Properties` Instance,置其 `map` 字段 = 该 HashMap(Properties.put/get/remove 委派之)。
/// 3. `System.props` 静态字段 ← 该 Properties Instance(沿超类链解析声明类 + 序号,Mutex 写)。
/// 4. 经 `Properties.put(Object,Object)Object` 真字节码逐项写入 OS 派生的 launcher 系统属性
///    (Layer 4.26):`file.separator`/`path.separator`/`user.dir`/…—— 解锁 `WinNTFileSystem.<init>:95`
///    等 `props.getProperty("file.separator").charAt(0)`(空 props → null → NPE)。
///
/// `java/util/Properties` 或 `java/lang/System` 未预载 → 静默跳过(保 `vm_system_bootstrap` 旧闸门:
/// 仅预载 Integer/HashMap/String,无 Properties/System 时仍绿)。
fn install_system_props(vm: &mut Vm) -> Result<(), VmError> {
    use crate::metadata::descriptor::FieldType;
    use crate::runtime::Slot;

    let reg = match vm.registry() {
        Some(r) => r,
        None => return Ok(()),
    };
    let props_lc = match reg.get("java/util/Properties") {
        Some(lc) => lc,
        None => return Ok(()),
    };

    // 1) 空 HashMap Instance(Properties.map 后盾)。&'a 引用不绑 &self(§6)→ 出块后 heap_mut 独占。
    let map_ref = {
        let Some(hm_lc) = reg.get("java/util/HashMap") else {
            return Ok(());
        };
        vm.heap_mut()
            .alloc(Oop::Instance(reg.new_instance(hm_lc)))
    };

    // 2) Properties Instance,置 map 字段 = HashMap。flattened_instance_fields 含继承字段;按名查
    //    `map`(Properties 自身声明的 CHM 字段)序号。字段未见(桩精简)→ 跳过置入但仍写 System.props。
    let props_ref = {
        let mut inst = reg.new_instance(props_lc);
        if let Some(ord) = reg
            .flattened_instance_fields(props_lc)
            .iter()
            .position(|f| f.name == "map")
        {
            inst.set_field(ord, Slot::Reference(map_ref));
        }
        vm.heap_mut().alloc(Oop::Instance(inst))
    };

    // 3) System.props = props_ref(对应 `props = createProperties(tempProps)`)。resolve_static_field
    //    沿超类链解析声明类(System 本身)+ 序号;System 未加载 → 返 None 静默跳过。经 Mutex 写。
    let ft = FieldType::Class("java/util/Properties".to_string());
    if let Some((sys_lc, props_ord)) = reg.resolve_static_field("java/lang/System", "props", &ft) {
        sys_lc.static_storage.lock().unwrap()[props_ord] = Slot::Reference(props_ref);
    }

    // 4) Phase 1 launcher 系统属性(对应真 launcher 经 native 注入、`System.initPhase1` 装入 props)。
    //    经 Properties.put 真字节码写入(map.put → HashMap.put;单线程后盾,首次 resize 分配 table)。
    //    **不含 java.class.path**(保 4.20 闸门:getProperty("java.class.path") 仍 null)。
    populate_launcher_props(vm, props_ref)?;
    Ok(())
}

/// 向 `System.props` 写入 OS 派生的 launcher 系统属性(Phase 1,Layer 4.26)。
///
/// 对应真 JVM 的 native launcher(`System.c` / `java.c`)经 `getSystemProperty` 注入、
/// `System.initPhase1`(`System.java:1720-1836`)装入 `props` 的标准属性集。rustj 不跑
/// `initPhase1`,故在此直接 `Properties.put` 真字节码逐项写入(委派 `map.put` → HashMap.put)。
/// 值从 OS 派生:`file.separator`=MAIN_SEPARATOR、`user.dir`=current_dir、… —— 解锁
/// `WinNTFileSystem.<init>:95` 等读 `file.separator`/`path.separator`/`user.dir` 的代码。
fn populate_launcher_props(vm: &mut Vm, props_ref: Reference) -> Result<(), VmError> {
    let is_win = std::path::MAIN_SEPARATOR == '\\';
    let sep = std::path::MAIN_SEPARATOR.to_string();
    let path_sep = if is_win { ";" } else { ":" }.to_string();
    let line_sep = if is_win { "\r\n" } else { "\n" }.to_string();
    let user_dir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let user_home = std::env::var(if is_win { "USERPROFILE" } else { "HOME" })
        .unwrap_or_else(|_| ".".to_string());
    let user_name = std::env::var(if is_win { "USERNAME" } else { "USER" })
        .unwrap_or_else(|_| "rustj".to_string());
    let tmpdir = std::env::temp_dir().display().to_string();
    let os_name = if is_win { "Windows" } else { "Linux" }.to_string();
    let os_arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        a => a,
    }
    .to_string();
    let java_home = std::env::var("JAVA_HOME")
        .or_else(|_| std::env::var("java.home"))
        .unwrap_or_else(|_| ".".to_string());

    let props: &[(&str, &str)] = &[
        ("file.separator", sep.as_str()),
        ("path.separator", path_sep.as_str()),
        ("line.separator", line_sep.as_str()),
        ("user.dir", user_dir.as_str()),
        ("user.home", user_home.as_str()),
        ("user.name", user_name.as_str()),
        ("java.io.tmpdir", tmpdir.as_str()),
        ("os.name", os_name.as_str()),
        ("os.arch", os_arch.as_str()),
        ("os.version", "10.0"),
        ("java.home", java_home.as_str()),
        ("java.version", "25"),
        ("java.specification.version", "25"),
        ("java.vm.specification.version", "25"),
        ("file.encoding", "UTF-8"),
        ("sun.jnu.encoding", "UTF-8"),
        ("native.encoding", "UTF-8"),
        ("stdin.encoding", "UTF-8"),
        ("stdout.encoding", "UTF-8"),
        ("stderr.encoding", "UTF-8"),
        ("sun.stdout.encoding", "UTF-8"),
        ("sun.stderr.encoding", "UTF-8"),
    ];
    for (k, v) in props {
        put_property(vm, props_ref, k, v)?;
    }
    Ok(())
}

/// 经解释器跑真字节码 `Properties.put(Object,Object)Object` 写入一个系统属性。
/// 委派 `map.put` → HashMap.put(单线程后盾)。&'a 引用(reg/lc/m/code/CP)不绑 &self(§6)→
/// 出块后 `interpret_with(&mut vm)` 可独占。
fn put_property(
    vm: &mut Vm,
    props_ref: Reference,
    key: &str,
    value: &str,
) -> Result<(), VmError> {
    let key_ref = crate::runtime::interpreter::string::intern(vm, key)?;
    let val_ref = crate::runtime::interpreter::string::intern(vm, value)?;
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("put_property 需要类注册表"))?;
    let lc = reg
        .get("java/util/Properties")
        .ok_or(VmError::BadConstant("put_property:Properties 须预载"))?;
    let m = find_method_by_sig(
        lc,
        "put",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
    )
    .ok_or(VmError::BadConstant(
        "Properties.put(Object,Object)Object 未找到",
    ))?;
    let code = m
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("Properties.put 须为真字节码"))?;
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    frame.locals.set_reference(0, props_ref)?;
    frame.locals.set_reference(1, key_ref)?;
    frame.locals.set_reference(2, val_ref)?;
    interp.interpret_with(&mut frame, vm)?;
    Ok(())
}

/// 按名 + 描述符查方法(launch 引导用 `Properties.put`,与 `find_static_method` 的仅按名查区分)。
fn find_method_by_sig<'a>(
    lc: &'a LoadedClass,
    name: &str,
    desc: &str,
) -> Option<&'a MethodInfo> {
    for m in &lc.cf.methods {
        let Ok(ConstantPoolEntry::Utf8(n)) = lc.cf.constant_pool.get(m.name_index) else {
            continue;
        };
        let Ok(ConstantPoolEntry::Utf8(d)) = lc.cf.constant_pool.get(m.descriptor_index) else {
            continue;
        };
        if n.as_str() == name && d.as_str() == desc {
            return Some(m);
        }
    }
    None
}

/// **VM 运行时初始化 Phase 2**(模块系统引导,`System.initPhase2` 等价,`System.java:1929`)。
///
/// 在 Phase 1 之后、用户代码前调用,对应真 JVM 的:
/// 1. `bootLayer = ModuleBootstrap.boot();`(`System.java:1932`)—— 分配真 `java/lang/ModuleLayer`
///    Instance 并置 `System.bootLayer` 静态字段(引导层单例对象)。
/// 2. `VM.initLevel(2);`(`System.java:1941`)—— `MODULE_SYSTEM_INITED`(`VM.java:45`),
///    使 `isModuleSystemInited()`(`initLevel >= 2`)→ true。
///
/// ModuleLayer Instance 的内部字段(`cf`/`parents`/`nameToModule`)当前不填——本层门
/// 仅依赖 `ModuleLayer.boot()`(= `getstatic System.bootLayer`)非 null + `Module.getLayer()`
/// 对 java.base 的特判(返 `boot()`,Module.java:239);完整 `Configuration`/`modules()` 顺延。
///
/// **前置**:注册表须已闭包预载 `java/lang/ModuleLayer`、`java/lang/System`、
/// `jdk/internal/misc/VM`。Phase 1(`initialize_system_class`)须已跑(`initLevel` 单调 1→2)。
pub fn bootstrap_module_system(vm: &mut Vm) -> Result<(), VmError> {
    use crate::metadata::descriptor::FieldType;
    use crate::runtime::Slot;

    // 1) 分配真 java/lang/ModuleLayer Instance(boot layer 单例对象)。
    //    &'a 引用(reg/ml_lc)不绑 &self(§6)→ 出块即释放,vm.heap_mut() 可独占。
    let layer_ref = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("Phase 2 引导需要类注册表"))?;
        let ml_lc = reg
            .get("java/lang/ModuleLayer")
            .ok_or(VmError::BadConstant("Phase 2 须预载 java/lang/ModuleLayer"))?;
        vm.heap_mut()
            .alloc(Oop::Instance(reg.new_instance(ml_lc)))
    };

    // 2) System.bootLayer = layer(对应 `bootLayer = ModuleBootstrap.boot();`)。沿超类链
    //    解析(声明类,序号)——bootLayer 声明于 System 本身;经 Mutex 写其 static_storage。
    let ft = FieldType::Class("java/lang/ModuleLayer".to_string());
    // `reg`(owned Arc,B.3.0)须留域内:`sys_lc: &LoadedClass` 借之,下句 static_storage 用之。
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("Phase 2 引导需要类注册表"))?;
    let (sys_lc, boot_layer_ord) = reg
        .resolve_static_field("java/lang/System", "bootLayer", &ft)
        .ok_or(VmError::BadConstant("Phase 2:System.bootLayer 静态字段未找到"))?;
    sys_lc.static_storage.lock().unwrap()[boot_layer_ord] = Slot::Reference(layer_ref);

    // 3) invokestatic VM.initLevel(I)V —— 置 2(MODULE_SYSTEM_INITED)。Phase 1 已置 1,
    //    单调上行校验(1 < 2 ≤ SYSTEM_SHUTDOWN)通过。
    invoke_static_void(vm, "jdk/internal/misc/VM", "initLevel", |frame| {
        frame.locals.set_int(0, 2)
    })?;

    Ok(())
}

/// 在 `vm` 上解释执行一个**单参静态方法**(用 `setup` 把唯一形参写入 `frame.locals[0]`)。
/// 供 Phase 1 的 `saveProperties(Map)`/`initLevel(I)` 调用——复用解释器执行真字节码,而非旁路 native。
/// 返回类型须为 void(忽略返回值)。
///
/// **借用**(`Vm::registry` 返 `&'a ClassRegistry`,`'a` 不绑定本次 `&self`,CLAUDE.md §6):
/// 故取出 `&'a LoadedClass`/`&'a ConstantPool`/`&'a [u8]` 后仍可再 `&mut vm` 跑 `interpret_with`。
fn invoke_static_void<F>(vm: &mut Vm, class: &str, name: &str, setup: F) -> Result<(), VmError>
where
    F: FnOnce(&mut Frame) -> Result<(), crate::runtime::FrameError>,
{
    let reg = vm
        .registry()
        .ok_or(VmError::BadConstant("Phase 1 引导需要类注册表"))?;
    let lc = reg
        .get(class)
        .ok_or(VmError::BadConstant("Phase 1 引导:目标类未预载"))?;
    let m = find_static_method(lc, name)
        .ok_or(VmError::BadConstant("Phase 1 引导:目标方法未找到"))?;
    let code = m
        .code
        .as_ref()
        .ok_or(VmError::BadConstant("Phase 1 引导:目标方法须为真字节码"))?;
    // &'a 引用(CP/字节码/异常表)与注册表同寿命;setup 写独立 frame,interpret_with 借 &mut vm。
    let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
        .with_exception_table(&code.exception_table);
    let mut frame = Frame::new(code.max_locals, code.max_stack);
    setup(&mut frame)?;
    match interp.interpret_with(&mut frame, vm)? {
        crate::runtime::Value::Void => Ok(()),
        _ => Err(VmError::BadConstant("Phase 1 引导:目标方法期望 void 返回")),
    }
}

/// 按名查首个同名静态方法(忽略描述符——Phase 1 的 saveProperties/initLevel 无重载)。
fn find_static_method<'a>(lc: &'a LoadedClass, name: &str) -> Option<&'a MethodInfo> {
    for m in &lc.cf.methods {
        let Ok(ConstantPoolEntry::Utf8(n)) = lc.cf.constant_pool.get(m.name_index) else {
            continue;
        };
        if n.as_str() == name {
            return Some(m);
        }
    }
    None
}
