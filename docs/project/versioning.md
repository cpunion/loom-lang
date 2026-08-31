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
| Interpreted MIR artifact | `loom.interpreted-mir`, version `43` |
| Portable library artifact | `loom-library`, source-and-interface version `4` |
| Persistent compiler cache | schema `16` |
| Compilation-cache domain | `loom-compilation-cache-v16` |
| Interpreted final-cache layer | `final-artifact-v3` |
| Interpreted artifact writer | `loom-interpreted-artifact-writer-v3` |
| Portable-library final-cache layer | `portable-library-artifact-v4` |
| LCIR textual dump | `lcir 49` |
| LCIR artifact identity | schema `51` |
| LCIR artifact route | `typed-lcir-whole-artifact` |
| LCIR native-object domain | `loom-lcir-native-object-v44` |
| Checked-MIR native-object domain | `loom-checked-mir-native-object-v4` |
| LLVM object-cache domain | `loom-llvm-object-cache-v48` |
| Controlled quality evidence | schema `4` |
| Runtime bundle manifest | schema `2` |
| Native runtime ABI component | `36` |
| Coroutine ABI component | `2` |
| Typed Task ABI component | `1` |
| Typed I/O ABI component | `1` |
| Typed resource ABI component | `1` |
| Wait ABI component | `1` |
| Typed process ABI component | `1` |
| Standard-library ABI component | `7` |

The checked-MIR native-object domain identifies the checked-MIR LLVM route. It
is a compiler-private invalidation boundary, not an artifact compatibility
layer or a supported public ABI.

The compiler-owned standard library uses the content identity
`loom-source-stdlib-v2/<sha256>`. The digest covers the Loom language version
and the ordered path, package name, and exact bytes of every distributed Loom
source file. A source-library change therefore invalidates dependent compiler
cache entries even when no public ABI component changes.

The complete compiler-private native runtime identity is:

```text
loom-value-v2/layout-v1/text-v3/wait-v1/task-v2/typed-task-v1/typed-task-adopt-v1/typed-task-winner-finalize-v1/typed-task-outcome-v1/typed-resource-ownership-v1/typed-timer-v1/typed-resource-v1/typed-io-v1/format-float-v1/typed-bytes-v1/typed-text-units-v1/typed-path-v1/typed-json-v1/typed-log-v1/stdout-v1/typed-process-v1/runtime-v30/gc-v9/shadow-stack-v1/typed-gc-v1/typed-repeated-v1/typed-shadow-stack-v1/witness-v1/int-list-v1/stdlib-v7
```

Runtime bundles compare this entire identity, not only native runtime component
`36` or one subordinate ABI version. Runtime component `36` pins the removal of
the former universal File, Socket, and close entry points and their fixed
source nominal IDs. The existing `typed-io-v1` request/outcome wire and
`typed-resource-v1` close boundary did not change. An older compiler or runtime
bundle is therefore rejected instead of crossing the removed symbol boundary.

Interpreted MIR 43 rejects postconditions that inspect current or `old`
Task-bearing inputs after the body may have transferred them, and defines
read-only receiver invariants as entry-only. LCIR native-object domain 44 and
checked-MIR native-object domain 4 pin the corresponding emitters. Their route
fingerprints already feed the unchanged compiler-wide LLVM object-cache key.

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
| Route-specific native-object domain | The corresponding emitter's input assumptions, lowering meaning, object format, or route-specific fingerprint changes. |
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
| LCIR or native object | Do not reuse it outside its exact identity, route, target, and optimization domain. |
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
