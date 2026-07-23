# Layer 1: 类文件解析 + 常量池 + 元数据 (Rust 迁移设计)

> HotSpot VM → Rust 迁移的第一层。对应源码:`src/hotspot/share/classfile/`、`src/hotspot/share/oops/{constantPool,constMethod,method}.*`、`src/hotspot/share/runtime/*` 中与类文件格式相关的部分。

## 目标

把 HotSpot 中"读取 `.class` → 常量池 + 元数据"这条**纯数据解析**链路迁移到 Rust,作为整个 VM 的地基。

- **零 unsafe**(纯字节解析,全部 safe 切片索引 + `from_be_bytes`)。
- **模块化封装**:每个 JVMS 概念一个模块,边界清晰、可独立测试。
- **API 稳定**:常量池用 owned `String`,将来可无痛替换为符号 intern 而不破坏调用方。

## 非目标(留给后续层)

类引用解析为 InstanceKlass 图、字节码校验、签名/注解深解、符号 intern、GC、解释器。

## 常量池建模:方案 A(标签枚举 + owned 数据)

HotSpot 用平行 `_tags` 数组 + 原始槽(为 metaspace/GC/指针压缩)。Rust 地道做法:

```rust
pub enum ConstantPoolEntry {
    Utf8(String),
    Class { name_index: u16 },
    String { string_index: u16 },
    Fieldref        { class_index: u16, name_and_type_index: u16 },
    Methodref       { class_index: u16, name_and_type_index: u16 },
    InterfaceMethodref { class_index: u16, name_and_type_index: u16 },
    NameAndType     { name_index: u16, descriptor_index: u16 },
    Integer(i32), Long(i64), Float(f32), Double(f64),
    MethodHandle { reference_kind: u8, reference_index: u16 },
    MethodType { descriptor_index: u16 },
    Dynamic { bootstrap_method_attr_index: u16, name_and_type_index: u16 },
    InvokeDynamic { bootstrap_method_attr_index: u16, name_and_type_index: u16 },
    Module { name_index: u16 },
    Package { name_index: u16 },
    Unusable,                  // long/double 占第二槽的占位,保留索引正确性
}
```

`ConstantPool { entries: Vec<ConstantPoolEntry> }`,1-based 索引,`get(index) -> Result<&Entry>`。

## 模块结构

```
src/
  lib.rs                # crate 根,re-export
  main.rs               # demo:读 .class 打印解析结果
  classfile/
    mod.rs  reader.rs  parser.rs  version.rs  attributes.rs  error.rs
  constant_pool/
    mod.rs  tag.rs  entry.rs  pool.rs
  metadata/
    mod.rs  access_flags.rs  descriptor.rs  field.rs  method.rs  class_file.rs
```

### 关键模块

- **`classfile/reader.rs`** — `Reader<'a> { bytes: &'a [u8], pos: usize }`,`u1/u2/u4/utf8() -> Result<_, ClassFileError>`。越界 → `Truncated`。纯 safe。
- **`classfile/error.rs`** — `ClassFileError { BadMagic, Truncated, InvalidTag(u8), BadIndex(u16), Utf8Decode, UnsupportedVersion{major}, InvalidConstantPool }`。库内全程 `?`,不 panic。
- **`constant_pool/pool.rs`** — 解析常量池,处理 long/double 占两槽。
- **`metadata/method.rs`** — `MethodInfo`,深解析 `Code` 属性(`max_stack`/`max_locals`/`code: Vec<u8>`/`exception_table`),其余属性存原始 `(name_index, bytes)`。
- **`metadata/descriptor.rs`** — 解析 `(IJLjava/lang/String;)V` 这类描述符为结构化 `MethodDescriptor`。
- **`classfile/parser.rs`** — 顶层编排,产出 `ClassFile`。

## 解析范围(JVMS §4)

magic(0xCAFEBABE)、major/minor、constant_pool_count、常量池、access_flags、this_class、super_class、interfaces_count/interfaces、fields_count/fields、methods_count/methods、attributes_count/attributes。

## 测试策略

1. 每模块 `#[cfg(test)]` 单元测试。
2. 集成测试 `tests/parse_real_class.rs`:用 `javac` 编译一个最小 Java 类(构建脚本里 `Command::new("javac")` 或预置 fixtures),解析并断言常量池条目数、方法数、某 `Code.max_locals`。
3. 损坏输入测试:坏 magic、截断、坏 tag、坏索引 → 各自错误变体。

## 构建顺序

constant_pool → classfile/reader + error → version/access_flags → descriptor → field → method(+Code) → attributes → parser → class_file → main demo → 集成测试。
