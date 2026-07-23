# HotSpot C1 + C2 — Faithful Port Research

**Wayfinder ticket:** [#6 — T5 忠实 C1+C2 移植](https://github.com/allenjin-login/rustj/issues/6)
**Date:** 2026-07-23
**Provenance:** Produced by a `/research` subagent (read-only) reading
`E:\rustj\jdk-master\src\hotspot\{c1,opto,compiler,code,asm,runtime}` + `cpu/x86`.
Citations are as-reported by the agent; **not independently re-verified line-by-line** — treat as a
decision input. Source paths are relative to `src/hotspot/`.

**Headline numbers (measured, not estimated):**

| Component | LOC (cpp+hpp) | Files |
|---|---|---|
| C1 `share/c1/` | **40,355** | 44 |
| C2 `share/opto/` | **188,792** | 160 |
| x86 `.ad` machine description | **25,853** (single file) | 1 |
| x86 MacroAssembler `cpu/x86/macroAssembler_x86.cpp` | **10,115** | 1 |
| Shared compiler infra `share/compiler/` | ~40 files (compileBroker, compilationPolicy, compileTask, directives, oopMap…) | — |
| `code/` (CodeCache, nmethod, debugInfo, reloc, oopMap, vtableStubs, compiledIC) | ~46 files | — |
| `share/asm/assembler.*` (abstract Assembler base) | 762 | 3 |
| `share/runtime/deoptimization.cpp` | 2,576 | — |
| `share/runtime/sharedRuntime.cpp` | 3,463 | — |

C2 is ~4.7× the size of C1; the single x86 `.ad` is ~64% the size of all of C1.

---

## (a) Component Map

### C1 — `share/c1/` (the "client" / tier-1–3 compiler)

| Subsystem | Anchor | Role | LOC |
|---|---|---|---|
| HIR Instruction hierarchy | `c1/c1_Instruction.hpp:274` (`Instruction` base), `:109` (`Value` typedef) | All SSA value nodes: `Constant:707`, `Local:680`, `AccessField:756`/`LoadField:810`/`StoreField:825`, array `LoadIndexed:928`/`StoreIndexed:953`/`ArrayLength:871`, arithmetic `Op2:1005`/`ArithmeticOp:1039`/`LogicOp:1065`/`ShiftOp:1055`/`Convert:1116`/`IfOp:1088`, control `BlockBegin:1575`/`BlockEnd:1782`/`Goto:1824`/`If:1935`/`Switch:1994`, `Return:2052`, `Throw:2074`, `Base:2093` (method entry), `Invoke:1218`, `Intrinsic:1505`, alloc `NewInstance:1263`/`NewArray:1313`, type checks `CheckCast:1419`/`InstanceOf:1446`, monitors `MonitorEnter:1480`/`MonitorExit:1494`, `Phi:624`, `OsrEntry:2111`, `ExceptionObject:2126` | ~2,404 |
| IR container / scopes / handlers | `c1/c1_IR.hpp:298` (`IR`), `:135` (`IRScope` — supports inlining), `:35`/`:109` (`XHandler`), `:257` (`CodeEmitInfo` = debug info for deopt) | Per-method HIR tree + exception-handler edges + per-bci debug state | ~368 |
| Bytecode → HIR | `c1/c1_GraphBuilder.cpp:50` (`BlockListBuilder` first pass) | Builds blocks, identifies leaders/handlers/loops, then walks bytecode emitting `Instruction`s; inlining + phi insertion | 4,532 |
| HIR opts | `c1_Canonicalizer.cpp:38` (const fold + identities), `c1_Optimizer.cpp:39` (`CE_Eliminator` → `IfOp`), `c1_ValueMap.cpp:46` (GVN/CSE hash), `c1_RangeCheckElimination.cpp:44` | C1's modest optimizer — fold, CSE, conditional-expr, array-bounds elimination | ~3,060 combined |
| LIR op hierarchy + operand model | `c1/c1_LIR.hpp:198` (`LIR_Opr` — compact operand: register/stack/address/constant encoded in an `intptr_t`), `:509` (`LIR_Address` base+index*scale+disp), `:1045` (`LIR_Op` base), `:1308`/`:1327`/`:1551`/`:1739`/`:1761` (Op0/1/2/3/4), `:1173` (`LIR_OpJavaCall`), `:1398` (`LIR_OpRTCall`), `:1234` (`LIR_OpArrayCopy`), `:1829` (`LIR_OpLock`), `:1500` (`LIR_OpTypeCheck`), `:2027` (`LIR_List`) | Machine-near IR; operands are virtual regs / spill slots / addresses | ~2,523 (hpp) + 2,080 (cpp) |
| HIR → LIR lowering | `c1/c1_LIRGenerator.cpp:59` (`PhiResolver`), visitor `do_*()` methods | Lowers each HIR instruction to LIR ops, allocates virtual regs, wires calling convention | 3,464 |
| Linear-scan register allocator | `c1/c1_LinearScan.hpp:101` (`LinearScan`), `:71` (`IntervalState`: unhandled/active/inactive/handled), `:81` (`IntervalSpillState`), `c1/c1_LinearScan.cpp:73` (ctor), `:113` (`reg_num`), `:213` (`allocate_spill_slot`) | Classic Poletto linear-scan over live intervals; splits + spill heuristics; the **single largest C1 file** | 6,740 + 960 |
| Frame map / calling convention | `c1/c1_FrameMap.hpp:64` | Layout: `[args][ABI][monitors][spill][reserved-arg-area]`; arg locations; reg counts | — |
| Code stubs (slow paths) | `c1/c1_CodeStubs.hpp:46` (`CodeStub` base: entry+continuation labels, virtual `emit_code`), `:83` `C1SafepointPollStub`, `:132` `RangeCheckStub`, `:186` `DivByZeroStub`, `:170` `PredicateFailedStub`, `:105` `CounterOverflowStub` | Out-of-line runtime-call slow paths emitted after the method body | 549 |
| Codegen (share base) | `c1/c1_LIRAssembler.cpp` (`LIR_Assembler` base: `emit_code`, exception-handler emission, null-check debug info) | Dispatches LIR ops to arch backend | 800 |
| Codegen (x86) | `cpu/x86/c1_LIRAssembler_x86.cpp:66` (const pools), `:200` `as_Address` (LIR_Address → x86 Address), `:720` `reg2reg`, `:766` `reg2stack` | Emits real x86 bytes via the MacroAssembler (`__` = `C1_MacroAssembler`) | ~2,000+ |
| Runtime1 helpers | `c1/c1_Runtime1.hpp:46` (`Runtime1`, static `CodeBlob* _blobs[C1_STUB_COUNT]`) | Generated stubs for: `new_instance/new_type_array/new_object_array/new_multi_array`, `monitorenter/monitorexit`, `throw_*`, `access_field_patching`, `load_appendix`, `deoptimize`, etc. — the C1↔runtime seam | 1,542 |
| Compilation driver | `c1/c1_Compilation.hpp:61` (`Compilation`), `c1/c1_Compilation.cpp:437` (`compile_method`), `:145` `build_hir`, `:257` `emit_lir`, `:471` `compile_java_method`, timers at `:46` | Per-method context; runs the phase sequence | 736 + 354 |
| Entry | `c1/c1_Compiler.cpp:254` `Compiler::compile_method` (RAII `Compilation c(...)` runs the whole pipeline) | Invoked by CompileBroker | 271 |

**C1 actual phase order** (from `c1_Compilation.cpp`): `initialize → build_hir (GraphBuilder → compute_use_counts → compute_predecessors → compute_block_order → eliminate_null_checks → optimize_blocks [GVN/CE] → RangeCheckElimination) → emit_lir (LIRGenerator → LinearScan::do_linear_scan → LIR_Assembler emit) → install_code`.

### C2 — `share/opto/` (the "server" / tier-4 optimizing compiler)

| Subsystem | Anchor | Role | LOC |
|---|---|---|---|
| Node core (the "sea of nodes") | `opto/node.hpp:248` (`Node`), `:318-335` (in/out edge arrays), `:349-361` (dense global `_idx`) | Universal IR: every value/op is a Node; edges = inputs/uses; arena-allocated | 2,339 + 3,215 |
| Opcode enum | `opto/opcodes.hpp:31` (`enum Opcodes`, ~500), `:49` includes `classes.hpp` (macro-defines every node class) | ~500 opcodes incl. machine-leaf `RegI/RegP/RegF/RegD/RegL/VecX/VecY/VecZ/RegFlags` | ~600 |
| Type lattice | `opto/type.hpp:86` (`Type`), `type.cpp` | Every Node carries an immutable hash-consed Type; meet/join dataflow | 2,578 + 6,602 |
| Constant / arithmetic / mem / control / call nodes | `connode.hpp:37`, `addnode.hpp:43`, `subnode.hpp:34`, `mulnode.hpp:36`, `divnode.hpp:38`, `memnode.hpp:42` (`MemNode`, alias/slice type system, 6,174 LOC), `cfgnode.hpp:64` (`Region/If/Proj/Catch/Phi`), `callnode.hpp:59` (`StartNode:59`, `CallJava/Static/Dynamic/Runtime/Leaf`, `SafePointNode`, `AllocateNode/AllocateArrayNode`, `Lock/Unlock/FastLock/FastUnlock`), `castnode`, `convertnode`, `movenode`, `intrinsicnode`, `opaquenode`, `rootnode`, `subtypenode` | The ideal-node taxonomy | many |
| Loops | `opto/loopnode.hpp:62` (`LoopNode:RegionNode`, flags Normal/Pre/Main/Post/InnerLoop/Vectorized/StripMined), `CountedLoopNode`; transforms in `loopopts.cpp` (4,668) + `loopTransform.cpp` (4,217) + `loopPredicate.cpp` + `loopUnswitch.cpp` | Counted-loop formation, unrolling, unswitching, predication | 7,550 + transforms |
| Vectors | `opto/vectornode.hpp:37`, `superword.hpp:31` (Larsen-Amarasinghe SLP), `superword.cpp` (3,198), `vectorIntrinsics.cpp` (3,264) | Auto-vectorization + Vector API | ~9,400 |
| Parsing / graph build | `opto/parse1.cpp:46` (`Parse`), `parse.hpp:42` (`InlineTree`), `graphKit.hpp:51` (`GraphKit : Phase`, `_map`=`SafePointNode`), `parse2.cpp`/`parse3.cpp`/`parseHelper.cpp`, `library_call.cpp` (intrinsics, **largest file** 9,346) | Bytecode → Node graph; inline decisions; ~all JDK intrinsics | 9,346 (lib) + parse |
| Optimizations — IterGVN | `opto/phase.hpp:44` (`enum PhaseNumber` — ALL phases), `phaseX.hpp:55` (`NodeHash`), `phaseX.cpp` (IterGVN — the workhorse applying `Node::Ideal()/Identity()` to fixpoint) | CSE/canonicalization engine driven off a worklist | 3,606 |
| Optimizations — escape analysis | `opto/escape.hpp:34` (Choi99 connection graph: LV/JO/OF, `-P>/-D>/-F>` edges), `escape.cpp` (5,292) | Scalar replacement + lock elision — **one of the hardest parts** | 5,292 |
| Optimizations — string | `opto/stringopts.cpp` | StringBuilder concat folding | — |
| Matcher (ideal → machine) | `opto/matcher.hpp:43` (`Matcher : PhaseTransform`, state enum Pre_Visit/Visit/Post_Visit), `:94` (`ReduceInst/ReduceInst_Chain_Rule/ReduceOper`), `matcher.cpp` (2,964) | DAG-tiling lowers `Node`s → `MachNode`s using ADLC-generated rules | 2,964 |
| Machine description (ADLC) | `cpu/x86/x86.ad` (**25,853 LOC**): `:70` `reg_def` for RAX…RDI w/ save-policy NS/SOC/SOE/AS; reg classes; `encclass` encoding rules; `instruct` match patterns (`match(AddI …)`, `enc(odds …)`) | DSL compiled by the **ADLC** tool into generated C++ (match switches, MachOper subclasses, reg masks, encodings, pipeline). C2's portability layer — and its biggest lock-in | 25,853 |
| MachNode | `opto/machnode.hpp:59` (`MachOper`), `MachNode` subclasses (MachIf/Goto/Call/Return…) | Post-match machine-level nodes that emit bytes | — |
| Regalloc (Chaitin) | `opto/chaitin.hpp:47` (`LRG` live-range graph, `_cost/_area`), `chaitin.cpp` (2,720), `ifg.cpp` (interference graph), `coalesce.cpp`, `live.cpp`, `reg_split.cpp`, `postaloc.cpp` | Graph-coloring with splitting + coalescing — **far harder than C1's linear scan** | ~6,000+ |
| CFG / GCM / scheduling | `opto/block.hpp`, `gcm.cpp:47` (`schedule_node_into_block`), `:73` (`replace_block_proj_ctrl`), `lcm.cpp` (local code motion), `output.hpp:57` (`PhaseOutput`) | Global code motion pins floating nodes to blocks; latency-aware | 2,587 + lcm |
| Output / emit | `opto/output.cpp` (3,405): MachNode→bytes, `_handler_table`, oop maps, debug info, exception tables, safepoint polls, final nmethod assembly | The back-end emit phase | 3,405 |
| Compile driver | `opto/compile.hpp`, `compile.cpp` (5,489 — the hub), OSR when `entry_bci != InvocationEntryBci`, macro-expand (`macro.cpp` 2,851) | Per-method context holding the graph + running ~15 phases | 5,489 |
| Opto runtime | `opto/runtime.hpp:35` (`OptoRuntime`), `runtime.cpp` (2,429) | Runtime stubs generated from ideal graphs (uncommon trap, deopt, helpers) | 2,429 |

**C2 actual phase order** (per `compile.cpp`): Parse → IterGVN → incremental inlining → IterGVN → escape analysis → IterGVN → loop opts → IterGVN → macro expand → IterGVN → **Matcher** → PhaseCFG → **GCM** → **regalloc** → **Output**.

### Shared compiler infrastructure (`share/compiler/`, `share/code/`, `share/asm/`)

| Subsystem | Anchor | Role |
|---|---|---|
| AbstractCompiler / tiers | `compiler/abstractCompiler.*`, `compiler/compilerDefinitions.*` | `compiler_c1`/`compiler_c2` enums; tier config |
| CompileBroker | `compiler/compileBroker.cpp` | Async compile queue + `CompilerThread`s; the dispatch hub that calls `C1/C2Compiler::compile_method` |
| CompilationPolicy (tiered) | `compiler/compilationPolicy.hpp:106-160` | **5 levels**: 0=interpreter (MethodData profiling), 1=C1 no-profiling, 2=C1+counters, 3=C1+full profiling, 4=C2. Transition predicates (invocation/backedge counters, queue-length feedback scaling `s`). OSR via `b > TierXBackEdgeThreshold`. |
| CompileTask / compilerThread | `compiler/compileTask.*`, `compiler/compilerThread.*` | A queued compile job + the threads that drain it |
| CodeCache / nmethod | `code/codeCache.*`, `code/nmethod.hpp:56` (`ExceptionCache`), `code/nmethod.*` | The code heap holding compiled methods; lifecycle not-entrant/zombie |
| CompiledIC / vtableStubs | `code/compiledIC.*`, `code/vtableStubs.*` | **Inline caches** (call-site → target patching) and virtual-call stubs |
| Debug info / deopt support | `code/debugInfo.*`, `code/debugInfoRec.*`, `code/scopeDesc.*`, `code/location.*`, `code/pcDesc.*` | Per-PC maps to reconstruct interpreter frames on deopt |
| Relocations / patching | `code/relocInfo.*`, `code/nativeInst.*` | Patching compiled code (IC, static-call, oop refs) |
| oop maps | `compiler/oopMap.*` | GC maps over compiled frames |
| Abstract Assembler | `share/asm/assembler.hpp` (514), `assembler.cpp` (248) | `AbstractAssembler`: code buffer, label/reloc machinery |
| MacroAssembler | `cpu/x86/macroAssembler_x86.cpp` (**10,115**) | The high-level x86 emitter used by C1/C2/stubs/runtime (prologue/epilogue, null checks, IC, barriers, locking) |
| SharedRuntime | `runtime/sharedRuntime.cpp` (3,463): `:3247` `OSR_migration_begin` / `:3326` `OSR_migration_end` (the OSR interpreter↔compiled handoff at `:3233`) | Runtime entry points for resolve, IC miss, OSR, helper calls |
| Deoptimization | `runtime/deoptimization.hpp:43` (`DeoptimizationScope`), `:76` `DeoptReason` enum (null_check/range_check/class_check/div0/unstable_if/…), `:127` `DeoptAction`; `deoptimization.cpp:283` `fetch_unroll_info`, `:470` helper, `:852` `unpack_frames` | **The uncommon-trap → interpreter pipeline**: compiled frame → UnrollBlock → reconstruct interpreter frames from debug info (`ScopeDesc`/`DebugInfoRec`) |

---

## (b) Port Sequence

**Verdict: C1 before C2. x86-first. IR before codegen. The macro-assembler + linear-scan + Runtime1 come together as the first end-to-end C1.**

Rationale grounded in source:

1. **C1 is the necessary deopt/safepoint/debug-info substrate for C2.** C2's uncommon-trap→deopt (`deoptimization.cpp:283/852`) and the inline-cache (`code/compiledIC.*`) / vtable-stub (`code/vtableStubs.*`) / nmethod (`code/nmethod.*`) / CodeCache (`code/codeCache.*`) machinery is **shared** — C2 cannot land before that seam exists, and C1 is the natural driver to build it because C1's pipeline is linear and far smaller (40K vs 189K).
2. **C1's phase order is the dependency order**: parse→HIR (block) → HIR-opt → LIR-gen (lowering) → linear-scan (regalloc) → LIR-assembler (emit). Each layer is independently testable. C2 by contrast funnels everything through one Node graph + ~15 phases + the Matcher, with no clean intermediate checkpoint until the whole thing emits.
3. **x86-first is forced.** C1's `LIRAssembler` and C2's `.ad`/Matcher are arch-coupled; `cpu/x86/` is the only target with a complete, canonical implementation (`macroAssembler_x86.cpp` 10K, `x86.ad` 26K, `c1_LIRAssembler_x86.cpp`, `matcher_x86.cpp`, `vmreg_x86.hpp`, `frame_x86.*`). aarch64 is second-class for a faithful port. Targeting Windows x86-64 specifically (the project host) since the Java→native ABI there is unusual but HotSpot has `os_windows.*`/`cpu/x86/assembler_x86.cpp` to port from.
4. **Where the macro-assembler / machine-description layer fits:** it sits **below** both compilers and **above** raw byte emission. `MacroAssembler` (`cpu/x86/macroAssembler_x86.cpp`) is used by C1 (`c1_MacroAssembler.hpp` wraps it), C2 (`.ad` `enc` rules call it), and the runtime stubs (SharedRuntime, Runtime1, OptoRuntime blobs). It is the single shared arch layer — build it once, both compilers consume it. C2 additionally needs the **ADLC toolchain** (a separate build tool that compiles `.ad` → generated C++); that is a prerequisite unique to C2 and a reason to defer C2.

**Recommended order:**
(0) Shared seam first: `code/` (CodeCache, nmethod, relocInfo, compiledIC, vtableStubs, debugInfo/scopeDesc/pcDesc, oopMap, vmreg, location) + `compiler/` (abstractCompiler, compileBroker, compileTask, compilerThread, compilationPolicy) + the `MacroAssembler`/`Assembler`/`CodeBuffer`/`Label` layer. **All designated-unsafe.**
(1) **C1**: GraphBuilder → HIR → Canonicalizer/Optimizer/ValueMap/RangeCheckElimination → LIRGenerator → LinearScan → LIRAssembler_x86 → Runtime1 stubs. End-to-end compile+run of a trivial method = first verifiable increment.
(2) **Deopt path** (uncommon-trap stub + `fetch_unroll_info`/`unpack_frames` equivalent reconstructing the tree-walking interpreter frame from debug info). This is what makes tiered compilation safe — required before C2 leans on speculative optimizations.
(3) **C2**: Node core + Type + GraphKit/Parse → IterGVN → Matcher+ADLC → PhaseCFG/GCM → Chaitin regalloc → Output. Defer EA, SuperWord, string opts, loop transforms to the end (they're pure optimizations over a working C2).

**IR before codegen: yes for both.** C1 builds HIR then LIR then emits; C2 builds the ideal graph then matches then emits. Emitting code before the IR is stable is impossible (you have no reg assignment, no CFG).

---

## (c) `jit/codegen/` designated-unsafe module structure

Inherently unsafe work (must be `#[allow(unsafe_code)]` at the item level per CLAUDE.md §2; the crate stays `#![deny(unsafe_code)]`):

```
jit/
  mod.rs                      # re-exports, the safe façade
  codegen/
    mod.rs                    # #[allow(unsafe_code)] gate for the whole submodule
    buffer.rs                 # CodeBuffer: Vec<u8> emit buffer + label/reloc fixups
                            #   (patching is safe until you reinterpret the buffer
                            #    as executable — that's the unsafe seam)
    assembler_x86.rs          # AbstractAssembler + MacroAssembler_x86 port
                            #   real x86-64 encoders (REX, ModR/M, SIB, immediate forms)
                            #   every emit_* fn touches raw bytes — #[allow(unsafe_code)]
    macro_assembler_x86.rs    # the 10K-LOC high-level layer (prologue/epilogue, null
                            #   check, IC build, barriers, lock fast-path)
    patching.rs               # relocInfo + NativeCall/NativeJump patching — overwrites
                            #   already-emitted instructions at a live PC -> unsafe
    vmreg.rs / frame_x86.rs   # register numbering, frame layout, ABI offsets
  regalloc/
    linear_scan.rs            # C1's allocator (algorithmically safe; calls into
                            #   codegen for spill-slot offsets)
    chaitin.rs                # C2's graph-coloring (deferred)
  (c1/)  HIR, LIR, generator — safe (pure data transforms, no machine code)
  (c2/)  Node, Type, GraphKit, Matcher, phases — safe
  exec/                       # THE unsafe bridge:
    exec_buffer.rs            # allocate RWX (or W then X) memory, memcpy the CodeBuffer,
                            #   flush i-cache, reinterpret as fn pointer, call it.
                            #   This is the single hardest unsafe seam; Windows uses
                            #   VirtualAlloc(PAGE_EXECUTE_READWRITE) or VirtualProtect.
```

**The one truly irreducibly unsafe operation** is in `exec/exec_buffer.rs`: making an emitted byte buffer executable and calling into it. Everything else (encoding bytes into a `Vec<u8>`, running regalloc) is safe Rust that merely *produces bytes*. So the unsafe surface can be kept tight: a `CompiledMethod::invoke(&self, args) -> Value` that does the W^X dance and an indirect call. All the C1/C2 IR machinery above it stays `#![deny(unsafe_code)]`-clean.

**Patching** (`patching.rs`) is unsafe because it mutates instructions at PCs that other threads may be executing (IC patching, static-call patching after class resolution) — HotSpot synchronizes this with the `ICache` flush + safepoint handshake (`code/nativeInst.*`, `code/relocInfo.*`). For an early port you can sidestep this by making ICs always go through the interpreter fallback stub (no patching) until that's a measured bottleneck.

---

## (d) Smallest first increment

**"Compile-and-run a leaf arithmetic method end-to-end through C1, fall back to the interpreter on anything C1 can't yet handle."**

Concrete shape (mirrors the source faithfully):
- **Method:** `static int add(int a, int b) { return a + b; }` (no allocation, no calls, no exceptions, no nulls).
- **Pipeline slice:** GraphBuilder (bytecode `iload_0, iload_1, iadd, ireturn` → HIR `LoadLocal/LoadLocal/ArithmeticOp(add)/Return`) → skip HIR opts → LIRGenerator (→ `LIR_Op2(lir_add)`) → LinearScan (2 args in arg regs, result in a reg) → `LIRAssembler_x86::emit_op2` (emit `addl`) → prologue/epilogue via MacroAssembler → CodeBuffer → `exec_buffer` W^X → call.
- **Verification:** an integration test that runs `add(2,3)` through the compiled nmethod and asserts `5`, then runs the same method through the tree-walking interpreter and asserts identical results. Then a slightly bigger method (`factorial` loop) to exercise a backedge + branch.
- **The deopt escape hatch is built in from day one** (even if rarely hit at first): every compiled frame must carry a `ScopeDesc`/`pcDesc`-equivalent so an uncommon trap can rebuild the interpreter frame. For this trivial increment the trap target is just "bail to interpreter at method entry." Without this seam you cannot safely turn C1 on for real code.

**Do NOT try** to start with: OSR, exceptions in compiled code, allocation (`new`), virtual/interface calls, synchronization, or any C2. Each of those adds a Runtime1 stub + a code-stub slow path + debug-info complexity. Add them one at a time after the leaf-method path is green, in this order: static call → instance field access → `new`/array alloc → virtual call (IC) → exceptions → monitors → OSR. Each maps to a `Runtime1` blob (`c1_Runtime1.hpp:46`).

---

## (e) Honest work estimate

**A faithful C1+C2 port is the largest single subsystem in the project, plausibly the majority of all remaining work.** HotSpot's compilers are ~230K LOC of C++ (40K C1 + 189K C2) plus ~26K LOC of `.ad` per architecture plus ~10K LOC of x86 MacroAssembler. This is not a "port a module" task; it is "build a production optimizing compiler." The reference implementations (V8 TurboFan, GraalVM, Azul) each took teams years.

**Complexity ranking, most-coupled-first:**

1. **Object-model impedance mismatch (THE architectural blocker, not in HotSpot at all).** rustj's `InstanceOop` is `Vec<Slot>` where `Slot` is a tagged enum (`slot.rs:8`: `Int/Float/Long/Double/Reference/ReturnAddress/Top`), accessed by `field(ordinal) -> Slot` returning a **copy** (`instance.rs:25`); the heap is `Vec<Oop>` indexed by a `u32` handle (`heap.rs:10`). **There are no byte offsets, no contiguous in-memory objects, no stable field addresses.** HotSpot compiled code does `mov rax, [obj + offset]` where `offset` is computed at class-link time from `InstanceKlass` field layout (`oops/instanceKlass`). For rustj to have a real JIT, **compiled field access cannot call back into Rust's borrow-checked `heap.get_mut()`** — that defeats the purpose (you'd be doing a `Vec` index + enum match + bounds check + handle indirection per field access, slower than the interpreter). Options, in increasing fidelity and difficulty:
   - **(A) Conservative: JIT only scalar leaf methods + arithmetic** (no field/array access). Useless for Minecraft.
   - **(B) Hybrid: compiled code treats `Reference` as an opaque 32-bit handle and emits calls into Rust runtime helpers for every field get/put.** This is basically the interpreter with extra steps; maybe 2-3× interpreter speed at best.
   - **(C) Faithful: introduce a contiguous, byte-offset-addressable native object layout** parallel to the current handle model — i.e. port `instanceOopDesc`/`arrayOopDesc` + `InstanceKlass::nonstatic_field_layout`. Field access becomes a real load at a computed offset. This means **reworking the object model and the heap**, touching the whole interpreter. This is the prerequisite that makes C1/C2 actually pay off, and it's a large project in itself before any compiler code is written. It also collides with `#![deny(unsafe_code)]`: native-layout oops are inherently pointer-y; field writes from compiled code are `unsafe` by nature (the whole point is the JIT bypasses Rust's borrow checker). The designated-unsafe `exec/` + `codegen/` modules handle this, but the *object representation* boundary needs a deliberate decision.
   - **This is the single most important finding for ticket #6: the JIT cannot be a drop-in over the current handle+Vec heap. Ticket #6 must be preceded by (or co-designed with) a "native object layout" decision.**

2. **ABI / architecture coupling (irreducible).** Calling convention (which arg regs, spill regs, return regs, shadow space on Windows x64), stack frame layout, register numbering (`vmreg`), prologue/epilogue, leaf-vs-non-leaf. Windows x64 specifically differs from SysV (shadow space, caller-callee-saved split, exception/unwind info). HotSpot has all this in `cpu/x86/` + `os_windows*`; it's portable but voluminous and unforgiving — a one-byte encoding error silently corrupts the stack.

3. **ADLC toolchain for C2 (build-system lock-in).** `x86.ad` (25,853 LOC) is a DSL. HotSpot compiles it with a separate `adlc` tool into generated C++ that the Matcher links against. Faithfully porting C2 means either (a) porting ADLC and the `.ad` language and re-using `x86.ad` verbatim, or (b) hand-translating `x86.ad` into a Rust matcher. Either is a multi-thousand-line sub-project and a hard prerequisite for C2. **This is a strong argument for shipping C1 to production usefulness first and deferring C2 indefinitely** — many real workloads (Minecraft modded server logic) are dominated by allocation, dispatch, and I/O where C1 + good GC already gets you most of the way.

4. **Deoptimization (cross-cuts everything).** `deoptimization.cpp` (2,576 LOC) reconstructs interpreter frames from compiled debug info. The tree-walking interpreter's frame model (slots, operand stack, locals) must be expressible as `ScopeValue`s/`ScopeDesc`s so compiled→interpreted transitions work. This is moderately hard but well-defined; it's the price of tiered compilation and must be done early (part of increment #1's escape hatch).

5. **Tiered CompilationPolicy + CompileBroker (moderate).** `compilationPolicy.hpp:106` — the 5-level policy, queue-length feedback, OSR backedge thresholds. Algorithmically clear; needs MethodData (profiling) which is a separate medium subsystem. Can be stubbed to "always compile at C1 level after N invocations" initially.

6. **Escape analysis, SuperWord, loop transforms, Chaitin regalloc, GCM (hard, but pure optimization — deferrable).** These are what make C2 *fast* but a C2 that skips them still produces correct code (it's just C1-tier quality). They can be layered after a correct-if-slow C2 exists. Realistic assessment: **these four together are a multi-engineer-year effort to port faithfully.**

**Bottom-line estimate (order-of-magnitude, faithful port, single developer, x86-64 first):**
- Shared seam (code/ + compiler/ + MacroAssembler + W^X exec): **months** — and it forces the object-layout decision above.
- **Native object layout rework (prerequisite, not technically "compiler"):** months, touches the interpreter broadly.
- **C1 end-to-end (leaf methods → calls → alloc → IC → exceptions → monitors → OSR):** a large fraction of a year to a year.
- **C2 correct-but-slow (Node + Type + Parse + IterGVN + Matcher+ADLC + CFG + Chaitin + Output, no EA/SuperWord/loop opts):** roughly a year, dominated by ADLC + Matcher + regalloc.
- **C2 full optimizations (EA, SuperWord, loop transforms, string opts, intrinsics):** additional large multi-quarter effort, likely >1 person-year.

**Recommendation for the wayfinder:** Sequence as **#6a Native object layout decision** → **#6b JIT shared seam (unsafe exec/codegen + code/ + MacroAssembler)** → **#6c C1** → **#6d Deopt + tiered policy** → (defer) **#6e C2 correct** → (indefinitely defer / scope-cut) **#6f C2 optimizations**. Treat C1-to-production as the realistic milestone that unlocks "runs Forge modded Minecraft tolerably"; treat faithful C2 as a stretch that may never be worth its cost versus shipping C1 + a good GC + interpreter improvements. Be explicit with stakeholders that "faithful C1+C2" is the largest line item in the entire roadmap and the object-model prerequisite (#6a) is non-negotiable and itself disruptive.

---

## Key file:line anchors (quick index)

- **C1 entry:** `share/c1/c1_Compiler.cpp:254`; driver phases `share/c1/c1_Compilation.cpp:437/145/257/471`; HIR `c1/c1_Instruction.hpp:274`; LIR `c1/c1_LIR.hpp:198/1045`; linear scan `c1/c1_LinearScan.hpp:101` + `.cpp:73/113/213`; x86 codegen `cpu/x86/c1_LIRAssembler_x86.cpp:66/200/720`; Runtime1 `c1/c1_Runtime1.hpp:46`.
- **C2 entry:** `share/opto/c2compiler.cpp:107`; hub `opto/compile.cpp` (5,489); Node `opto/node.hpp:248`; Type `opto/type.hpp:86`; opcodes `opto/opcodes.hpp:31`; Matcher `opto/matcher.hpp:43`/`.cpp` (2,964); x86 desc `cpu/x86/x86.ad` (25,853); EA `opto/escape.hpp:34`/`.cpp` (5,292); Chaitin `opto/chaitin.hpp:47`; GCM `opto/gcm.cpp:47`; Output `opto/output.cpp` (3,405).
- **Shared:** tiers `compiler/compilationPolicy.hpp:106-160`; broker `compiler/compileBroker.cpp`; nmethod `code/nmethod.hpp:56`; inline cache `code/compiledIC.*`; deopt reasons `runtime/deoptimization.hpp:76/127`; deopt pipeline `runtime/deoptimization.cpp:283/470/852`; OSR handoff `runtime/sharedRuntime.cpp:3233/3247/3326`; MacroAssembler `cpu/x86/macroAssembler_x86.cpp` (10,115).
- **rustj object model (the blocker):** `src/oops/instance.rs:10/25`, `src/oops/oop.rs:15`, `src/runtime/slot.rs:8`, `src/runtime/heap.rs:10`.
