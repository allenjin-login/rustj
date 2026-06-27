# Layer 4.7b — 异常/错误模型统一(plan)

日期:2026-06-27
spec:`docs/superpowers/specs/2026-06-27-exception-unification-design.md`
节奏:每阶段 TDD(先红后绿)→ 集成闸门 → 单独提交。

## 阶段划分(由低风险到高风险,逐阶段提交)

### 阶段 A — 引导异常桩 + 抛出辅助(低风险,基础设施)
1. **红**:在 `oops::bootstrap` 新模块写测试——`synth_classfile("X","Y")` 产出合法
   `ClassFile`(this_class="X"、super_class="Y"、空 fields/methods、CP 可解析)。
2. **绿**:实现 `synth_classfile` + `install_bootstrap`(扁平 `(name, super)` 表,
   §3.1 层次)。`ClassRegistry::new()` 调之。
3. **红**:`is_instance` 标准层次测试——`is_instance("java/lang/NullPointerException",
   "java/lang/Exception")`==true、`("…NPE","java/lang/Throwable")`==true、
   `("…NPE","java/lang/Error")`==false、`("…ArithmeticException","java/lang/RuntimeException")`==true。
4. **绿**:桩装入后既有 `supertypes_of`/`is_instance` 应已自然满足(验证;若既有测试因
   用户异常类现可上行 Throwable 而变,调整断言)。
5. **绿**:`runtime::interpreter` 加 `throw_exception(vm, class_name) -> VmError`
   (取桩 → `new_instance` → alloc → `ThrownException`)。单元测试:抛 NPE 返回
   `ThrownException`,对象类名 == "java/lang/NullPointerException"。
6. 提交:`feat(oops): 引导异常类桩 + throw_exception(单一 Java 异常通道)`。

### 阶段 B — dispatch 抽取 + 同帧捕获(核心,中风险)
7. **红**:`mod.rs` 单元测试——同帧 `getfield` on null → 本帧异常表 `catch(NPE)` 捕获
   跳 handler(手工造字节码 + 异常表)。先看它因循环不咨询表而失败。
8. **绿**:抽 `dispatch(op, frame, vm, &mut pc) -> Result<Step, VmError>`(`Step` 枚举)。
   循环改 §3.3 形态。`athrow` 臂简化(表查找上移到循环)。
9. **绿**:把 7 个运行时变体的抛出点改为 `throw_exception`(field/invoke/array/type_check/
   idiv/rem/stack_depth)。**移除** `VmError` 这 7 变体 + Display 臂。
10. **回填**:既有 7 处 `assert_eq!(.., VmError::NullPointer 等)` 改为匹配
    `ThrownException`(并核对对象类名)。
11. 全 `cargo test` 绿 + clippy。提交:`refactor(interp): dispatch 抽取 + 运行时异常统一为 ThrownException`。

### 阶段 C — javac 集成闸门 + 收尾
12. **红→绿**:`tests/throw_internal.rs`(javac):NPE/ArithmeticException/AIOOBE 同帧与跨帧
    捕获、未捕获传播。`catch(Exception)`/`catch(Throwable)` 现应匹配(桩层次)。
13. 最终闸门:全测试绿、clippy `-D warnings`、零 unsafe。
14. 更新 memory(Layer 4.7b 完成 + 教训)。
15. 提交:`test(interp): Layer 4.7b 运行时异常 javac 集成闸门`。

## 每阶段闸门
- `cargo test --lib`(单元)→ `cargo test`(含集成)→ `cargo clippy --all-targets -- -D warnings`。
- 任一阶段失败不进下一阶段;每阶段独立提交,信息含 `Co-Authored-By: Claude Opus 4.8`。

## 反史山自检(每阶段)
- 无新特殊分支散落(`is_instance` 不加 ad-hoc 特判;统一走桩)。
- 异常分派单点(`find_handler` 同帧/跨帧共用)。
- `VmError` 二分清晰(ThrownException vs 内部故障)。
- 无重复构造三件组(沿用 4.7 的 `with_exception_table` builder)。
