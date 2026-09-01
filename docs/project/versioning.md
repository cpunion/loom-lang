# Versioning and compatibility

Loom versions each serialized or executable boundary independently. A number
belongs to the data or contract it protects; it is not a release-history
counter and must not be reused as a proxy for another boundary.

The project has not published a compatibility baseline. The current toolchain
accepts only its current source language and current artifact schemas. When an
internal format changes, the old format is invalidated and rebuilt; the
compiler does not carry aliases, tombstones, upgrade readers, or dual-format
writers for unreleased formats. Development history belongs in
[the changelog](../../CHANGELOG.md), not in this specification.

## Current versions

| Boundary | Current version or identity |
| --- | --- |
| Cargo toolchain packages | `0.1.0` |
| Loom language | `0.4` |
| Manifest schema | `2` |
| Lockfile schema | `2` |
| Registry protocol and bundle | `1` |
| Interpreted MIR artifact | `loom.interpreted-mir`, version `49` |
| Portable library artifact | `loom-library`, source-and-interface version `4` |
| Persistent compiler cache | schema `22` |
| Compilation-cache domain | `loom-compilation-cache-v22` |
| Interpreted final-cache layer | `final-artifact-v3` |
| Interpreted artifact writer | `loom-interpreted-artifact-writer-v3` |
| Portable-library final-cache layer | `portable-library-artifact-v4` |
| LCIR textual dump | `lcir 55` |
| LCIR artifact identity | schema `57` |
| LCIR native-object domain | `loom-lcir-native-object-v50` |
| LLVM object-cache domain | `loom-llvm-object-cache-v53` |
| Controlled quality evidence | schema `5` |
| Runtime bundle manifest | schema `2` |
| Native runtime ABI component | `40` |
| Shared Task ABI component | `2` |
| Typed Task ABI component | `1` |
| Typed I/O ABI component | `1` |
| Typed resource ABI component | `1` |
| Wait ABI component | `1` |
| Typed process ABI component | `1` |
| Standard-library ABI component | `10` |

The compiler-owned standard library uses the content identity
`loom-source-stdlib-v2/<sha256>`. The digest covers the Loom language version
and the ordered path, package name, and exact bytes of every distributed Loom
source file. A source-library change therefore invalidates dependent compiler
cache entries even when no public ABI component changes.

The complete compiler-private native runtime identity is:

```text
layout-v1/text-v4/wait-v1/task-v2/typed-task-v1/typed-task-adopt-v1/typed-task-winner-finalize-v1/typed-task-outcome-v1/typed-resource-ownership-v1/typed-timer-v1/typed-resource-v1/typed-io-v1/format-float-v1/typed-bytes-v2/typed-path-v1/typed-log-v1/stdout-v1/typed-process-v1/runtime-v34/gc-v9/typed-gc-v1/typed-repeated-v1/typed-shadow-stack-v1/stdlib-v10
```

Runtime bundles compare this entire identity, not only native runtime component
`40` or one subordinate ABI version. Runtime component `40` and standard-
library component `10` delete the retired `Text.from_utf8_units(List[Int])`
builtin and its typed runtime symbol. Ordinary source builds packed Bytes with
`Bytes.add` and converts them through `Bytes.decode_utf8`; the runtime identity
therefore has no `typed-text-units` component. Runtime component `39` and
standard-library component `9` previously added the checked `Bytes.add(Int)`
surface and its ordinary and unique packed-push boundaries. `typed-bytes-v2`
admits hidden ByteObject capacity, while `text-v4` admits a Text allocation
larger than its logical byte length after non-collecting UTF-8 decode relabels
valid ByteObject storage. Text-backed Bytes is never mutated. Runtime component
`38` and standard-library component `8` removed the typed JSON-formatting
descriptor and runtime entry point after `std.json.format_json` moved to
ordinary Loom source.
Runtime component `37` pinned deletion of the unreachable universal
`ValueSlot` heap and root chain, runtime witness arena, universal Task/value
operations, and Int-list implementation. The existing shared `task-v2`
join/fault operations (`loom_task_prepare_join`,
`loom_task_add_join_child`, `loom_task_suspend_join`, `loom_task_join_step`,
`loom_task_join_winner`, and `loom_task_report_fault`) and the typed Task, I/O,
resource, GC, and shadow-stack wires did not change. An older compiler or
runtime bundle is therefore rejected instead of crossing a removed symbol
boundary.

Interpreted MIR 49 removes the retired `IoErrorKind` and `IoErrorMessage`
accessor builtins. The record's two declared source fields now flow through
ordinary record construction, projection, equality, MIR, and LCIR lowering;
the compiler no longer injects hidden storage or protected-value rules.
Persistent compiler-cache schema 22 and its matching domain invalidate typed
state and MIR that retained the accessor builtin identities or protected-record
semantics.
The LCIR and runtime ABI versions are unchanged because the exact canonical
`{ kind IoErrorKind, message Text }` product layout and typed-I/O wire did not
change.

Interpreted MIR 48 removes the fixed Duration prelude slot and its construction
and inspection builtins; the same compiler slice removes the frontend's
Duration prelude identity. `std.time.Duration` is now an ordinary imported
source refined type over `Int`; its public function, method, constraint, and
refined-to-base conversion use general language mechanisms, and `Task.sleep`
accepts `Int` without a Duration-specific overload. MIR and LCIR therefore see
canonical `Int` milliseconds. MIR 48 also adds the independently
validated `Precondition { index }` refined-construction certificate: a direct
immutable parameter established by the exact retained `requires` expression
can remain check-free across artifact and cache boundaries without trusting a
process-local `Proven` marker. Persistent compiler-cache schema 21 and its
matching domain invalidate semantic and MIR entries that retained the builtin
identity or lacked that certificate. LCIR dump 55 and artifact identity schema
57 remove the Duration canonical-catalog field and its obsolete construction
fault; native-object domain 50 and LLVM object-cache domain 53 reject objects
prepared from the old catalog and lowering. The runtime ABI is unchanged
because the timer boundary already consumed only `Int` milliseconds.
The source language remains `0.4` because the project has no published
compatibility baseline and supports only this current definition; portable
library version 4 is unchanged because its source-and-interface envelope and
validation contract did not change, and consumer compilers recheck its source.

Interpreted MIR 47 removes the fixed Json/JsonError prelude slots and all
compiler-synthesized constructors and patterns; both enums are ordinary exact
`std.json` source definitions. Persistent compiler-cache schema 20 and its
matching domain invalidate typed state and MIR that retained the builtin
identities. LCIR dump 54 and artifact identity schema 56 remove their canonical
catalog fields; native-object domain 49 and LLVM object-cache domain 52 prevent
reuse of objects prepared from the old fixed catalog. The native runtime ABI is
unchanged because it never receives a Json value or source type identity.

Interpreted MIR 46 removes the `TextFromUtf8Units` builtin and rejects older
serialized enum sets. Persistent compiler-cache schema 19 and its matching
domain invalidate semantic and MIR entries that could retain that dispatch.
LCIR dump 53 and artifact identity schema 55 remove the dedicated instruction;
native-object domain 48 and LLVM object-cache domain 51 prevent reuse of
objects that called the removed runtime symbol. Runtime component 40,
`runtime-v34`, standard-library component 10, and the complete native identity
remove that symbol atomically.

Interpreted MIR 45 adds the exact mutable-receiver `BytesAdd` builtin and
rejects older serialized enum sets. Persistent compiler-cache schema 18 and its
matching domain invalidate semantic and MIR layers that predate that surface.
LCIR dump 52 and artifact identity schema 54 add checked `BytesPush`, the
independently validated `BytesPushUnique`, exact byte-range guard proofs, and
the zero-code `CollectionShare` COW alias boundary; native-object domain 47 and
LLVM object-cache domain 50 prevent reuse of objects with the older Bytes
effects, root plan, or runtime calls. `BytesDecodeUtf8` is now non-collecting
because a valid standalone ByteObject is relabelled Text without moving its
payload.

Interpreted MIR 44 removes the public-name `JsonFormat` builtin. Persistent
compiler-cache schema 17 and its matching domain reject typed-state or MIR
entries that predate ordinary source resolution. LCIR dump 51 and artifact
identity schema 53 remove the dedicated JSON-format instruction; native-object
domain 46 and LLVM object-cache domain 49 prevent reuse of objects that called
the removed runtime boundary. JSON formatting now closes through ordinary
source functions, matches, collections, Float formatting, and Text
construction.

Interpreted MIR 43 rejects postconditions that inspect current or `old`
Task-bearing inputs after the body may have transferred them, and defines
read-only receiver invariants as entry-only. LCIR native-object domain 45 pins
the sole native emitter. Its fingerprint already feeds the unchanged
compiler-wide LLVM object-cache key.

LCIR dump 50 and artifact identity schema 52 add atomic
`TaskCarrierProject` and `TaskCarrierUpdate` operations for accessing a
Task-free leaf inside a Task-bearing product. The native-object domain advances
to 45 because LLVM now lowers those paths as nested `extractvalue` and
`insertvalue` operations while preserving affine sibling ownership. The same
versions raise the checked list-construction guard to 65,536 elements; the
typed emitter allocates one backing object and emits iterative stores, so large
list literals no longer require another backend. No runtime ABI or physical
value layout changes.

Quality evidence schema 5 removes the former per-scenario route comparison.
Native preparation has only the typed LCIR backend, so each successful native
build gate is itself the nonredundant backend evidence.

LCIR dump 49 and artifact identity schema 51 add compiler-private
`TaskCarrierBorrow`, `UnrefineBorrow`, `ProductBorrow`, and `SumBorrowSwitch`
inspection for Task-bearing carriers. These operations preserve the original
affine owner and cannot feed a consuming LCIR boundary. No runtime ABI or
physical layout changes.

LCIR dump 48 and artifact identity schema 50 add the linear paired-sum switch
used by structural equality. Earlier identities cannot be reused for checked
artifacts whose sum dispatch CFG or implicit payload-edge contract predates
that terminator.

LCIR dump 47 and artifact identity schema 49 add the atomic structural-tuple
split operation. Earlier identities cannot be reused for checked artifacts
whose affine aggregate ownership depends on that operation.

LCIR artifact identity schema 48 records the exact runtime-width
`TaskJoinList` opcode and its mode-specific typed result contract. Older
identities are not reused for artifacts whose checked LCIR meaning predates
the affine `List[Task[T]]` carrier.

Interpreted MIR version 42 independently rejects infinite by-value nominal
cycles, source-impossible mutable parameter shapes, and mutation or moves that
bypass constrained-type predicates or record-invariant boundaries as part of
decoded-program validation. Only parameter zero of a synchronous mutable
receiver may be a mutable slot, async functions cannot carry receiver metadata,
and only that receiver may mutate through its own top-level invariant. Fresh
receiver-restoration markers carry process-local `Proven` authority; encoding
and decoding normalize them to `Recheck`, whose exact contract is rebuilt from
the nominal receiver type. The format also encodes exact ordinary
standard-library source identities for `IoError`, `File`, and `Socket`. Their
protected source records receive compiler-private MIR storage, while resource
cleanup carries only the selected source `Dispose.dispose` proof. Checked MIR
rejects direct construction and projection. LCIR, native objects, and the
runtime wire do not change because no valid value layout or operation contract
changed.

## Source language

`language = "0.4"` selects the only source language accepted by the current
compiler. A manifest that omits `language` currently selects that same version,
but generated and reviewed manifests should state it explicitly. An unknown or
different language version is rejected; it is never silently reinterpreted as
the current language.

The Cargo package version, source-language version, standard-library content
identity, and artifact versions are independent. Changing one does not imply
that the others changed.

## When a boundary must change

Version the narrowest boundary that makes stale data or code unsafe or
semantically incorrect. A single implementation change may affect several
boundaries, and every affected boundary must advance together.

Until the first published compatibility baseline, the sole current language
version may evolve in place: the compiler does not retain or concurrently
accept an older meaning under that version. After a baseline is published, an
incompatible source-language change advances the language boundary described
below.

| Boundary | Advance it when |
| --- | --- |
| Loom language | Accepted grammar, static meaning, observable execution semantics, or source-level proof and contract rules change incompatibly. |
| Manifest or lockfile schema | Fields, canonical encoding, defaults, validation, or the meaning of recorded dependency state changes. |
| Registry protocol/bundle | Request, index, source-bundle, authentication, checksum, or materialization semantics change. |
| Interpreted MIR artifact | The envelope, exact MIR field set, serialized enum set, required compiler-known identity, validation contract, or execution meaning changes. |
| Portable library artifact | Its module graph, source payload, public-interface encoding, bounds, or validation meaning changes. |
| Persistent cache schema | A persisted reference, envelope, payload, namespace, or trust check changes in a way that can make an existing entry unsafe to read or reuse. |
| Final-cache layer or writer identity | Artifact selection, closure, dense remapping, writing, or final-byte derivation changes even when the underlying artifact schema does not. |
| LCIR dump | Serialized LCIR syntax, field set, instruction set, type encoding, control-flow meaning, or validation contract changes. |
| LCIR artifact identity schema | Canonical identity encoding or any semantic input included in that identity changes. |
| LCIR native-object domain | The native emitter's input assumptions, lowering meaning, object format, or fingerprint changes. |
| LLVM object-cache domain | Object-cache key composition or compiler-wide native reuse policy changes. |
| Quality evidence schema | The machine-readable evidence record or its interpretation changes. |
| Runtime bundle schema | The manifest envelope, file-tree contract, target metadata, archive metadata, or linker-input representation changes. |
| Native runtime ABI identity | Any generated-code/runtime symbol, signature, layout, status domain, ownership rule, collection rule, or other cross-boundary semantic contract changes. |
| Standard-library ABI component | A compiler/runtime contract attributed to the standard-library ABI changes; ordinary source-byte changes are already covered by the source-library digest. |
| Cargo package version | A toolchain release is cut or its user-visible package contract changes under the [release policy](../contributing/releases.md). |

A semantic correction may require an identity change even when its physical
layout is unchanged. For example, altered ownership transfer or validation
rules must invalidate an object or runtime bundle that was produced under the
old meaning. Conversely, adding implementation detail that is absent from a
serialized form and from all cache inputs must not advance unrelated formats.

When a boundary advances:

1. update the authoritative constant or identity domain;
2. update cache and runtime identities that embed it;
3. add tests for current round trips and exact mismatch behavior;
4. update the current-version table and the boundary's reference document;
5. remove the superseded decoder, alias, slot, or special case rather than
   retaining a migration path.

## Strict mismatch behavior

| Boundary | Incompatible input behavior |
| --- | --- |
| Manifest or lockfile | Reject with a configuration or version diagnostic. |
| Registry index or bundle | Reject before module bytes are trusted or materialized. |
| `.loomi` | Reject format, artifact version, language version, or envelope-kind mismatch before execution; matching headers still undergo exact deserialization and complete MIR validation. |
| `.loomlib` | Reject format, artifact version, or language mismatch before import; matching input still undergoes bounded graph, path, identity, source, and interface validation. |
| Compiler cache | Treat the entry as a miss and recompute from authoritative inputs. |
| LCIR or native object | Do not reuse it outside its exact identity, target, and optimization domain. |
| Runtime bundle | Reject before linking on schema, tree shape, target, data layout, CPU policy, complete ABI identity, archive digest, or linker-input mismatch. |
| Native executable | Run only on its target platform with the runtime already linked into that artifact; no cross-toolchain ABI compatibility is promised. |

Matching a version number is necessary but not sufficient. Current `.loomi`
MIR requires every current field and rejects unknown fields. Current portable
libraries recompute their public interfaces from embedded source. Cache blobs
and registry bundles are digest-checked, and runtime bundles validate both the
manifest and materialized directory tree.

Digests and checksums establish integrity for the bytes under validation; they
do not authenticate a publisher or resist an attacker with the same local
filesystem authority. Registry authentication and the user's distribution
trust policy are separate boundaries. A registry cache record or sidecar is
never sufficient without rehashing the bundle and validating materialized
files.

Never make incompatible bytes appear valid by editing a version, identity, or
checksum. Rebuild them with the intended compiler. No current decoder upgrades
another Loom artifact schema.

## Reproducibility

Commit `loom.lock` for applications and use `--locked` in CI. A reproducible
build fixes at least:

- the complete Loom toolchain revision and Cargo package versions;
- the Loom language version and exact source tree;
- the committed lockfile and validated dependency artifacts;
- enabled features, selected target, optimization profile, and entry roots;
- the LLVM toolchain and target-machine policy for native output;
- the exact runtime bundle and its published SHA-256 when linking.

Preserve the producing toolchain and checksums with any long-lived artifact.
Otherwise preserve source and lock state and rebuild. Do not mix compiler,
language server, cache, portable artifact, native object, or runtime-bundle
components from unrelated toolchain builds merely because one numeric version
happens to match.

The project currently exposes no stable public native library or FFI ABI. Any
future public boundary requires an explicit specification, compatibility
policy, and independent version before users can rely on it.
