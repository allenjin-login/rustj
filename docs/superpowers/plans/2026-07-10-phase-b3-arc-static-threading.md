# Phase B.3 真多线程(Arc<'static>)实现计划

> **For agentic workers:** 本计划在 `/goal` 自治循环下逐层 TDD 执行(§7:逐层自动提交 master,
> 不暂停问确认)。各层 RED→GREEN→闸门→commit。Checkbox 跟踪 `- [ ]`。

**Goal:** 让 rustj 支持真 OS 多线程(`Thread.start0` 真起线程 + 阻塞管程 + wait/notify),
端到端跑 Java 多线程程序(两线程 synchronized 自增共享计数器得正确总数)。

**Architecture(2026-07-10 用户决策:Arc<'static> 移除 'a):**
原 spec(`2026-07-09-vm-decomposition-and-threading-design.md`)选 scoped threads,但
`std::thread::Scope` 句柄的 `'scope` 寿命短于 `Vm<'a>` 的 `'a`,无法干净存进 `VmShared<'a>`,
且 thread-local 无法持非 `'static` 引用 —— 在 `#![deny(unsafe_code)]` 下无安全解。故改方案:
**把 `registry` 从 `&'a ClassRegistry` 改为 `Arc<ClassRegistry>`(owned),移除 `Vm<'a>` 的
`'a`**。于是 `VmShared: 'static`,`Arc<VmShared>: 'static` → 普通 `thread::spawn(move || …)`
跨线程共享 `Arc::clone` 的 `VmShared`,无需 scope、无 lifetime plumbing、支持 detach/daemon。
§6 NLL trick 由「`registry()` 返 owned `Arc<ClassRegistry>`(独立 local 绑定,不借 `&self`)」
保留 —— 取出 `&LoadedClass`(借 `Arc`)后仍可 `&mut vm`。

**Tech Stack:** std::sync::{Arc, Mutex, Condvar}(`#![deny(unsafe_code)]` 下安全);edition 2024;
零依赖。javac 编译真 Java 程序作并发闸门。

---

## 文件结构(改动范围)

- `src/runtime/vm.rs`:`VmShared<'a>`→`VmShared`、`Vm<'a>`→`Vm`、`registry:Option<Arc<ClassRegistry>>`、
  `Vm::new(Arc<ClassRegistry>)`、`registry()->Option<Arc<ClassRegistry>>`、`Default for Vm`、
  `sync_assertions` 增 `assert_static` 测试。`impl<'a> Vm<'a>`→`impl Vm`。
- `src/runtime/vm/{mirrors,monitors,threads,exceptions}.rs`:4 个 `impl<'a> Vm<'a>`→`impl Vm`。
- `src/runtime/interpreter/{mod,launch,invoke,field,clinit,string,exception,type_check,array,arraycopy}.rs`:
  所有 `&mut Vm<'_>`/`Vm<'_>` 参数→`&mut Vm`/`Vm`(去 lifetime 注解)。
- `src/runtime/interpreter/native/*.rs`:同上。
- **~70 个测试站点**:`Vm::new(&reg)`→`Vm::new(Arc::new(reg))`(reg 移交进 Arc)。
- `src/runtime/class_loader/loader.rs`(测试):`Vm::new(Arc::new(registry))` 收尾包 Arc。

`ClassRegistry`(src/oops/klass.rs)**不动**:`load_or_replace(&mut self)` 仍 `&mut self` ——
故 registry 须**先 owned 载入完毕,再 `Arc::new(registry)` 包**,后段不能再 `load_closure`。
launch 路径已先 load 后建 Vm,符合。

---

## Task B.3.0: 移除 'a —— registry→Arc<ClassRegistry>(行为保持重构)

**Files:** `src/runtime/vm.rs`、4 子模块、interpreter/* 、native/* 、~70 测试站点。

- [ ] **S1 RED:`assert_static::<Arc<VmShared>>()` 编译失败**

在 `vm.rs` 的 `sync_assertions` mod 加:
```rust
fn assert_static<T: ?Sized + 'static>() {}
/// B.3.0:`Arc<VmShared>` 须 `'static` —— B.3b `thread::spawn(move || …)` 前置。
/// 当前 `VmShared<'a>` 借 `&'a ClassRegistry` → 非 'static → `VmShared` 缺 lifetime 参数 → 编译失败(RED)。
#[test]
fn vmshared_arc_is_static() { assert_static::<std::sync::Arc<VmShared>>(); }
```
Run: `cargo build --tests`(预期 RED:`wrong number of lifetime arguments`)。

- [ ] **S2 GREEN:核心结构去 'a**

`vm.rs`:
- `pub(crate) struct VmShared<'a> {` → `pub(crate) struct VmShared {`
- `registry: Option<&'a ClassRegistry>,` → `registry: Option<Arc<ClassRegistry>>,`
- `impl<'a> VmShared<'a> {` → `impl VmShared {`;`fn new(registry: Option<&'a ClassRegistry>)` → `fn new(registry: Option<Arc<ClassRegistry>>)`
- `pub struct Vm<'a> { shared: Arc<VmShared<'a>>, … }` → `pub struct Vm { shared: Arc<VmShared>, … }`
- `impl<'a> Vm<'a> {` → `impl Vm {`
- `pub fn new(registry: &'a ClassRegistry)` → `pub fn new(registry: Arc<ClassRegistry>)`(体:`VmShared::new(Some(registry))`)
- `pub(crate) fn from_shared(shared: Arc<VmShared<'a>>)` → `Arc<VmShared>`;`shared_arc() -> Arc<VmShared<'a>>` → `Arc<VmShared>`
- `pub fn registry(&self) -> Option<&'a ClassRegistry> { self.shared.registry }` →
  `pub fn registry(&self) -> Option<Arc<ClassRegistry>> { self.shared.registry.clone() }`(owned clone 保 §6 trick)
- `impl Default for Vm<'_>` → `impl Default for Vm`;`VmShared::new(None)` 不变
- 4 子模块 `impl<'a> Vm<'a>` → `impl Vm`(mirrors/monitors/threads/exceptions)
- 移除 `from_shared`/`shared_arc` 上的 `#[allow(dead_code)]`(B.3b 起将真用;若仍 warn 则保留)

- [ ] **S3 GREEN:调用点机械去 lifetime**

- interpreter/* 与 native/* :所有 `&mut Vm<'_>` → `&mut Vm`、`Vm<'_>` → `Vm`、`fn x<'a>(…: &mut Vm<'a>)` → 去 `<'a>`。
  (interpret_with 签名 `&mut Vm<'_>` → `&mut Vm`。launch 三函数 `&mut Vm<'_>` → `&mut Vm`。)
- ~70 测试站点:`Vm::new(&reg)` → `Vm::new(Arc::new(reg))`(用 `use std::sync::Arc` 或 `std::sync::Arc::new(reg)`)。
  若某测试建 Vm 后仍用 `reg`(load_closure 后),改 `let reg = Arc::new(ClassRegistry::new()); <setup via &mut *Arc …>`——
  **核查**:多数站点建 Vm 后不再用 reg,直接 `Arc::new(reg)` 移交即可。compiler 会指认例外。
- sync_assertions 现有 4 测试:`check<'a>(_: &'a ClassRegistry)` 助手签名去 `<'a>`;`from_shared_shares_arc_vmshared`
  的 `Vm::new(&reg)`→`Vm::new(Arc::new(reg))`。
- loader.rs 测试:`Vm::new(Arc::new(registry))`(若该处建 Vm)。

Run: `cargo build --tests` → 编译过;`cargo test --lib` → 全绿(行为保持)。

- [ ] **S4 闸门 + clippy**

Run: `cargo test --lib --test javabase_full_load --test stack_trace --test thread_mirror --test synchronized_block`;`cargo clippy --all-targets -- -D warnings`。
预期:全绿 + clippy 净 + 零 unsafe。

- [ ] **S5 commit + memory**

```
git add -A; git commit 多 -m
feat(interp): Phase B.3.0 移除 Vm<'a> — registry→Arc<ClassRegistry>(VmShared: 'static)
… body:解锁 B.3b thread::spawn …
Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>
```
更新 memory `hotspot-rust-migration-project.md`(B.3.0 完 + commit hash)。

---

## Task B.3a: 真 JavaMonitor + Condvar(阻塞管程)

**Files:** `src/runtime/vm/monitors.rs`、`src/runtime/vm.rs`(`VmShared.monitors` 类型换)、新
`tests/concurrent_monitor.rs` 或 lib 内 `#[test]`(spawn 真 OS 线程)。

**设计(spec §2.3):**
```rust
struct MonitorInner { owner: Option<Reference>, count: u64 }
pub(crate) struct JavaMonitor {
    inner: Mutex<MonitorInner>,
    entry: Condvar,          // monitor_enter 阻塞用:被他人持有时 wait
}
// VmShared.monitors: Mutex<HashMap<Reference, MonitorState>>
//   → Mutex<HashMap<Reference, Arc<JavaMonitor>>>  (惰性 per-object)
```
`monitor_enter`:null→NPE;锁 monitors 表取 `Arc<JavaMonitor>`(无则插新);**释表锁**;
锁 JavaMonitor.inner:owner==本线程→count++;owner==None→置 owner+count=1;owner==他人→
`entry.wait` 循环至 owner==None。`monitor_exit`:null→NPE;取 Arc;锁 inner:owner 不符/0→IMSE;
count-1;归零 owner=None+`entry.notify_one`。`holds_lock`:锁 inner 读 owner==本线程&&count>0。

owner 解析:rustj 每线程一 Vm,`self.thread.thread_ref`(Thread 镜像句柄)即当前线程身份。
(当前 `main_thread()` 用于单线程;B.3a 改用 `self.thread.thread_ref.unwrap_or(main_thread())`
或专门 `current_thread_id()`——见下"线程身份"注。)

- [ ] **S1 RED:Rust 级两线程争用门**

新测试(用 `std::thread`,无需 Java Thread.start0):两 OS 线程各 `Vm::from_shared(Arc::clone)`
派生共享 VmShared 的 Vm,对同一锁对象 `monitor_enter`/`monitor_exit` 包夹自增共享 `AtomicU64`,
各跑 N 次 → 总数 == 2N。当前 monitor_enter 重入不阻塞 → 竞态损坏(总数 ≠ 2N)→ RED。

- [ ] **S2 GREEN:JavaMonitor + Condvar 实装**(如上设计)。

- [ ] **S3 闸门** + 单线程回归(`monitor_tests` 仍绿)+ commit + memory。

---

## Task B.3b: Thread.start0 真起线程 + join

**Files:** `src/runtime/vm/threads.rs`、`src/runtime/interpreter/native/java_lang.rs`(Thread.start0/join0)、
新 javac 闸门。

**设计(B.3.0 后 `Arc<VmShared>: 'static`):**
`start0`(native,在主线程 Vm 上调):取 `Arc::clone(&self.shared)` → `std::thread::spawn(move || {`
`let mut child_vm = Vm::from_shared(shared);`
`child_vm.thread = new ThreadContext(tid=next_thread_tid(), thread_ref=此 Thread 镜像);`
`// 跑 Thread.run():若 target!=null → target.run()`
`interpret Thread.run() on child_vm; })`;`JoinHandle` 存 `ThreadManager` 表(tid→handle+state)。
`join0`:`ThreadManager` 表取 handle→`.join()`(阻塞至子完)。

注:`start0` 跑 `Thread.run()` 须经解释器调 `target.run()`;`target` 是 Thread 实例字段(Runnable)。
Thread 镜像句柄(Reference)在主线程堆上,Arc 共享堆 → 子线程可见。

- [ ] **S1 RED:javac Java 闸门**

```java
class T { static int v; public static void main(String[] a) throws Exception {
  Thread t = new Thread(()-> T.v = 42); t.start(); t.join(); if (T.v!=42) throw new RuntimeException(); }}
```
javac → 跑 → 当前 start0 空操作桩(v 仍 0)→ RED。

- [ ] **S2 GREEN:start0 真起线程 + join0 join**(如上设计)。

- [ ] **S3 闸门** + commit + memory。

---

## Task B.3c: Object.wait/notify/notifyAll(后置)

`JavaMonitor += wait_set + wait_cvar`;Object.wait/notify/notifyAll natives。javac 生产者-消费者闸。
依赖 B.3a/B.3b。本计划暂不展开。

---

## 风险 / 注意

- **MutexGuard Drop 借用(B.2.3b 经验)**:`Arc<JavaMonitor>` 须在释 monitors 表锁后**单独**锁
  (drop-before-recurse);`entry.wait` 持 inner 锁——标准 Condvar 用法,`wait` 释锁阻塞、唤醒重获。
- **死锁**:`monitor_enter` 持 JavaMonitor.inner 等待时已释 monitors 表锁;不同对象不同 JavaMonitor → 无锁序问题。
- **重入 std Mutex 死锁**:解释器递归 `interpret_with` 贯穿同一 `&mut Vm` → 任何同线程重入同一 `Mutex`
  会自死锁。B.2.3b 已确立 drop-before-recurse 纪律;start0 spawn 的**新线程**有独立 `ThreadContext`/`&mut Vm`,
  不与主线程共享 MutexGuard(各自 lock)。但**子线程与主线程争同一对象 JavaMonitor** 正是设计意图(Condvar 串行化)。
- **线程身份**:`monitor_enter` 的 owner 须为**当前 OS 线程**对应 Thread 镜像。每 Vm 一 ThreadContext,
  `self.thread.thread_ref` 即当前线程身份(主线程惰性 `main_thread`)。B.3a 用此判 owner==本线程。
- **不走 scoped threads**:B.3.0 后纯 `thread::spawn` + `Arc::clone`,JoinHandle 显式 join(B.3b)。
  daemon/未 join 线程:进程退出时 Arc<VmShared> drop(Arc 最后一个 clone 释放);非 join 线程被 OS 回收
  ——与 HotSpot daemon 语义近似(顺延精确化)。
