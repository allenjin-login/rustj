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
use crate::runtime::{Frame, Interpreter, Vm, VmError};

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
pub fn initialize_system_class(vm: &mut Vm<'_>) -> Result<(), VmError> {
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

    Ok(())
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
pub fn bootstrap_module_system(vm: &mut Vm<'_>) -> Result<(), VmError> {
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
    //    解析(声明类,序号)——bootLayer 声明于 System 本身;经 RefCell 写其 static_storage。
    let ft = FieldType::Class("java/lang/ModuleLayer".to_string());
    let (sys_lc, boot_layer_ord) = {
        let reg = vm
            .registry()
            .ok_or(VmError::BadConstant("Phase 2 引导需要类注册表"))?;
        reg.resolve_static_field("java/lang/System", "bootLayer", &ft)
            .ok_or(VmError::BadConstant("Phase 2:System.bootLayer 静态字段未找到"))?
    };
    sys_lc.static_storage.borrow_mut()[boot_layer_ord] = Slot::Reference(layer_ref);

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
fn invoke_static_void<F>(vm: &mut Vm<'_>, class: &str, name: &str, setup: F) -> Result<(), VmError>
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
