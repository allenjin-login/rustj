# Faithful HotSpot G1 GC — Research

**Wayfinder ticket:** [#7 — T6 忠实 G1 移植](https://github.com/allenjin-login/rustj/issues/7)
**Date:** 2026-07-23
**Provenance:** Done inline in the main loop (the `/research` subagent died on a gateway usage cap).
Read `E:\rustj\jdk-master\src\hotspot\share\gc\g1\{g1CollectedHeap.hpp,g1HeapRegion.hpp,g1HeapRegionRemSet.hpp,g1BarrierSet.hpp}`
+ directory inventory (155 headers). File:line citations verified by direct read.

**Headline — two findings, one sober, one favorable:**

1. **Sober:** A *faithful* G1 (regions, card tables, remembered sets, block-offset tables) is **byte-address/pointer-based throughout** — `G1HeapRegion` is a `[bottom, end)` `HeapWord*` range, remsets are `G1CardSet`s of cards. Faithful G1 therefore **requires the #6a native object-layout rework** (contiguous, byte-offset-addressable objects). Faithful G1 ≈ #6a + the GC algorithms.
2. **Favorable:** rustj's **u32 handle model is an asset, not a blocker**, for *moving* GC. Handles are stable indices (a `Vec<Oop>` slot survives `Vec` realloc); moving an object's storage does **not** invalidate any `Reference` — far better than HotSpot's raw-`oop` world, which needs forwarding pointers + read barriers on every access. So a **functionally moving GC (evacuation + compaction, the core benefit that solves rustj's grow-only-heap debt) is achievable WITHOUT #6a** via a forwarding-in-slot scheme on the current `Vec<Oop>` heap. Defer faithful region-based G1 to coincide with #6a.

---

## (a) Component Map (`src/hotspot/share/gc/g1/` — 155 headers)

| Subsystem | Anchor | Role |
|---|---|---|
| **Heap hub** | `g1CollectedHeap.hpp:148` `class G1CollectedHeap : public CollectedHeap` (comment :61-64: "Garbage First… concurrent marking with parallel, incremental compaction of heap subsets that will yield large amounts of garbage") | The collector; owns regions, collection set, barrier set, concurrent mark, allocator; drives STW pauses + concurrent cycles |
| **Region** | `g1HeapRegion.hpp:71` `class G1HeapRegion : CHeapObj`; fields `_bottom`/`_end` (`HeapWord* const`, :74-75), `_top` (`Atomic<HeapWord*>`, :77 bump pointer), `_bot` (`G1BlockOffsetTable*`, :79); `bottom()`/`end()`/`top()`/`used()`/`free()` (:90-120); humongous = `StartsHumongous`+`ContinuesHumongous` (:63-70) | Smallest independently-collectible unit (comment :59-60). A contiguous `[bottom,end)` word range, bump-allocated at `_top`, with a block-offset table to locate object starts |
| **Block-offset table** | `g1BlockOffsetTable.hpp` (ref :28, :79, :132-140 `advance_to_block_containing_addr`/`block_start`) | Maps any address in a region back to the start of the object occupying it — needed to scan/iterate objects in a region without an object header chain |
| **Remembered set** | `g1HeapRegionRemSet.hpp:41` `class G1HeapRegionRemSet`; `_code_roots` (`G1CodeRootSet`, :44 — nmethods pointing in), `_hr` (:49), `card_set()` → `G1CardSet` (:56-64); `cardset_is_empty()` (:70) | Per-region set of **cards (in other regions) that contain references into this region**. Lets G1 collect a region by scanning only its remset, not the whole heap. Backed by `g1CardSet.hpp` + `g1FromCardCache.hpp` |
| **Card table (two of them)** | `g1BarrierSet.hpp:66` `G1BarrierSet : CardTableBarrierSet`; `_refinement_table` (`Atomic<G1CardTable*>`, :71); `swap_global_card_table()` (:84) | Two card tables — mutator's "card table" + "refinement table" — swapped when dirty-card count exceeds a threshold (comment :38-64). Removes per-write synchronization between mutator and refinement threads |
| **Write barriers** (GC↔mutator seam) | `g1BarrierSet.hpp:101` `write_ref_field_pre` (SATB enqueue of pre-value), `:106` `write_ref_field_post` (card dirty); `:97-99` `write_ref_array_pre`; `:114` `G1SATBMarkQueueSet`; `on_thread_create/destroy/attach/detach` (:109-112) | **Pre-write (SATB):** during concurrent mark, enqueue the *old* reference value so the mark snapshot includes since-unlinked objects. **Post-write:** dirty the card containing the field so the remset learns of a new inter-region edge. These run on **every** reference store |
| **Concurrent mark** | `g1ConcurrentMark.hpp` + `g1ConcurrentMarkThread.hpp` + `g1ConcurrentMarkBitMap.hpp` + `g1ConcurrentMarkRemarkTasks.hpp`; `g1BarrierSet.hpp:69` `_satb_mark_queue_set` | Snapshot-At-The-Beginning marking: concurrent thread marks live graph from roots + SATB buffers; remark STW; produces the liveness bitmap that drives region selection ("garbage first") |
| **Concurrent refine** | `g1ConcurrentRefine.hpp` + `g1ConcurrentRefineThread.hpp` + `g1ConcurrentRefineSweepTask.hpp` + `g1ConcurrentRefineStats.hpp` | Refinement threads drain the refinement card table into remsets concurrently, keeping pause times bounded |
| **Collection set** | `g1CollectionSet.hpp` + `g1CollectionSetCandidates.hpp` + `g1CSetCandidateGroup` (remset :47) | The set of regions chosen for a collection pause; "garbage first" = pick regions with the most reclaimable garbage |
| **Allocator** | `g1Allocator.hpp` + `g1AllocRegion.hpp` + `g1EvacStats.hpp` | Bump-pointer allocation into the current mutator region; PLABs for evacuation allocation in survivor/old |
| **Evacuation** | `g1EvacStats.hpp` + `g1EvacFailureRegions.hpp` + `g1EvacInfo.hpp`; (driver in `g1CollectedHeap.cpp` evac path) | Copy live objects out of collection-set regions into survivor/old regions; forward the old copies. `EvacFailureRegions` handles regions where evacuation failed (PROMOTION_FAILED) |
| **Full GC** | `g1FullCollector.hpp` + `g1FullGC{Mark,Prepare,Compact,Adjust}Task.hpp` + `g1FullGCCompactionPoint.hpp` + `g1FullGCMarker.hpp` | Fallback full compaction (mark + compact in place via compaction points) when evacuation can't keep up |
| **Policy / pause prediction** | `g1Policy.hpp` + `g1Analytics.hpp` + `g1AnalyticsSequences.hpp` + `g1HeapSizingPolicy.hpp` + `g1SurvRateGroup.hpp` | Predicts pause times and survival rates to size the collection set to hit pause goals (`MaxGCPauseMillis`) |
| **Region manager / sets** | `g1HeapRegionManager.hpp` + `g1HeapRegionSet.hpp` + `g1HeapRegionType.hpp` + `g1CommittedRegionMap.hpp` + `g1EdenRegions.hpp` + `g1SurvivorRegions.hpp` | Region inventory + free/eden/survivor/old/humongous region lists; commit/decommit memory |
| **NUMA** | `g1NUMA.hpp` | NUMA-aware region placement |
| **C1/C2 barrier integration** | `g1/c1/g1BarrierSetC1.hpp` + `g1/c2/g1BarrierSetC2.hpp` + `g1BarrierSetAssembler.hpp` + `g1BarrierSetRuntime.hpp` | The barriers emitted by the JIT compilers — **couples G1's barriers to the JIT (#6)** |

---

## (b) Port Sequence

**Verdict: two tracks. Track 1 (no #6a, pragmatic moving GC) first; Track 2 (faithful region G1) when #6a lands.**

### Track 1 — forwarding-in-slot moving GC on the current `Vec<Oop>` heap (no #6a)

1. **Root enumeration.** Enumerate all live roots: static field region, every thread's operand stack + locals, the JNI local/global ref tables, class metadata mirrors, `Thread` instances. (rustj already has the static-field region + thread stacks + handle tables — this is assembly, not new structure.)
2. **Mark.** STW mark from roots over the handle heap (`heap.get(h)` → follow `Reference` fields). Produce a live-bitset over handle indices.
3. **Evacuate/compact via forwarding.** Add `Oop::Forwarded(Reference)` (or a forward tag in `Instance`). For each live object, copy its `Oop` value to a fresh slot in a new `Vec<Oop>` (compaction) and set the old slot to `Forwarded(new)`. After copy, walk all roots + all live objects' reference fields and **resolve forwarding** (replace each `Reference` with its forwarded target) — *or* leave forwards in place and resolve lazily on `get()`.
4. **Reclaim.** Swap the heap to the compacted `Vec` (handles re-indexed) **or** keep handles stable and free dead/forwarded slots via a free-list. Decide: stable-handle + free-list (simpler, keeps handles valid) vs. re-index (denser, but every `Reference` value changes — requires the resolve pass to rewrite them all).
5. **Write-barrier hook (minimal).** Add a post-store hook on `putfield` (reference) / `aastore` (reference) — initially a no-op or a simple "remember this object is dirty" card approximation. Required before concurrency.

*Verifiable:* a program that allocates tens of thousands of objects, drops all references, triggers GC, and observes (a) heap size drops, (b) surviving objects' values intact, (c) no use-after-free. This **solves the grow-only-heap debt** and is a real moving GC — without #6a.

### Track 2 — faithful region-based G1 (needs #6a)

When #6a lands (contiguous byte-layout objects), port in this order: `G1HeapRegion` (`[bottom,end)` + `_top` + BOT) → card table (`G1CardTable`) → remembered set (`G1HeapRegionRemSet` + `G1CardSet` + `G1FromCardCache`) → **write barriers** (`write_ref_field_pre` SATB + `write_ref_field_post` card-dirty) into the interpreter (and JIT) → STW young collection (evacuation) → concurrent mark (SATB, `G1ConcurrentMark`) → concurrent refine (`G1ConcurrentRefine`) → collection-set policy / pause prediction (`g1Policy` + `g1Analytics`) → humongous objects → evac-failure / full GC (`g1FullCollector`). Each layer is independently testable once #6a gives byte-addressable objects.

---

## (c) `gc/` designated-unsafe module structure

**Nuance: G1's algorithms are mostly *safe* Rust.** Unlike `jit/` (raw machine code) and `jni/` (FFI dlopen/indirect-call), a GC is marking + copying + bitmaps + hashsets over the heap — safe data-structure work. The unsafe concentrates in two places:

```
gc/
  mod.rs                # safe façade
  roots.rs              # root enumeration (static region, thread stacks, JNI handle tables) — safe
  mark.rs               # STW mark (traverse handle graph) — safe (RefCell + Vec)
  evac.rs               # evacuation: copy Oop + forwarding — safe (Vec<Oop> ops)
  forwarding.rs         # Oop::Forwarded variant + resolve-on-get — safe
  freelist.rs           # slot free-list / compaction — safe
  barrier.rs            # write-barrier hooks (SATB enqueue + card-dirty) — safe (queues + bitmap)
  cardtable.rs          # card table (Track 2) — safe (byte bitmap over region space)
  remset.rs             # remembered sets (Track 2) — safe (card-set hash structures)
  concmark.rs           # concurrent mark thread — safe (std::sync + atomics); coordination
                         #   with mutator via safepoints/handshake (see unsafe note below)
  native_layout.rs      # #[allow(unsafe_code)] ONLY in Track 2: raw HeapWord* object access,
                         #   BOT, card-table base pointers — this is the #6a-coupled unsafe seam.
                         #   In Track 1 this module does not exist (no byte addressing needed).
```

**Track 1 may not need a designated-unsafe `gc/` module at all** — it's safe Rust over `Vec<Oop>` + `RefCell` + `std::sync`. The designated-unsafe designation becomes necessary only in Track 2 (raw pointer object access via #6a) and for **safepoint/handshake coordination** between mutator threads and GC threads (suspending threads at safe points — atomic + memory-ordering, arguably `unsafe`-adjacent via `std::sync` but expressible safely). So `gc/` is the **least** inherently-unsafe of the three designated modules (`jit/` > `jni/` > `gc/`).

---

## (d) Smallest first increment (Track 1)

**A stop-the-world mark + evacuate on the current `Vec<Oop>` heap, triggered manually, reclaiming dead objects.**

- Roots: static field region + current thread's stack/locals + (for a first cut) ignore JNI handle tables.
- Mark: BFS/DFS from roots over `Reference` fields; live-bitset over handle indices.
- Evacuate: allocate a fresh `Vec<Oop>`; for each live handle in order, copy its `Oop` to the new Vec at a new index, record `old → new` in a forwarding map, set old slot to `Forwarded(new)`.
- Resolve: walk roots + live objects' reference fields, rewrite each `Reference` via the forwarding map (so post-GC, all references point to new indices). Drop the old Vec.
- *Verification:* integration test — allocate a linked structure + lots of garbage, null the garbage refs, call `gc()`, assert (a) the live structure is fully intact and traversable, (b) heap slot count dropped to ~live set, (c) a subsequent allocation reuses freed capacity.

**Do NOT start with:** concurrent mark, SATB barriers, regions, card tables, remembered sets, humongous, evac failure, or pause prediction. All are Track 2 / later-Track-1. Get STW mark+evac correct first; it's the foundation everything else refines.

---

## (e) Honest work estimate

- **Track 1 — STW mark + evacuate + forwarding (no #6a):** **weeks to a couple months.** The algorithms are simple; the work is **root enumeration** (finding every live reference across statics, every thread's frame, JNI handles, class mirrors) and the forwarding/resolve pass. Solves the grow-only-heap debt — high value, modest cost.
- **Track 1 — concurrent mark (SATB) + write barriers in the interpreter:** a few more months. SATB correctness (the mark snapshot) is subtle; barriers must hook every reference store (`putfield`, `aastore`, and later JIT code-gen). Thread coordination via safepoints/handshakes (couples to Phase B threading + the Vm singleton).
- **Track 2 — faithful region G1 (with #6a):** a **large multi-quarter effort**, dominated by #6a itself. Regions + BOT + card table + remsets are straightforward once byte-addressable objects exist; the hard parts are concurrent refine, pause-time prediction, humongous, and evac-failure/full-GC.
- **JIT barrier integration (`g1/c1/`, `g1/c2/`):** couples to #6 (JIT) — the JIT must emit the pre/post barriers in compiled code. Not tractable until C1 exists.

**Key architectural finding (favorable):** the map's fog note feared "移动式 GC 破坏裸 u32 句柄,大重塑." The opposite is true: **handles are stable references, so moving GC is *easier* in rustj than in HotSpot** (no forwarding-pointer chase on every pointer deread; only the storage layer moves). The real cost of *faithful* G1 is #6a (byte-addressable objects for regions/cards/remsets), not the moving per se. → Sequence: **Track 1 (moving GC, no #6a) now** to kill the grow-only debt; **Track 2 (faithful G1) when #6a lands.**

---

## Key file:line anchors

- Hub: `g1CollectedHeap.hpp:148` (class), comment `:61-64`; forward decls `:67-88` (`G1RemSet`, `G1ConcurrentMark`, `G1ConcurrentRefine`, `ReferenceProcessor`, `G1Allocator`); scanner queue typedefs `:90-94`.
- Region: `g1HeapRegion.hpp:71` (class), `_bottom`/`_end`/`_top`/`_bot` `:74-79`, `bottom/end/top/used/free` `:90-120`, humongous `:63-70`, `block_start` `:139`, `object_iterate` `:142`.
- Remembered set: `g1HeapRegionRemSet.hpp:41` (class), `_code_roots` `:44`, `_hr` `:49`, `card_set()` `:56-64`, `cardset_is_empty` `:70`; backed by `g1CardSet.hpp` + `g1FromCardCache.hpp`.
- Card table + barriers: `g1BarrierSet.hpp:66` (class), two-table design comment `:38-64`, `_refinement_table` `:71`, `swap_global_card_table` `:84`, `write_ref_field_pre` (SATB) `:101`, `write_ref_field_post` (card-dirty) `:106`, `write_ref_array_pre` `:97-99`, `_satb_mark_queue_set` `:69`, `on_thread_*` `:109-112`, `grain_shift` (region size) `:120`.
- Concurrent mark: `g1ConcurrentMark.hpp` + `g1ConcurrentMarkThread.hpp` + `g1ConcurrentMarkBitMap.hpp` + `g1ConcurrentMarkRemarkTasks.hpp`.
- Refine: `g1ConcurrentRefine.hpp` + `g1ConcurrentRefineThread.hpp` + `g1ConcurrentRefineSweepTask.hpp`.
- Collection set: `g1CollectionSet.hpp` + `g1CollectionSetCandidates.hpp`.
- Allocator: `g1Allocator.hpp` + `g1AllocRegion.hpp` + `g1EvacStats.hpp` + `g1EvacFailureRegions.hpp`.
- Full GC: `g1FullCollector.hpp` + `g1FullGC{Mark,Prepare,Compact,Adjust}Task.hpp` + `g1FullGCCompactionPoint.hpp`.
- Policy: `g1Policy.hpp` + `g1Analytics.hpp` + `g1HeapSizingPolicy.hpp` + `g1SurvRateGroup.hpp`.
- Region manager: `g1HeapRegionManager.hpp` + `g1HeapRegionSet.hpp` + `g1HeapRegionType.hpp` + `g1CommittedRegionMap.hpp`.
- JIT barrier integration: `g1/c1/g1BarrierSetC1.hpp` + `g1/c2/g1BarrierSetC2.hpp` + `g1BarrierSetAssembler.hpp` (couples to #6).
- rustj integration points: u32-handle heap (`src/runtime/heap.rs`) — handles are stable, so moving GC is tractable (Track 1 forwarding-in-slot); static-field region + thread stacks (Phase B) — root enumeration; `Vm` singleton (Phase V) — safepoint/handshake coordination; grow-only-heap debt (current §9.5) — Track 1 directly resolves it.
