# invokedynamic(makeConcatWithConstants)设计 — Layer 4.10u

## 背景 / 动机

JDK 9+ javac 把**非常量**字符串拼接(`s + s`、`"x" + n`)编译为
`invokedynamic`,引导方法 `java/lang/invoke/StringConcatFactory.makeConcatWithConstants`
(经 `javap -v` 实证:recipe = `""`,两动态实参占位)。无 `invokedynamic` 支持,
rustj 无法运行任何 JDK 9+ 编译、含动态拼接的真实 Java(现仅靠 `-XDstringConcat=inline`
退回 StringBuilder 风格绕过)。本层是运行现代 java.base 的战略解锁。

**真实字节码**(Step 0,`javap -v Indy`,`s+s`):
```
5: invokedynamic #9, 0  // InvokeDynamic #0:makeConcatWithConstants:(QString;QString;)QString;
BootstrapMethods:
  0: #29 REF_invokeStatic java/lang/invoke/StringConcatFactory.makeConcatWithConstants:(...)
    Method arguments:
      #27    // recipe:两占位、无字面量/常量
```
- `CONSTANT_InvokeDynamic{ bootstrap_method_attr_index, name_and_type_index }`
  → name_and_type 给**动态调用点的类型**(`(QString;QString;)QString;` = 实参类型 + 返回 String),
  **非**引导方法的描述符。
- `BootstrapMethods` 属性(类级):`{ u2 num; [ u2 bootstrap_method_ref(CONSTANT_MethodHandle); u2 num_args; [u2 arg]* ] }`。
- 引导方法识别:`bootstrap_method_ref` → `CONSTANT_MethodHandle{reference_kind, reference_index}` →
  `Methodref` → `(类, 名, 描述符)`。识别 `(StringConcatFactory, makeConcatWithConstants)`。

## 设计决策:按名特判引导方法(语义移植)

真实 HotSpot **运行**引导方法(`makeConcatWithConstants` 是 java.base 里的 Java 方法,返回 `CallSite`,
链入调用点)。但该路径深陷 `MethodHandle` 组合器 / `StringConcatFactory` recipe 生成等机制,
**运行它远比特判深**。沿用用户既定的「按语义移植」决策(native 表特判 `JVM_*` 同理),rustj
**按引导方法 (类,名) 特判**,直接综合调用目标:

- `StringConcatFactory.makeConcatWithConstants` → 按 recipe 拼接(本层)。
- 其余引导方法(`LambdaMetafactory.metafactory` 等)→ 未支持,明确诊断(后续层)。

## 实参 / recipe 语义

- 动态实参按**调用点描述符**(`name_and_type.descriptor`)的形参类型逆序弹出 → 翻正序
  (`args[i]` 对应 `param_types[i]`,沿用 `pop_arg`)。
- recipe(`bsm_args[0]` 的 `CONSTANT_String`):
  - `` = 动态实参占位 → 取下一个 `args[i]`,按 `param_types[i]` 字符串化,拼入。
  - 其它字符 = 字面量 → 原样拼入(`"Result: "` 常见)。
  - `` = 常量占位(顺延;少见于简单拼接,本层 best-effort 跳过,记债)。
- 结果 `String` 经 `string::intern` 规范化 → `Value::Reference`,经 `finish_invoke` 回填。

## 字符串化(常见类型精确,float/double 记债)

- `Class`(`String` 引用)→ `read_text`;`null` → `"null"`(Java 语义)。
- `Int`/`Byte`/`Short` → 十进制;`Long` → 十进制;`Char` → 该字符;`Boolean` → `"true"/"false"`。
- `Float`/`Double` → Rust 格式(**非 Java 精确**,如 `1.0` 与 Java 略异;独立债,后续)。

## 改动点

1. `classfile/attributes.rs`:`BootstrapMethodEntry{bootstrap_method_ref:u16, bootstrap_arguments:Vec<u16>}`
   + `parse_bootstrap_methods(&[u8])`(镜像 `parse_line_number_table`)+ 单测。
2. `metadata/class_file.rs`:`ClassFile::bootstrap_methods() -> Vec<BootstrapMethodEntry>`
   (扫 `attributes` 按名 `"BootstrapMethods"` 经 cp 识名,解码;owned,规避借用纠缠)。
3. `runtime/interpreter/mod.rs`:`Interpreter::declaring_class() -> Option<&'a str>`
   (借 `identity.class`;匿名帧 None)。
4. `runtime/interpreter/invoke.rs`:`invoke_dynamic(interp,frame,vm,index,caller_pc)`
   + `resolve_invoke_dynamic` / `resolve_method_handle` / `resolve_recipe` / `concat_with_recipe` /
   `stringify_arg`(私有)。`finish_invoke` 复用(Ok 路径回填 String、Fallthrough)。
5. `runtime/interpreter/mod.rs`:分派臂 `Opcode::Invokedynamic`(`pc += 5`)。

## 闸门

`tests/indy_concat.rs`(javac,**无** `-XDstringConcat=inline`,即真 invokedynamic):
- `selfConcatLength`:`String s="abc"; return (s+s).length();` → 6。
- `mixedConcat`:`return ("n=" + 7).length();` → `"n=7".length()`=3(字面量 + int 占位)。
预载真 String 闭包。任何未支持引导方法 → 明确 `VmError` 诊断。

## 顺延(后续层)

- `LambdaMetafactory.metafactory`(lambda / SAM 实例化)。
- `CONSTANT_Dynamic`(condy)。
- `` 常量占位、Java 精确 float/double 字符串化。
- 运行任意真实引导方法(若真需)。
