# Layer 4.10q — 栈轨迹行号(`LineNumberTable` 解码 + 每帧 bci → 行号)

## 背景

异常债之一:`format_trace` 仅出 `at Class.method`,无源文件行号。HotSpot
`StackTraceElement = (declaringClass, methodName, fileName, lineNumber)`,lineNumber
来自 `method->line_number_from_bci(bci)`。当前 `CallFrame` 只有 class+method,
字节码 pc 不入帧,故无 pc↔line 可言。

## 源码依据(Step 0)

- `LineNumberTable`(JVMS §4.7.12):`u2 line_number_table_length` 后接
  `{u2 start_pc; u2 line_number}[]`,内嵌于 `Code` 属性的子属性。
- 行号解析:`Method::line_number_from_bci(bci)` 取 **最大的 `start_pc ≤ bci`** 的
  `line_number`(条目按 start_pc 升序)。HotSpot `classFileParser.cpp:1601/1618` 校验。
- `SourceFile`(§4.7.10):类级属性,体 `u2 sourcefile_index` → 常量池 Utf8(文件名)。
- HotSpot 渲染:`at pkg.Cls.method(File.java:42)`;无表 → "Unknown Source"。

## 设计

### 1. `LineNumberTable` 解码(`attributes.rs` + `method.rs`)

`resolve_code` 处已持 `cp`(按名识属性),故在此解析 Code **内嵌**的 LineNumberTable:

```rust
pub struct LineNumberEntry { pub start_pc: u16, pub line_number: u16 }

// CodeAttribute 增 line_number_table: Vec<LineNumberEntry>(parse_code 初始化空)
// resolve_code 在 parse_code 后扫 code.attributes,按名 "LineNumberTable"(经 cp)
//   调 parse_line_number_table(&info) 填入。
```

`parse_line_number_table(info)`(cp 无关纯解码,保持 attributes.rs 解耦)。

### 2. `SourceFile` 文件名(`class_file.rs`)

懒查不进结构:`ClassFile::source_file_name() -> Option<&str>` 扫 `self.attributes`
按名 "SourceFile"(经 cp),取体 u2 → Utf8。无 → None。

### 3. 每帧记 bci(`vm.rs` + `mod.rs`)

`CallFrame { class, method, pc: u32 }`(`push_frame` 初始化 0)。新增
`Vm::set_top_frame_pc(pc: u32)`(写 `call_stack.last_mut().pc`)。`run()` 在分派前
`vm.set_top_frame_pc(pc as u32)`——记**当前指令起始** bci:

- 抛出帧:抛点即当前指令 bci(分派 step 顶已写)。
- 调用者帧:陷入被调用者后冻结于 invoke 点(其 run loop 挂起前最后写入)。

native 帧(arraycopy 等经 native::invoke 推入)无 run loop → pc 恒 0 → 无行号,
渲染裸 `at Class.method`(可接受,HotSpot 标 Native Method)。

### 4. `format_trace` 行号解析

每帧:`registry.get(class)` → 遍历 `cf.methods` 取名匹配且 `pc < code.code.len()`
者 → 其 `line_number_table` 取最大 `start_pc ≤ pc` 的 `line_number`。命中 + 有
`source_file_name()` → 渲染 `at Class.method(File.java:LINE)`;否则裸
`at Class.method`(保持既有测试 `contains("Cls.<clinit>")` 兼容)。registry 为空 /
类未加载 / 无表 → 优雅降级裸帧。

类名仍用内部名(`java/lang/…`,非点分),与现有测试一致。

## TDD

红先:`stack_trace.rs` 集成闸门编译真 `.java`(已知抛出行)→ 断言 `format_trace`
含 `(File.java:N)`。单测:`parse_line_number_table` 解码;pc→line 取最大 start_pc ≤ pc。

## 顺延

- 重载方法按名+pc 范围匹配;若真二义,再加描述符进 `CallFrame`。
- 真 `Throwable.getStackTrace()` → `StackTraceElement[]`(本层先把 line 解析铺好)。
