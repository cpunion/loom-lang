# Versioning and compatibility

Loom versions each boundary according to what it protects. Toolchain, language,
artifact, cache, registry, and runtime versions are deliberately independent.

## Current versions

| Boundary | Current version |
| --- | --- |
| Cargo toolchain packages | `0.1.0` |
| Loom language | `0.3` |
| Manifest schema | `1` |
| Lockfile schema | `1` |
| Registry protocol/bundle | `1` |
| Interpreted MIR artifact | format `loom.interpreted-mir`, version `24` |
| Portable library artifact | source-package version `2` |
| Persistent compiler cache | schema `4` |
| Portable-library final-cache layer | `portable-library-artifact-v3` |
| LCIR textual dump | version `33` |
| LCIR artifact identity | schema `34` |
| LCIR native-object domain | `loom-lcir-native-object-v30` |
| Legacy native-object domain | `loom-legacy-native-object-v5` |
| LLVM object-cache domain | `loom-llvm-object-cache-v35` |
| Controlled quality evidence | schema `2` |
| Runtime bundle manifest | schema `2` |
| Native runtime ABI component | `22` |
| Coroutine/Task ABI component | `2` |
| Typed Task ABI component | `1` |
| Wait ABI component | `1` |
| Standard-library ABI component | `4` |

The exact compiler-private native ABI identity contains additional layout,
text, shadow-stack, witness, list, and runtime component versions. Runtime
bundles compare the whole identity, not only the numeric runtime component.

Compiler-generated native harness output adds the exact-length
`loom_runtime_stdout_write_v1(data, length)` boundary and advances the current
native runtime ABI identity to component 20 with `stdout-v1` and
`runtime-v14`. The byte range includes any intended literal LF and bypasses C
runtime text-mode translation; the runtime does not scan for NUL or append a
delimiter. Success means that the complete range was accepted and flushed.
Failure may follow a partial write, so generated callers never retry. Pure
LCIR objects may reference this output-only symbol without acquiring execution
runtime or executor requirements. LCIR serialization and artifact schema do not
change because harness output is not part of LCIR. Native-object and
object-cache domains do not advance because native fingerprints and
runtime-bundle checks already include the exact runtime ABI identity.

Typed structured logging adds the synchronous, non-collecting
`loom_runtime_log_typed_v1(level, message, fields, count)` boundary. Generated
code passes direct Text object pointers and the canonical contiguous
`TextMap[Text]` entry view; no universal value, runtime type tag, scheduler, or
Loom GC safepoint crosses the call. Runtime status `2` becomes the canonical
`LogWriteFault` and follows LCIR cleanup edges, while invalid or unknown ABI
statuses trap as compiler/runtime defects. This advances the runtime identity
to component 21 with `typed-log-v1` and `runtime-v15`, while the public
standard-library ABI remains v4. The new `LogWrite` terminator advances the
LCIR dump to 31, artifact schema to 32, native-object domain to
`loom-lcir-native-object-v28`, and object-cache domain to
`loom-llvm-object-cache-v33`.

Typed JSON formatting adds the collecting
`loom_runtime_json_format_typed_v1(json, layout, out_text)` boundary. Generated
code passes the exact recursive `Json`, `List[Json]`, and `TextMap[Json]`
target layout in a compiler-private descriptor; neither a universal value nor
an executor crosses the call. The runtime validates that descriptor, requires
the TextMap's canonical strictly increasing UTF-8 key order, and stages the
complete JSON text before its sole managed Text allocation. Depth-limit and
non-finite-number statuses become ordinary `JsonError` variants; malformed ABI
inputs and unknown statuses trap as compiler/runtime defects. This advances
the runtime identity to component 22 with `typed-json-v1` and `runtime-v16`.
The new `JsonFormat` instruction advances the LCIR dump to 32, artifact schema
to 33, native-object domain to `loom-lcir-native-object-v29`, and object-cache
domain to `loom-llvm-object-cache-v34`. The public standard-library ABI remains
v4.

Compiler-generated `StructuralEquality` instances subsequently close recursive
List/TextMap-backed value equality through finite, type-specialized direct-call
cycles. Because the new instance role participates in canonical callable
identity, this advances the LCIR dump to 33, artifact schema to 34,
native-object domain to `loom-lcir-native-object-v30`, and object-cache domain
to `loom-llvm-object-cache-v35`. It adds no runtime ABI or LCIR instruction.

Task composition and timer calls now remain ordinary HIR calls until semantic
resolution selects a stable compiler-owned `StandardLibraryItem`. The item is
serialized in typed body facts, so the persistent compiler cache advances to
schema 4 and domain `loom-compilation-cache-v4`; the embedded standard-library
identity advanced at that stage to `loom-embedded-builtins-v3`. Older cache
entries are misses, not upgraded. Checked MIR, LCIR, native objects, and runtime
ABI remain unchanged because MIR retains the established `Sleep` and
`TaskJoin` forms.

The source-backed standard-library foundation supersedes that historical
builtin identity. The current identity is
`loom-source-stdlib-v1/<sha256>` and covers the language version plus the
ordered path, module name, and exact bytes of every compiler-distributed Loom
source file. It is part of compiler-cache identity; a changed source library is
therefore rechecked instead of reusing a builtin-era cache entry.

Controlled quality evidence schema 2 adds an ordered native-route record for
every prepared object. Each record carries the scenario, expected and actual
route, and the optional named legacy allowance. This changes evidence consumers
only: route policy does not alter a selected object's LCIR artifact, object
identity domain, cache key format, or native runtime ABI.

Admitting canonical List and compiler-private TextMap managed pointers in typed
coroutine frames changes classifier, validator, and debug-type coverage. It
reuses the existing LCIR `ManagedPointer` representation,
repeated-allocation descriptors, coroutine frame layout, typed shadow roots,
and typed-task ABI. No serialized shape changes, so the current dump, artifact,
native-object, cache, runtime bundle, and runtime ABI versions remain unchanged.
Compiler/backend build fingerprints still invalidate objects produced by an
older implementation.

Interpreted MIR version 21 replaces the lossy synthesized `Let` plus `Defer`
encoding of `scoped` with an atomic scoped-initialization and disposal record.
Version 20 is intentionally rejected rather than inferred or upgraded.

Interpreted MIR version 22 carries each concept's source-module provenance and
the compiler-known identity of the canonical `standard.resource.MustScope`
marker. Validation independently requires the qualified concept, identity tag,
prelude id, dense concept id, and empty non-dynamic marker shape to agree.
Missing, redirected, duplicated, or inconsistent metadata fails closed before
execution. Persistent cache schema 3 invalidates older semantic and checked-MIR
entries; cached typed body facts never supply this identity, which semantic
analysis rederives from the current HIR. The portable-library envelope remains
independently versioned. Its current version 2 contains no checked MIR, so MIR
version changes do not advance it. No source syntax, resource lifetime rule,
LCIR, native ABI, or runtime boundary changes in version 22.

Interpreted MIR version 23 makes the compiler-private `ConstraintError` record
shape explicit and validator-enforced. Older artifacts are rejected because
their synthetic prelude record could omit the six structured fields. The
persistent cache schema remains 3: cached checked-MIR envelopes already carry
and validate the interpreted-artifact version, while semantic cache payloads
do not encode this synthetic lowering shape.

Interpreted MIR version 24 adds the checked `TextMapEntryAt` builtin for the
source-level `TextMap.entry_at(index)` operation. Older artifacts are rejected
because the serialized builtin enum has changed. The operation reuses LCIR's
existing typed `TextMapEntryGet` instruction and the established TextMap
layout, so LCIR, native-object, runtime, and cache schema versions do not
advance.

Portable library version 2 replaces version 1's checked-MIR payload with a
source-and-interface package envelope. It stores the resolved non-standard
package graph, exact source text, language version, and canonical public
interfaces. Decoding validates structural and byte/count bounds, identities,
graph closure, portable paths, and public-interface fingerprints recomputed
from source. It stores neither producer-local proof state nor the
compiler-owned standard-library implementation. Consumers supply the matching
compiler-distributed standard library and repeat parsing, type checking, proof
search, lowering, and MIR validation. Version 1 is rejected and must be
rebuilt. The derived final-artifact cache layer advances to
`portable-library-artifact-v3` and binds the source-package format/version;
`.loomi` version 24, persistent compiler-cache schema 4, and the checked-MIR
cache envelope are unchanged by this library-format replacement.

Nongeneric `.loomi` proof replay advances the LCIR dump to 18, artifact schema
to 19, native-object domain to v15, and LLVM object-cache domain to v20. The
checked predicate is explicit typed CFG, rejection raises the canonical
`ArtifactProofRejected` `RuntimeFault`, and the established nominal value is
created only on the accepted edge. This reuses the existing fault-context ABI
and changes no runtime component.

Concrete generic invariant-record proof replay later reuses that same LCIR
assertion and protected product construction. It changes route coverage rather
than representation or serialization: exact record arguments, substituted
field types, and substituted lexical contract bindings are already present in
the existing checked MIR and LCIR identities. The LCIR dump, artifact schema,
native-object domain, object-cache domain, and runtime ABI therefore do not
advance again.

Synchronous Task-producing helpers likewise reuse existing `NEEDS_EXECUTOR`,
`TaskCreate`, `TaskJoinAll`, and `TaskSleep` identities. The exact transitive
effect already participates in the canonical LCIR artifact identity, and the
LLVM backend build fingerprint covers the derived hidden executor parameter
and forwarding implementation. Older validators could not produce a checked
synchronous executor-dependent LCIR object, so no previously valid callable
ABI can collide. The LCIR dump remains 31, artifact schema 32, native-object
domain v28, object-cache domain v33, and native runtime ABI component 21.

Typed scalar builtins advance the LCIR dump to 19, artifact schema to 20,
native-object domain to v16, and LLVM object-cache domain to v21. `ParseInt`
and `ParseFloat` reuse their existing closed status ABI; `IsFinite` and
`Duration` lower to typed comparisons, products, and canonical runtime-fault
assertions. `FormatFloat` adds
`loom_runtime_format_float_typed_v1(value, out_cell)`, advancing the native
runtime ABI component to 15 with `format-float-v1` and `runtime-v9`. The Text
layout remains `text-v3`, and collection remains `gc-v9`.

Typed TextMap and the first checked stackless-coroutine slice advance the LCIR
dump to 20, artifact schema to 21, native-object domain to v17, and LLVM
object-cache domain to v22. The dump now records Task handles,
`task.create`/`task.await`, and every coroutine output/resume/live-row type, so
target-laid-out frame and root-map changes invalidate cached objects. TextMap
reuses `typed-repeated-v1`; typed coroutines reuse typed-task v1 and the
existing scheduler/join ABI. Native runtime component 15, `runtime-v9`,
`text-v3`, and `gc-v9` remain unchanged.

Artifact-closed finite dynamic catalogs then advance the LCIR dump to 21,
artifact schema to 22, native-object domain to v18, and LLVM object-cache
domain to v23. Candidate order, exact payload types, managed descriptors,
construction, and direct finite dispatch are therefore cache identity inputs.
The existing typed fixed-object allocator is reused, so native runtime
component 15, `runtime-v9`, and `gc-v9` remain unchanged.

LCIR's literal-only direct `Text` representation first added the physical
pointer ABI and allocation-free operations. The subsequent typed moving-GC
foundation advanced the native runtime component to `9`, GC identity to
`gc-v8`, and added `typed-gc-v1` plus `typed-shadow-stack-v1` identity
components without emitting those calls from LCIR.

Dynamic `Text.concat` makes those typed root facilities part of generated LCIR.
An artifact containing concat or a Text-bearing tuple/record uses one
`ManagedPointer` provenance mode for all Text. Products remain unboxed exact
SSA aggregates; only their deterministic Text leaves receive stable root cells
when the aggregate is live after a collecting operation. This advanced the
LCIR dump to 12, artifact schema to 13, native-object domain to v9, and
object-cache domain to v14. The concat helper previously advanced the native
runtime component to `10` and the exact identity components to `text-v2` and
`runtime-v4`; product leaf rooting reuses that typed-shadow-stack v1 wire and
does not advance the runtime ABI. The collector remains `gc-v8`.

The typed Task runtime foundation adds `typed-task-v1` beside the retained
legacy `task-v2` identity and advances the runtime component to `runtime-v5`
(`11`). Typed descriptors, stable coroutine frames, precise suspended/result
root rows, cancellation, result transfer, and deterministic result disposal
cross this boundary without a universal value payload. Source and LCIR do not
emit these symbols yet. Typed Task management calls use their own
`TYPED_TASK_*` operation-status domain; coroutine callbacks independently
return `TASK_*` scheduler steps, and cancellation/disposal callbacks may not
return `TASK_PENDING`. Cleanup runs newest-child-first and cannot create or
publish work, mutate joins, re-enter the scheduler, or register/suspend a wait.
Nested callbacks preserve the caller's independent legacy and typed root-chain
baselines. An established cancellation remains primary over a well-formed
cleanup RuntimeFault; invalid callback statuses, missing fault records, and
scheduler-topology violations remain runtime defects and are never laundered
into cancellation.

Direct lexical cleanup advances the LCIR dump to 13, artifact schema to 14,
native-object domain to v10, and object-cache domain to v15. These boundaries
encode assertion cleanup edges, compiler-expanded LIFO cleanup suffixes, and
typed resource-close control flow. Static-concept disposal and deferred blocks
use existing typed calls and add no runtime ABI. Canonical File/Socket disposal
adds `typed-resource-v1` and advances the native runtime component to 12 with
`runtime-v6`. The close helper receives one exact handle cell; it neither
constructs a universal value nor drives an executor.

Managed Text leaves in closed unboxed sums advance the LCIR dump to 14,
artifact schema to 15, native-object domain to v11, and object-cache domain to
v16. The typed root plan catalogs deterministic variant candidates and guards
publication and reconstruction with exact runtime tags. It reuses the existing
typed-shadow-stack v1 wire, so runtime ABI component 13, `runtime-v7`, and
`gc-v9` remain unchanged.

Typed `Text.get` first adds the direct
`loom_runtime_text_get_typed_v1(text, scalar_index, out_cell)` boundary and
advances native runtime ABI component 14, Text identity to `text-v3`, and
runtime identity to `runtime-v8`; GC remains `gc-v9`. Its typed LCIR consumer
then advances the LCIR dump to 15, artifact schema to 16, native-object domain
to v12, and object-cache domain to v17. The instruction returns a canonical
managed `Option[Text]`, treats missing indices as nonallocating, publishes
exact live-after roots before a found-value allocation, and traps on invalid
runtime status without introducing another ABI change.

The transitive LCIR effect lattice adds explicit runtime, collection,
executor, and suspension capability identity. Current typed source lowering
emits effect-free and `MAY_FAULT` functions, and dynamic Text concat/get emits
the exact `MAY_COLLECT | NEEDS_RUNTIME` capability pair. It still emits no
executor or suspension operation. The lattice's canonical encoding advanced
the four compiler-private LCIR and object-cache boundaries; the Text runtime
boundaries are versioned separately above.

Typed source-contract placement advances the LCIR dump to 16, artifact schema
to 17, native-object domain to v13, and object-cache domain to v18. The encoded
meaning now distinguishes checked-root wrappers from assumed bodies, places
synchronous preconditions at concrete calls, checks receiver invariants at body entry, and
checks current receiver invariants and postconditions only after lexical
cleanup. Checked contract arithmetic and its original runtime faults use
existing control flow. This changes no physical value ABI, runtime symbol, or
native runtime ABI component.

Monomorphized managed List lowering advances the LCIR dump to 17, artifact
schema to 18, native-object domain to v14, and object-cache domain to v19.
Concrete closed `List[T]` values use one direct managed pointer and exact
target-data-derived repeated descriptors. `ListAppendUnique` additionally
encodes an independently checked ownership certificate for capacity reuse.
The implementation consumes the existing `typed-repeated-v1` allocator and
typed shadow stack, so native runtime ABI component 14, `runtime-v8`, and
`gc-v9` do not change.

Collision-free closed-sum carrier planning adds no opcode or runtime entry
point. The checked representation graph already identifies each exact sum;
the LLVM target-data plan now assigns variant offsets by pointer/non-pointer
byte class so repeated descriptors remain precise. Target-specific carrier
offsets remain emitter-private, but the compiler-private boundary advances
monotonically from finite dynamic catalogs: LCIR dump 22, artifact schema 23,
native-object domain v19, and LLVM object-cache domain v24 prevent a checked
artifact or native object planned with the old overlapping carrier from
sharing the corrected domain. Existing `typed-repeated-v1`, typed shadow-stack,
Text, and GC wires are reused, so native runtime ABI component 15 does not
change. Canonical Json is admitted through its List/TextMap cycle breakers and
remains 24 bytes on supported 64-bit targets. Json equality and typed
parse/format are separate versioning decisions.

Managed closed sums in checked coroutine frames and typed Task results advance
the LCIR dump to 23, artifact schema to 24, native-object domain to v20, and
LLVM object-cache domain to v25. Coroutine descriptors reuse the generic
carrier's exact static pointer offsets and per-state bitmaps; inactive pointer
lanes remain zero. Fallible completion still publishes an ordinary typed
`Result`, while Task fault and cancellation remain distinct scheduler states.
No typed-task or native runtime ABI component changes.

Typed TextMap containment, functional removal, indexed entry comparison, and
structural equality advance the LCIR dump to 24, artifact schema to 25,
native-object domain to v21, and LLVM object-cache domain to v26. Removal
reuses `typed-repeated-v1`; containment, lookup, and equality allocate no map
storage. No runtime symbol or universal value is added, so native runtime ABI
component 15, `runtime-v9`, and `gc-v9` remain unchanged.

Checked typed `Task.sleep` advances the LCIR dump to 25, artifact schema to 26,
native-object domain to v22, and LLVM object-cache domain to v27. The artifact
records the normalized millisecond operand and explicit normal/fault control
flow. The additive `loom_typed_timer_task_create_v1(executor, deadline_ns)`
factory creates a zero-root typed `Task[Unit]` over the established reactor and
advances the native runtime ABI component to 16 with `typed-timer-v1` and
`runtime-v10`. The existing `typed-task-v1`, `wait-v1`, `text-v3`, and `gc-v9`
components remain unchanged.

Static heterogeneous `Task.all` advances the LCIR dump to 26, artifact schema
to 27, native-object domain to v23, and LLVM object-cache domain to v28. A
directly awaited fixed tuple or `Task.all` carries all exact child types in one
`AwaitTasks` suspension. A stored `Task.all` value uses an exact typed
composite task and advances the native runtime ABI component to 17 with
`typed-task-adopt-v1` and `runtime-v11`. The adoption operation validates the
complete transfer before atomically moving the ordered children from their
active parent to the composite. It adds no universal result slot, runtime type
tag, or change to `typed-task-v1`, `wait-v1`, `gc-v9`, or `typed-timer-v1`.

Static cleanup and cancellation exits across `AwaitTasks` advance the LCIR dump
to 27, artifact schema to 28, native-object domain to v24, and LLVM object-cache
domain to v29. Each suspension now identifies explicit normal, fault, and
cancel targets over one identical exact live row, and the artifact records the
coroutine-only `TaskCancelled` terminal. Source coroutine descriptors reuse the
generated resume callback as their cancel callback and dispatch by cancellation
request plus frame state. Cleanup remains compiler-expanded LIFO CFG; no runtime
cleanup stack or new runtime entry point is added. Native runtime component 17
and `runtime-v11` therefore remain unchanged for that slice.

Nonempty, immediately awaited, fixed-arity homogeneous `Task.any` advances the
LCIR dump to 28, artifact schema to 29, native-object domain to v25, and LLVM
object-cache domain to v30. Each suspension records an explicit join mode and
the complete exact child-output row even though `any` has one normal result. The
runtime keeps the original winner ordinal while generated code selects the
corresponding static coroutine-frame field. Consuming the existing join-step
entry now finalizes typed losers exactly once before exposing the step, so no new
symbol or descriptor wire is required. That semantic boundary advances the
native runtime component to 18 and adds `typed-task-any-finalize-v1` plus
`runtime-v12` to the exact identity. Coroutine v2, typed-task v1, wait v1, and GC
v9 remain unchanged.

Static `Task.settled` and `Task.race`, specialization of a sole nonempty Task
List literal, and explicit terminal outcome transfer advance the LCIR dump to
29, artifact schema to 30, native-object domain to v26, and LLVM object-cache
domain to v31. `AwaitTasks` now distinguishes all four private join modes;
`settled` and `race` inject affine terminal handles that a validated
`TaskOutcomeTake` consumes into canonical `TaskOutcome[T]` values. The exact
artifact records those mode-specific continuation types and the collecting
outcome operations.

The additive `loom_typed_task_take_outcome_v1` boundary moves an exact completed
value, publishes independently rooted managed Text for the primary fault code
and message, or reports payload-free cancellation before retiring the child.
Winner finalization is generalized across `any` and `race`. These changes
advance the native runtime component to 19 and replace the previous specialized
identity with `typed-task-winner-finalize-v1`, `typed-task-outcome-v1`, and
`runtime-v13`. Typed-task v1, coroutine v2, wait v1, Text v3, and GC v9 remain
unchanged. No universal value, source-visible runtime tag, or language syntax is
added; the public join policies remain standard-library APIs.

Async state-zero preconditions and creation-site blame advance the LCIR dump to
30, artifact schema to 31, native-object domain to v27, and LLVM object-cache
domain to v32. A checked coroutine plan now records whether its compiler-shaped
frame carries the creating call's file/start/end coordinates. Contract metadata
distinguishes that dynamic blame source from a static span. `TaskCreate` supplies
the coordinates but does not inherit the child coroutine's `MAY_FAULT`; an async
root receives its declaration span from the harness. The existing context-fault
entry points and JSON wire schema are reused, so native runtime ABI component
19, `runtime-v13`, typed-task v1, coroutine v2, wait v1, Text v3, and GC v9 do
not change.

Structural equality adds no LCIR opcode or runtime entry point. Products,
refined values, sums, and finite List-backed graphs expand into the existing
typed comparisons, product extraction, sum switches, List length/get, proved
successor, branches, and block parameters. Artifact identity already includes
that complete generated CFG. The LCIR dump remains 17, artifact schema 18,
native-object domain v14, object-cache domain v19, and native runtime ABI
component 14.

Concrete static-concept resolution adds no physical ABI. The compiler-private
domains advance because the planner now normalizes associated projections,
closes witness-method edges, and rejects open type or proof arguments before a
checked LCIR program can be emitted.

Closed-world unique-witness dynamic-concept erasure also adds no physical ABI
or LCIR opcode. A proved view reuses its concrete registered value type and an
existing direct-call instance; mutable parameters reuse existing typed inout
writebacks. Recursive erasure through products, sums, and Lists selects only
already-versioned concrete aggregate types and repeated descriptors. Because
no runtime witness, tag, indirect-call surface, or artifact format is added,
LCIR dump 19, artifact schema 20, native-object domain v16, object-cache domain
v21, and native runtime ABI component 15 remain unchanged.

Artifact-closed finite dynamic catalogs advance the LCIR dump to 21, artifact
schema to 22, native-object domain to v18, and LLVM object-cache domain to
v23. A catalog with two or more exact closed nongeneric conformances records
its deterministic candidate order in checked LCIR. Each value is one managed
pointer to a candidate-specific box containing a compiler-private ordinal tag
and the exact concrete payload. The backend emits one precise fixed-object
descriptor per candidate and a finite tag switch whose arms make ordinary
direct calls. Mutable calls return the updated concrete receiver and allocate
a fresh box on both normal and fault writeback paths. This reuses
`loom_gc_typed_alloc_v1`; the native runtime ABI component remains 15. The
catalog is compiler-private and does not define a cross-artifact dynamic ABI.

## Toolchain releases

The repository uses SemVer-shaped Cargo versions. While the toolchain is below
`1.0.0`, minor releases may contain source or artifact incompatibilities.
Release notes must call out:

- language behavior and diagnostic changes;
- manifest, lockfile, registry, artifact, or cache version changes;
- native runtime ABI changes;
- supported/removed CI and release platforms;
- migration or regeneration steps.

A Git tag does not by itself create support for a platform; only archives
successfully produced and checked by the release workflow are release
artifacts.

## Source language

`language = "0.3"` selects the one source language accepted by the current
compiler. Unknown or older language versions are rejected; the compiler does
not silently reinterpret them as the current language.

The manifest currently defaults an omitted `language` to the current version.
New manifests should write the value explicitly so a future compiler cannot
silently change project intent.

A future language version needs explicit compatibility or migration rules. It
must not be inferred from the toolchain package version.

## Format mismatch behavior

| Boundary | Incompatible input behavior |
| --- | --- |
| Manifest or lockfile | Reject with a configuration/version diagnostic. |
| Registry index or bundle | Reject; never use package bytes under the wrong protocol/language identity. |
| `.loomi` | Reject before execution, then run complete MIR validation for matching versions. |
| `.loomlib` | Reject before import; for matching version 2, validate bounded source-package structure and recomputed interface fingerprints before normal source compilation. |
| Compiler cache | Treat as a miss; versioned roots prevent accidental reuse. |
| Runtime bundle | Reject before linking on schema, target, data layout, ABI, digest, or tree mismatch. |
| Native executable/object | Target-specific and not promised compatible with another runtime or toolchain. |

Never “fix” incompatibility by editing an envelope version or checksum.
Regenerate the artifact with the intended compiler and review any source or
dependency migration.

MIR version `19` makes process-local construction proofs non-portable. A
matching `.loomi` replays each serialized proof. The local compiler cache and a
version 2 `.loomlib` consumer instead rebuild proof-bearing semantic and MIR
layers from source so a warm or artifact-backed build derives its own proof
decisions.

MIR version `21` defines projected `Move` as an ownership transfer of the
selected leaf that consumes the complete root local. It deliberately does not
introduce partially initialized aggregates or a wire-compatible interpretation
for version `19` artifacts.

Typed repeated-element allocation adds `typed-repeated-v1`. Its descriptor
copies exact fixed-header and per-element pointer offsets and derives storage
from an allocator-supplied capacity; no mutable object field controls GC
tracing. This changes the collector identity to `gc-v9` and advances the native
runtime ABI component to 13 with `runtime-v7`. The fixed-offset
`typed-gc-v1` symbols remain unchanged.

The typed Text scalar-selection helper adds a direct pointer output and a
three-value found/missing/invalid status boundary without using the universal
envelope. It advances Text identity to `text-v3` and the native runtime ABI
component to 14 with `runtime-v8`; `gc-v9` and both typed allocation wires are
unchanged.

The typed Float formatter adds a direct managed-Text output cell and a closed
success/error status without using the universal envelope. It advances the
native runtime ABI component to 15 with `format-float-v1` and `runtime-v9`;
the established `text-v3` layout, `gc-v9`, and typed allocation wires remain
unchanged.

## Reproducibility and rollback

Commit `loom.lock` for reproducible applications. Build with `--locked` and the
same toolchain version used in CI. Preserve release archive checksums and the
toolchain version alongside long-lived artifacts.

To roll back, install a complete previous release archive and its matching
runtime bundle after verifying the published SHA-256. Do not mix `loomc`,
`loom-lsp`, caches, portable artifacts, or runtime bundles from unrelated
toolchain versions unless their versioned decoder explicitly accepts them.

The project currently provides no stable public native library or FFI ABI.
That boundary, if added, will require its own compatibility and deprecation
policy.
