# Real JNI Architecture — Research

**Wayfinder ticket:** [#5 — T4 真 JNI 架构](https://github.com/allenjin-login/rustj/issues/5)
**Date:** 2026-07-23
**Provenance:** Done inline in the main loop (the `/research` subagent died on a gateway usage cap).
Read `E:\rustj\jdk-master\src\hotspot\share\prims\{jni.cpp,jvm.cpp,nativeLookup.cpp}`. File:line citations
verified by direct read. Contingent on **T1** codifying the revised §2 (allow `libloading`/`jni-sys` as
"necessary") and the `jni/` designated-unsafe module (revised §1).

**Headline:** real JNI for rustj is **mechanically large but architecturally simple**, and — unlike the
JIT (#6) — **composes with the current u32-handle + id-arena heap**; it does NOT require the #6a native
object-layout rework. The work is: (1) a `libloading`-backed dlopen/dlsym bridge for `JVM_LoadLibrary`/
`FindLibraryEntry`/`Unload`; (2) JNI name-mangling + a dynamic native-resolution path extending the existing
`NativeRegistry`; (3) a self-rolled C-ABI `JNIEnv`/`JavaVM` function-pointer table (~230 entries) bridging
into the VM. The irreducible unsafe surface is the FFI (dlopen/dlsym + indirect calls into native code).

---

## (a) Component Map (HotSpot side, all `src/hotspot/share/prims/`)

| Subsystem | Anchor | Role |
|---|---|---|
| **JNIEnv function table** | `jni.cpp:3164` `struct JNINativeInterface_ jni_NativeInterface = { … }` | The C-ABI table of ~230 function pointers handed to native code as `JNIEnv*`. Layout (from :3164): 4 reserved nulls → `GetVersion` (:3171) → `DefineClass`/`FindClass` (:3173-3174) → reflection (`FromReflectedMethod/Field`, `ToReflectedMethod/Field`) → `GetSuperclass`/`IsAssignableFrom` → `Throw`/`ThrowNew`/`ExceptionOccurred`/`ExceptionDescribe`/`ExceptionClear`/`FatalError` (:3186-3191) → local-frame (`Push`/`PopLocalFrame`) → global/local refs (`NewGlobalRef`/`DeleteGlobalRef`/`DeleteLocalRef`/`IsSameObject`/`NewLocalRef`/`EnsureLocalCapacity`) → `AllocObject`/`NewObject`/`NewObjectV`/`NewObjectA` → `GetObjectClass`/`IsInstanceOf` → `GetMethodID` (:3212) → **`Call<Type>Method{,V,A}` × 9 types × {virtual, Nonvirtual}** (:3214-3280+) → `Get/Set<FieldID` + `Get/Set<Type>Field` × {instance, static} → `CallStatic<Type>Method{,V,A}` → strings (`NewString`/`GetStringLength`/`GetStringChars`/`ReleaseStringChars`/`NewStringUTF`/`GetStringUTFChars`/…) → arrays (`New<Type>Array`/`Get<Type>ArrayElements`/`Release`/`Get<Type>ArrayRegion`/`Set<Type>ArrayRegion`) → `RegisterNatives`/`UnregisterNatives` → `MonitorEnter`/`MonitorExit` → `GetJavaVM` → `GetStringRegion`/`GetDirectBufferAddress`/… |
| Table accessor / override | `jni.cpp:3513` `jni_functions()` / `:3521` `jni_functions_nocheck()`; `:3464` `copy_jni_function_table()` | Per-thread accessor (returns the table, possibly JVMTI-overridden); `copy_jni_function_table` lets JVMTI/Panama replace entries |
| **JavaVM invocation table** | `jni.cpp:4065` `const struct JNIInvokeInterface_ jni_InvokeInterface = { … }`; `:3134` `extern struct JavaVM_ main_vm;`; `:3549` `struct JavaVM_ main_vm = {&jni_InvokeInterface};` | The 4-entry invocation interface: `DestroyJavaVM` (:3740 `jni_DestroyJavaVM_inner`), `AttachCurrentThread`, `DetachCurrentThread`, `GetEnv`. `JavaVM_` is a struct whose first slot is a pointer to this table (standard JNI C ABI). `JNI_CreateJavaVM_inner` at `:3583` |
| **Library-load bridge** (JVM_* → os) | `jvm.cpp:3150` `JVM_LoadLibrary(name, throwException)` → `:3156` `os::dll_load(name, ebuf, sizeof ebuf)`; `:3182` `JVM_UnloadLibrary(handle)` → `os::dll_unload`; `:3188` `JVM_FindLibraryEntry(handle, name)` → `:3189` `os::dll_lookup(handle, name)` | The bridge `java.lang.ClassLoader$NativeLibraries` calls. `JVM_LoadLibrary` uses `ThreadToNativeFromVM` (:3155) to transition the thread to `_thread_in_native` before the OS call (so GC may run), and throws `UnsatisfiedLinkError` on failure (:3168). `JVM_LoadZipLibrary` (:3146) is a separate special-case for the JDK zip lib |
| OS dynamic-loader abstraction | `runtime/os.cpp` + `os/windows/os_windows.cpp` | `os::dll_load`/`dll_lookup`/`dll_unload` are the platform seam: `dlopen`/`dlsym`/`dlclose` (POSIX) or `LoadLibraryA`/`GetProcAddress`/`FreeLibrary` (Windows). **This is where rustj's real FFI lands** |
| **Native-method resolution** | `nativeLookup.cpp:409` `NativeLookup::lookup` → `:380` `lookup_base` → `:308` `lookup_entry` + `:345` `lookup_entry_prefixed`; `:242` `lookup_style` | Resolves a Java `native` method to a C function address. `lookup_entry` (:308) tries the **short name** (`Java_pkg_Class_method`, :324) then the **long name** (`Java_pkg_Class_method__argsig`, :336). `lookup_style` (:242): for system classes (null loader) tries `lookup_special_native` (:231, a fixed table at :215) then `os::dll_lookup(os::native_java_library(), …)` (:256); otherwise iterates the class's registered native libs and `os::dll_lookup(it.next()->os_lib(), jni_name)` (:288). `lookup_entry_prefixed` (:345) handles the `@Native`-prefix / wrapper-renamed path |
| `JNI_OnLoad` lifecycle | (orchestrated by `java.base` `ClassLoader$NativeLibraries`) | After `JVM_LoadLibrary`, the loader resolves `"JNI_OnLoad"` via `JVM_FindLibraryEntry` and invokes `jint JNI_OnLoad(JavaVM*, void*)`, handing the lib the `JavaVM*`. `JNI_OnUnload` symmetrically. (Not in `prims/`; the hook name is looked up like any other symbol.) |
| Thread attach | `JNIInvokeInterface_::AttachCurrentThread` / `DetachCurrentThread` | Native-spawned threads must attach to obtain a `JNIEnv*` before calling back in. Couples to rustj's `ThreadManager` (Phase B threading work) |

---

## (b) Port Sequence / Phasing

1. **`jni/` module skeleton + loader bridge.** `#![allow(unsafe_code)]` `jni/` module. Implement `dll_load`/`dll_lookup`/`dll_unload` via `libloading` (the revised-§2 "necessary" dep). Wire `JVM_LoadLibrary`/`JVM_FindLibraryEntry`/`JVM_UnloadLibrary` JVM_* natives (rustj already has a `NativeRegistry`; add these). Connect `java.lang.System.loadLibrary`/`ClassLoader$NativeLibraries.load` to them. *Verifiable: a Java program calls `System.load("path.dll")` and rustj logs a successful dlopen + handle.*
2. **Native resolution path.** Port `nativeLookup` short+long name-mangling (`Java_<pkg>_<Class>_<method>` / `__<argsig>`, mangling `.`→`_`, `_`→`_1`, `;`→`_2`, `[`→`_3`). Add a per-class native-library registry (which libs a class's loader has loaded). Extend the native-dispatch **miss path**: when a `native` method isn't in the compile-time `NativeRegistry`, run name-mangling + `dll_lookup` across the class's libs → cache the `address` on the method (like HotSpot's `Method::set_native_function`). Also handle `RegisterNatives` (Java-driven explicit `JNINativeMethod` registration, bypassing name-mangling).
3. **Minimal `JNIEnv`/`JavaVM` + first end-to-end native call.** Construct the C-ABI `JavaVM_` (single instance, `main_vm` equivalent) and a per-thread `JNIEnv` (first slot = pointer to the `JNINativeInterface_` table). Populate just `GetVersion` + whatever the first test native needs. Call a native function pointer resolved in step 2 via an `unsafe` indirect call, passing `(JNIEnv*, jclass, args…)`. *Verifiable: `Hello.add(2,3)` → 5, where `Hello.add` is a `native` method backed by a tiny cdylib exporting `Java_com_example_Hello_add`.*
4. **`JNI_OnLoad`.** On library load, `dll_lookup("JNI_OnLoad")`; if present, invoke `JNI_OnLoad(JavaVM*, nullptr)` and honor its returned JNI version. *Verifiable: a lib with `JNI_OnLoad` printing the version on load.*
5. **Fill the `JNIEnv` table incrementally**, driven by what real libs (LWJGL) actually call. Order: local/global ref handle ops (`NewGlobalRef`/`DeleteGlobalRef`/`NewLocalRef`/`DeleteLocalRef`/`EnsureLocalCapacity`/`Push|PopLocalFrame`) → `FindClass` → `GetMethodID`/`GetFieldID`/`GetStaticMethodID`/`GetStaticFieldID` → `Get/Set<Type>Field` + `Get/SetStatic<Type>Field` → `Call<Type>Method{,V,A}` + `CallStatic<Type>Method{,V,A}` + `CallNonvirtual<Type>Method` → strings (`NewStringUTF`/`GetStringUTFChars`/`ReleaseStringUTFChars`/`NewString`/`GetStringChars`/…) → arrays (`New<Type>Array`/`Get<Type>ArrayElements`/`Release`/`Get/Set<Type>ArrayRegion`) → `RegisterNatives`/`UnregisterNatives` → `MonitorEnter`/`Exit` → `Throw`/`ThrowNew`/`Exception*`. Each is an independent, testable bridge.
6. **Thread attach/detach** (`AttachCurrentThread`/`DetachCurrentThread`/`GetEnv`) + `JNI_OnUnload`, for native-spawned threads and clean unload.

---

## (c) `jni/` designated-unsafe module structure

```
jni/
  mod.rs                 # safe façade: load_library / find_entry types
  loader.rs              # #[allow(unsafe_code)] dll_load/dll_lookup/dll_unload via libloading
                         #   (Windows: LoadLibraryA/GetProcAddress/FreeLibrary under the hood)
  lookup.rs              # nativeLookup port: name-mangling (short/long) + per-class lib registry
                         #   + RegisterNatives table — algorithmically safe (string building + table);
                         #     calls loader.rs for the actual dlsym
  env.rs                 # JNIEnv/JavaVM C-ABI tables (jni-sys types). The jni_* function impls
                         #   are SAFE Rust bridging into the VM (FindClass→registry, Call*Method→
                         #   interpreter invoke, NewGlobalRef→handle table, …). The table itself is
                         #   just a static struct of fn pointers — safe to build.
  invoke.rs              # #[allow(unsafe_code)] THE indirect-call seam: take a resolved `address`
                         #   (void*), transmute to the right fn signature, call it with (JNIEnv*,
                         #   jclass, args…). This + loader.rs are the only irreducibly unsafe parts.
  refs.rs                # local/global ref handle layer (maps jobject <-> rustj u32 handle +
                         #   refcounts). Safe (table + RefCell), sits atop the existing heap.
  direct.rs              # #[allow(unsafe_code)] GetDirectBufferAddress / Get<Type>ArrayElements
                         #   "direct" paths that expose raw heap pointers — needs contiguous backing
                         #   (couples loosely to #6a; the copy/region paths don't)
```

**Unsafe surface is tight:** `loader.rs` (dlopen/dlsym FFI), `invoke.rs` (calling a `void*` as a fn), and `direct.rs` (raw pointer exposure). Everything else — name-mangling, the `jni_*` bridges, the ref handle layer — is safe Rust over existing VM capabilities.

---

## (d) Smallest first increment

**Load a tiny native cdylib and call one exported `Java_…` symbol end-to-end, with a near-empty `JNIEnv`.**

- Build a Rust cdylib (or C DLL) exporting `JNIEXPORT jint JNICALL Java_com_example_Hello_add(JNIEnv*, jclass, jint a, jint b)` that returns `a+b`.
- Java: `class com.example.Hello { static native int add(int a, int b); static { System.load("…hello.dll"); } }`.
- rustj: `System.load` → `JVM_LoadLibrary` → `libloading` dlopen (handle cached). First call to `Hello.add` → native-dispatch miss → `nativeLookup` short-name mangling → `dll_lookup` → cache address → `invoke.rs` indirect call with a minimal `JNIEnv*` + `jclass` + the two `jint`s → return `5`.
- The native function ignores `JNIEnv`, so this works before most of the table is populated. **Verification:** integration test asserts `Hello.add(2,3)==5` and that the same method routed through the existing compile-time `NativeRegistry` (for a hand-ported native) still works — i.e. real-JNI and hand-port coexist.

**Do NOT start with:** `JNI_OnLoad`, `RegisterNatives`, strings, arrays, `Call*Method` (callback into Java), or thread attach. Each adds table entries + marshalling; add after the leaf-symbol call is green, per the phase-5 order.

---

## (e) Honest work estimate

- **Loader bridge + nativeLookup + dispatch miss-path hook (phases 1-2):** days to ~2 weeks. Small, well-bounded; mostly string mangling + wiring into the existing `NativeRegistry`/`NativeDispatch`.
- **Minimal JNIEnv + first native call (phase 3):** ~1-2 weeks. The C-ABI struct layout (`jni-sys` removes transcription risk) + the one indirect-call seam + handle↔jobject marshalling.
- **`JNIEnv` table to LWJGL-usable subset (phase 5):** a few months. ~230 entries, but each is a thin, mechanical bridge onto existing VM ops (class registry, method/field resolution, interpreter invoke, handle table). Low difficulty, high volume. Driven by actually running LWJGL and filling what it calls.
- **Thread attach + OnLoad/OnUnload + direct-buffer paths (phases 4, 6):** weeks. Attach couples to `ThreadManager`; direct-buffer/array-element raw pointers couple loosely to #6a (copy/region paths don't).

**Key architectural finding (favorable):** JNI hands native code **opaque `jobject` handles**, never raw object internals. Native code reaches the VM *through* `JNIEnv` functions, not by dereferencing pointers at field offsets. Therefore **JNI composes with the current u32-handle + `Vec<Oop>` heap** and does **not** need the #6a native object-layout rework that the JIT (#6) requires. The only #6a-coupled bits are the "direct" paths (`GetDirectBufferAddress`, `Get<Type>ArrayElements` returning raw pointers), which can initially be served by copy/region variants. → **Safe to sequence JNI before or parallel to #6a.**

---

## Self-roll vs `jni` crate — decision

- **The `jni` crate is the wrong direction.** It provides a `JNIEnv` for *Rust code to call into a JVM* (Rust as a native lib hosted by a real JVM). rustj **is** the JVM — it must *expose* a `JNIEnv`/`JavaVM` to native libs. Not a fit.
- **Self-roll the function implementations** (all the `jni_*` bridges) — they're rustj-specific by nature.
- **Use `jni-sys` for the C type layout** (`JNINativeInterface_`, `JNIInvokeInterface_`, `JavaVM_`, `jobject`/`jint`/`jstring`/`jarray` typedefs). `jni-sys` is a `-sys` FFI crate — pure type definitions, no logic, no direction. It removes the error-prone hand-transcription of the ~230-entry struct layout and the platform ABI details. Contingent on T1's revised §2 (it's a "necessary" dep).
- **Use `libloading` for dlopen/dlsym** (the `os::dll_load`/`dll_lookup` equivalent). Also contingent on revised §2. Avoids hand-rolling Windows `LoadLibrary`/`GetProcAddress` + POSIX `dlopen`/`dlsym` twice.

→ **`jni-sys` (types) + `libloading` (loader) + self-implemented `jni_*` functions.** All unsafe FFI confined to `jni/loader.rs` + `jni/invoke.rs` + `jni/direct.rs` (`#![allow(unsafe_code)]`).

---

## Key file:line anchors

- JNIEnv table: `prims/jni.cpp:3164` (`JNINativeInterface_ jni_NativeInterface`); accessor `:3513` `jni_functions()` / `:3521` `jni_functions_nocheck()`; override `:3464` `copy_jni_function_table`.
- JavaVM table: `prims/jni.cpp:4065` (`JNIInvokeInterface_ jni_InvokeInterface`); `main_vm` `:3134`/`:3549`; `JNI_CreateJavaVM_inner` `:3583`; `DestroyJavaVM` `:3740`.
- Library bridge: `prims/jvm.cpp:3150` `JVM_LoadLibrary`→`:3156` `os::dll_load`; `:3182` `JVM_UnloadLibrary`→`os::dll_unload`; `:3188` `JVM_FindLibraryEntry`→`:3189` `os::dll_lookup`; `ThreadToNativeFromVM` `:3155`; `UnsatisfiedLinkError` `:3168`.
- Native resolution: `prims/nativeLookup.cpp:409` `lookup`→`:380` `lookup_base`→`:308` `lookup_entry` (short :324 / long :336)→`:242` `lookup_style`; system-lib path `:256` `os::dll_lookup(os::native_java_library(), …)`; per-class-lib path `:288`; prefixed `:345`.
- OS loader abstraction: `runtime/os.cpp` + `os/windows/os_windows.cpp` (`os::dll_load`/`dll_lookup`/`dll_unload`).
- rustj integration points: existing `NativeRegistry` (fn-pointer table, `a562781` refactor) — extend its miss path; `ThreadManager` (Phase B) — for `AttachCurrentThread`; u32-handle heap (`src/runtime/heap.rs`) — `jobject`↔handle mapping (no #6a needed).
