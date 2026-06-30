# Layer 4.10n — clinit 失败的 cause 链 + clinit 帧

## 背景 / 触发

调试 `Math.<clinit>` 失败(4.10m)时,顶层仅见

```
java/lang/ExceptionInInitializerError
  at java/util/Arrays.copyOfRange
  at java/lang/String.<init>
  at java/lang/StringBuilder.toString
```

**既无 clinit 帧,也无真实异常类** —— 只好挂临时 `[clinit 诊断]` eprintln。两处缺口:

1. `run_clinit`(clinit.rs:46)构造 `Interpreter` **未调 `.with_identity`** → `interpret_with`
   在 `identity==None` 时**跳过 `push_frame`**(mod.rs:191-193)→ clinit 帧**从不入栈轨迹**。
2. EIIE 包裹(clinit.rs:108)调 `throw_exception(vm, "...ExceptionInInitializerError")`
   —— **不传 cause**,原始异常引用被丢弃 → 无 `Caused by:` 链。

## 真实 JVM 语义

`InstanceKlass::initialize_impl`:`<clinit>` 抛异常 E 时 `new ExceptionInInitializerError(E)`
(JVMS §5.5):
- EIIE 的 `cause` 字段 = E;`e.getCause()` 返 E。
- E **自身**的轨迹(抛出点 `fillInStackTrace` 捕获)携带 clinit 内部位置。
- 顶层打印 EIIE 轨迹后接 `Caused by: <E>\n\tat …`(E 的轨迹)。

## 修复

### A. clinit 帧入轨(`run_clinit` 加 identity)

```rust
let interp = Interpreter::new(&code.code, &lc.cf.constant_pool)
    .with_exception_table(&code.exception_table)
    .with_identity(lc.name(), "<clinit>");   // ← 新增
```
`lc.name()` 借自 `&'a LoadedClass`(registry().get 的 `'a` 寿命,不绑 `&self`)→ 与
`&mut vm` 并存无碍。`<clinit>` 为 `&'static str`。

### B. cause 链(`Vm` 增 `causes` 映射 + `format_trace` 追链)

`traces: HashMap<Reference, Vec<CallFrame>>` 已键控异常→帧。新增并行的

```rust
causes: HashMap<Reference, Reference>,   // 包裹异常 → 被包 cause
```

- `record_cause(&mut self, wrapper, cause)`:登记。
- `format_trace`:渲染完本异常帧后,沿 `causes` 追链,每跳输出
  `\nCaused by: <cause 类>` + cause 自身帧(逆序)。带深度上限(64)防环。

### C. EIIE 包裹挂 cause(clinit.rs)

```rust
} else {
    let err = throw_exception(vm, "java/lang/ExceptionInInitializerError");
    if let VmError::ThrownException(eiie) = err {
        vm.record_cause(eiie, cause);     // ← 新增:EIIE.cause = 原异常
        Err(VmError::ThrownException(eiie))
    } else {
        err
    }
}
```

## 完成判据

- 单测:`Cls.<clinit>`(1/0 → ArithmeticException)→ `ensure_class_initialized` 返 EIIE;
  `format_trace(eiie)` 含 `ExceptionInInitializerError` 头、`Caused by: java/lang/ArithmeticException`、
  `Cls.<clinit>` 帧。
- 既有 clinit / 全量测试不退。
- string_concat 闸门仍绿(无行为回归)。
- (诊断 eprintln 已于 4.10m 移除;不再需要。)
