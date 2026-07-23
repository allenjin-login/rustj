# Forge / NeoForge Support — Research

**Wayfinder ticket:** [#8 — T7 Forge/NeoForge 支持](https://github.com/allenjin-login/rustj/issues/8)
**Date:** 2026-07-23
**Provenance:** Done inline (the `/research` subagent died on a gateway usage cap). Forge/NeoForge are **not
in `jdk-master`** — findings combine domain knowledge + a targeted web search (NeoForge 1.21.1 crash stack +
SpongePowered Mixin wiki) that confirmed the live class/module names and architecture. Package names verified
against modlauncher-11.0.5 / loader-4.0.42 / sponge-mixin-0.15.2 (NeoForge 1.21.x era). Re-verify against the
specific target NeoForge version when implementation begins.

**Headline:** Forge/NeoForge's JVM requirements are dominated by **one central capability rustj entirely
lacks: a bytecode-instrumentation layer** — a transforming classloader running a transformer chain + a
launch-plugin SPI that operates on ASM `ClassNode` (tree API), plus `java.lang.instrument`. On top of that
sit Mixin (load-time bytecode merging), AccessTransformer, and Forge's reflection/annotation/event-bus
machinery. The good news: rustj **already has the module system (4.11/4.14) and reflection (4.15)** — the
gaps are the instrumentation engine, reflection *completeness* (annotations, `setAccessible` honored,
`privateLookupIn`), and runtime module `addOpens`/`addExports`.

---

## (a) Forge runtime JVM-requirement checklist (confirmed by search)

The modern stack (NeoForge 1.20.2+/Forge 1.13+, package `cpw.mods.modlauncher` + `net.neoforged.fml` + `cpw.mods.securejarhandler`):

| # | Requirement | Evidence (search) | rustj status |
|---|---|---|---|
| 1 | **Transforming classloader**: a classloader whose `defineClass` runs a **transformer chain** over the bytes before defining | `cpw.mods.modlauncher.TransformingClassLoader.maybeTransformClassBytes(:57)` → `ClassTransformer.transform(:120)` | ❌ **none** — rustj's classloader defines bytes as-is |
| 2 | **Launch-plugin SPI** operating on ASM **`ClassNode`** (tree API), with per-class "process with flags" hooks | `cpw.mods.modlauncher.serviceapi.ILaunchPluginService.processClassWithFlags(:156)`; `LaunchPluginHandler.offerClassNodeToPlugins(:94)` | ❌ no SPI, no ASM tree API |
| 3 | **JPMS module layers** — multiple (bootstrap / transformer / app), with a module-aware `ModuleClassLoader` over secure jars | `cpw.mods.securejarhandler/cpw.mods.cl.ModuleClassLoader.readerToClass(:190)`; stack frames `MC-BOOTSTRAP`, `TRANSFORMER` layers | 🔶 partial (4.14 ModuleLayer) — need multi-layer + `ModuleClassLoader`-over-jars + runtime `addOpens`/`addExports`/`addReads` |
| 4 | **Module export/open negotiation** (programmatic + `--add-opens`); mods force packages open to Forge/Mixin for reflection | MixinExtras crash: `module … export package … to …`, `contains package …` errors | ❌ no runtime `addOpens`/`addExports` yet |
| 5 | **Mixin** load-time weaving via the transform chain: merge mixin bytecode into target, `@Inject`/`@Redirect`/`@Overwrite`, hierarchy validation, INVOKESPECIAL resolution for detached mixins — operates on ASM tree, needs `COMPUTE_FRAMES` | `org.spongepowered.asm.mixin` (sponge-mixin-0.15.2) as a launch plugin; Mixin wiki: "pumped through the transformer chain… processed before class loading… hierarchy validation… INVOKESPECIAL opcodes… analyse superclass hierarchy" | ❌ no Mixin, no ASM tree, no frame computation |
| 6 | **AccessTransformer (AT)**: rewrite `access` flags (public-ize, de-final) per config; runtime must honor transformed flags | Forge `accesstransformer.cfg`; implemented as a class transformer | ❌ no instrumentation; also rustj access checks must respect transformed flags |
| 7 | **`ServiceLoader`** SPI discovery (modlauncher's `ITransformationService`, `ILaunchPluginService`, FML's `IModFileProvider`/`IModProvider` are all `META-INF/services`-discovered) | modlauncher + FML design (SPI-driven) | 🔶 verify rustj `ServiceLoader` + `getResources` |
| 8 | **Reflection completeness**: `getDeclaredFields/Methods/Constructors`, `setAccessible(true)` honored in access checks, `Method.invoke`, `Field.get/set` (static + instance), array reflection, **annotation reflection** (`getDeclaredAnnotation`/`getAnnotationsByType`) for `@Mod`/`@SubscribeEvent`/`@ObjectHolder`/`@CapabilityInject` discovery | Forge mod wiring everywhere | 🔶 partial (4.15: `Class.forName0`, `getDeclared*`, `Method.invoke0` via MethodHandle, static `Field.get/set` B.5.1-3) — **missing**: `setAccessible` honored, `Constructor`, array reflection, annotation reflection |
| 9 | **`MethodHandles.Lookup.privateLookupIn`** + hidden-class definition (deep reflection into module-private classes; Mixin/Forge use it) | Java 9+ API, Forge uses it | ❌ not implemented |
| 10 | **`sun.misc.Unsafe`** subset (object field base/offset, compare-and-swap) — some libs/mods lean on it | Forge ecosystem | 🔶 byte[] only (§9.5) |
| 11 | **`java.lang.instrument`** (`Instrumentation`, `ClassFileTransformer`, `redefineClasses`/`retransformClasses`, agent `premain`/`agentmain`) — for runtime retransform / agent-attached Mixin | standard JVM API; Mixin can attach as agent | ❌ none |
| 12 | **Event bus** (`@SubscribeEvent` reflective method scan + dispatch), **`@ObjectHolder`** static-field injection, **`@Mod`** lifecycle | Forge libraries (not JVM features) — built on reflection + annotations | 🔶 depends on #8 (annotations) + #7 (ServiceLoader) |

---

## (b) Gap analysis vs rustj

- **Central gap = instrumentation engine (#1, #2, #5, #6, #11).** rustj has **no** bytecode instrumentation: no transforming classloader, no transformer chain, no ASM (event or tree API), no `java.lang.instrument`, no Mixin. This is the single largest piece of new work for Forge support and the prerequisite for AT, Mixin, and most modding.
- **Reflection completeness (#8, #9, #10).** rustj's 4.15 reflection is a strong start but needs: `setAccessible(true)` actually suppressing access checks during linkage/invoke; `Constructor`/`newInstance`; array reflection (`Array.get/set/newInstance`); **annotation reflection** (the discoverability backbone for `@Mod`/`@SubscribeEvent`/`@ObjectHolder`); `privateLookupIn` + hidden classes; broader `Unsafe`.
- **Module runtime ops (#3, #4).** rustj has 4.11 module parsing + 4.14 ModuleLayer structure, but Forge needs *runtime* `Module.addOpens`/`addExports`/`addReads` (programmatic, not just descriptor-driven), multi-layer (bootstrap/transformer/app), and a `ModuleClassLoader` that serves classes from modular jars with transform hooks.
- **Service/resource loading (#7).** `ServiceLoader` + `ClassLoader.getResources` enumeration — verify/complete.
- **Favorable:** the event bus, `@ObjectHolder`, `@Mod` lifecycle are **Forge library** code (pure Java on top of reflection + annotations) — once rustj runs real `java.*` + has complete reflection + ServiceLoader, those run "for free" without JVM work.

---

## (c) Shared foundation design

Build the foundation in this order — each layer unblocks the next:

1. **`java.lang.instrument` + transforming classloader.** Implement `Instrumentation` (singleton: `addTransformer`/`removeTransformer`/`retransformClasses`/`redefineClasses`/`isModifiableClass`/`getAllLoadedClasses`) + a transform hook in rustj's class-define path that runs registered `ClassFileTransformer`s over the bytes before defining. Provide a `TransformingClassLoader` (rustj-side, mirrors modlauncher's) that Forge's launcher can plug into. **This is the keystone** — enables AT, Mixin (load-time), and generic transforms. (No JIT needed; the interpreter runs transformed bytecode.)
2. **ASM subset port.** Event API first (`ClassReader`/`ClassVisitor`/`ClassWriter` + access-flag rewrite — enough for AT + simple transformers), then **tree API** (`ClassNode`/`MethodNode`/`FieldNode` — required by Mixin + `ILaunchPluginService`), then **`COMPUTE_FRAMES`** stack-map frame computation (required for any method that Mixin rewrites). See (e).
3. **Reflection completeness.** `setAccessible` honored; `Constructor`/`newInstance`; `Array.*`; annotation reflection (`getDeclaredAnnotations`/`getAnnotationsByType`/`getAnnotation` — needs runtime-visible annotation bytes from the class file, retained at parse); `privateLookupIn` + hidden classes; broader `Unsafe`.
4. **Module runtime ops.** Runtime `Module.addOpens`/`addExports`/`addReads`; multi-layer `ModuleLayer.defineModulesWithOneLoader`/`defineModules`; a `ModuleClassLoader` over modular/secure jars that feeds the transform hook.
5. **ServiceLoader + getResources.** Verify/complete `java.util.ServiceLoader` + `ClassLoader.getResources` enumeration (mod + service discovery).
6. **Mixin host** (on top of 1+2+3+4): register Mixin as a `ClassFileTransformer`/launch-plugin, drive its config (`*.mixins.json`), apply merge/inject/redirect/overwrite on ClassNode, recompute frames. (Mixin itself is Java code that runs once the foundation exists — porting/hosting it is integration, not reimplementation, though it stresses the ASM tree API + frames hard.)

---

## (d) First target

**Boot modlauncher + the NeoForge FML loader itself — NOT Minecraft.**

Get far enough that: modlauncher's `Launch` initializes → `ServiceLoader` discovers `ITransformationService`/`ILaunchPluginService` → `TransformingClassLoader` + `ModuleClassLoader` build the bootstrap/transformer layers → at least one no-op `ITransformationService` transforms a hello-world class through the chain → FML's mod-discovery (`IModProvider` via ServiceLoader) parses mod metadata. **Stop before loading any `net.minecraft.*` class.**

This exercises the entire shared foundation (transform pipeline + ASM + modules + ServiceLoader + reflection) without needing the full MC class graph, LWJGL (#5 real JNI), or working MC game loops. It is the natural "Forge runs at all" milestone and the honest first checkpoint.

A **sub-increment** before even that: a standalone test where a trivial `ITransformationService` rewrites one method of a hello-world class via the ASM event API through rustj's `TransformingClassLoader`, and the rewritten class runs correctly in the interpreter. This proves the instrumentation seam end-to-end before touching real Forge jars.

---

## (e) ASM full-vs-subset decision

**Subset, grown incrementally — but full Mixin eventually needs near-full ASM.**

- **Phase 1 (event API):** `ClassReader`/`ClassVisitor`/`ClassWriter` + access-flag rewrite. Enables AT + simple `IClassTransformer`s. **No frame computation needed** if transformers don't change method bodies' stack maps (AT doesn't).
- **Phase 2 (tree API):** `ClassNode`/`MethodNode`/`FieldNode` + `accept`/`visit`. Required by Mixin + `ILaunchPluginService` (which hands plugins a `ClassNode`). Mixin's `MixinApplicator` merges nodes.
- **Phase 3 (`COMPUTE_FRAMES`):** stack-map frame re-computation for rewritten methods (Mixin `@Inject`/`@Redirect` that change control flow). This is the algorithmically hard part (ASM's `FrameAnalyzer`/`Analyzer` — frame inference + stack-map v2 encoding). **Full Mixin support cannot avoid this.**
- **Phase 4 (full ASM):** signatures, annotations tree, all visitors, edge encodings — only as a transformer needs them.

**Honest:** AT + simple Forge transformers + the transform-pipeline classloader go far on Phase 1-2. **Full Mixin support ≈ near-full ASM port including `COMPUTE_FRAMES`** (multi-thousand-line, algorithmically nontrivial). Recommendation: build Phases 1-2 to unlock the foundation + AT + basic mods; treat Phase 3 (frames) as the gating effort for full Mixin, undertaken when a target mod actually requires Mixin. Do **not** attempt to port all of ASM up front.

ASM is a candidate for the revised §2 "necessary" dependency (it's a pure-data bytecode library, no unsafe, widely tested) — **but** rustj's posture is hand-porting pure-computation libraries (DEFLATE/zip were hand-ported), and ASM is exactly that category. Decision: **hand-port the subset** (consistent with §2's spirit and the DEFLATE/zip precedent), re-evaluate pulling ASM as a crate only if the `COMPUTE_FRAMES` port proves disproportionately costly.

---

## (f) Honest work estimate

- **`java.lang.instrument` + transforming classloader + transform hook (foundation #1):** weeks to ~1-2 months. The hook is a clean seam in rustj's class-define path; the `Instrumentation` API surface is small.
- **ASM Phase 1-2 (event + tree API, no frames):** ~1-2 months. Mechanical port of a well-documented library; bounded subset.
- **ASM Phase 3 (`COMPUTE_FRAMES`):** the hard part — **months**, algorithmically nontrivial (data-flow frame inference + stack-map v2 encoding). Gates full Mixin.
- **Reflection completeness (#8):** ~1-2 months (`setAccessible`, `Constructor`, array reflection, annotation reflection, `privateLookupIn`).
- **Module runtime ops (#3,#4) + ServiceLoader:** weeks–~1 month, building on 4.14.
- **Mixin hosting (foundation #6):** weeks of integration once 1-4 exist (Mixin is Java; hosting = wiring its transformer + config), **but it stresses ASM tree + frames hard** — realistically debugged over months against real mods.
- **First target (boot modlauncher+FML, no MC):** reachable after foundation 1-5; **a few months** of integration work, dominated by the ASM + module-layer + ServiceLoader completeness it demands.

**Bottom line:** Forge support is **not** one new subsystem — it's the convergence of the instrumentation engine (the big new piece) + reflection completion + module runtime ops, all of which are independently useful and mostly already-started. The instrumentation engine + ASM are the long poles. **No #6a (native layout) dependency** — Forge transforms and runs bytecode, which the interpreter already executes; it does not need byte-addressable objects. → Forge support can proceed **in parallel with, and independent of, #6a/#6**.

---

## Key anchors

- **modlauncher** (launcher, `cpw.mods.modlauncher`): `TransformingClassLoader.maybeTransformClassBytes(:57)`; `ClassTransformer.transform(:120)`; `LaunchPluginHandler.offerClassNodeToPlugins(:94)`; `serviceapi.ILaunchPluginService.processClassWithFlags(:156)`. (Search-verified, modlauncher-11.0.5.)
- **securejarhandler** (JPMS modules): `cpw.mods.cl.ModuleClassLoader.readerToClass(:190)`. (Search-verified.)
- **NeoForge FML loader**: `net.neoforged.fml.loading` (`BackgroundWaitger.runAndTick(:29)`), `fmlloader`/`fmlcore`/`fmlcore` mod discovery via `IModProvider`/`IModFileProvider` ServiceLoader. (loader-4.0.42.)
- **Mixin**: `org.spongepowered.asm.mixin` (sponge-mixin-0.15.2) — launch plugin + transformer; operates on ASM `ClassNode`; processes mixins before target load; hierarchy validation; INVOKESPECIAL resolution for detached mixins. Config via `*.mixins.json`. (Mixin wiki + search stack.)
- **Module negotiation**: runtime `--add-opens`/`Module.addOpens`/`addExports`/`addReads`; multiple layers (`MC-BOOTSTRAP`, `TRANSFORMER`, app). (Search stack frames + MixinExtras module errors.)
- **JVM APIs to implement**: `java.lang.instrument` (`Instrumentation`, `ClassFileTransformer`, `redefineClasses`, `retransformClasses`, `premain`/`agentmain`); ASM event+tree API + `COMPUTE_FRAMES`; reflection completion (`setAccessible`, `Constructor`, `Array`, annotations, `privateLookupIn`); `Module` runtime ops; `ServiceLoader`/`getResources`.
- **rustj integration points**: classloader define-path (transform hook); 4.11/4.14 module system (extend with runtime opens/exports + multi-layer + `ModuleClassLoader`); 4.15 reflection (extend to completeness); class-file parser (retain runtime-visible annotations for reflection); interpreter (runs transformed bytecode unchanged — no JIT needed).
