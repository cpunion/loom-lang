# Caching

Loom uses two separate caches:

- a project-local compiler cache for parse, semantic, MIR, object, and portable
  artifact work;
- an HTTP registry cache for downloaded package bundles.

Neither cache is a source of authority. Cached bytes are untrusted and are
validated before reuse.

Here “untrusted” means that malformed or accidentally corrupted bytes cannot
bypass decoding and structural validation. The project-local content hashes
are not authentication against an actor that can rewrite both a reference and
its blob. Do not share a writable compiler cache across trust principals or
reuse one in a more privileged build context.

## Compiler cache

The default compiler cache is:

```text
PROJECT/target/loom/cache/v4
```

Use `--cache-dir DIR` to choose another root or `--no-cache` to disable
compiler caching for a source command.

The cache is content-addressed. Small reference records point to SHA-256 blobs,
and every load verifies the reference namespace, schema, key, declared size,
blob size, and content digest. A corrupt entry becomes a cache miss rather than
a partially trusted compiler input. Cached checked MIR additionally passes the
normal artifact decoder and MIR validator.

Construction proofs are process-local analysis conclusions, not cache
certificates. The compiler therefore does not publish proof-bearing typed state
or checked MIR to the persistent cache. Loads also reject a forged proof
disposition. Such a layer is reported as a miss and rebuilt from source, so a
warm command does not silently change from a proof-eliminated direct route to a
runtime-recheck route. Parse, interface and other proof-free layers can still
hit normally. Long-lived in-process analysis reuse has not crossed this disk
boundary and remains available.

Current namespaces include:

- source parse results;
- module interfaces;
- typed module semantic state;
- complete checked MIR and stable diagnostics;
- target-specific LLVM objects;
- interpreted executable and portable-library artifacts.

Native final executables are deliberately not cached. Linking depends on
non-hermetic SDK, sysroot, CRT, system-library, linker-child, and debug
companion inputs. A prepared target object is cacheable because its key includes
the exact route, LLVM/codegen, target machine, optimization, roots, reachable
content, runtime ABI, and debug-source identity. A requested LLVM-IR side
artifact bypasses the object cache so the requested file is always written.

Cache writes and materialization are best-effort during compilation. A failure
falls back to fresh work. Explicit `cache stat` and `cache prune` operations
report I/O failures because those commands were requested directly.

## Inspecting and pruning

```sh
loomc cache stat .
loomc cache prune .
```

`cache stat` reports schema version, reference and blob counts, bytes, invalid
references, and reclaimable unreferenced blobs. `cache prune` removes only
invalid references and blobs unreachable from a valid reference inside the
exact versioned cache root.

Deleting the compiler cache is semantically safe; the next source command
rebuilds it. Do not treat cache deletion as a substitute for reporting a
reproducible compiler defect.

## Cache identities

Frontend keys include normalized project/source identities, Loom language
version, compiler/frontend build identity, embedded standard-library identity,
contract mode, stable source paths and bytes, selected features, and resolved
package graph.

Module caching separates:

- public interface fingerprints;
- declaration/semantic shape fingerprints;
- body fingerprints.

That split permits unchanged module bodies to be reused when a body-only edit
leaves the declaration graph compatible. Any incompatible shape falls back to
fresh semantic analysis.

LLVM object keys use separate LCIR and legacy identity domains. Both include
the exact linked LLVM identity, target triple and data layout, CPU policy and
features, implicit-versus-explicit target selection, optimization pipeline,
PIC relocation, debug sources, and native runtime ABI. The LCIR identity
streams the complete checked-artifact identity. The legacy identity includes
the run/test harness kind, selected roots, source reachability, reachable
functions and witness slots, and the semantic tables consumed by legacy
lowering. Fingerprint errors disable neither validation nor correctness; they
are reported instead of being converted into a cache miss.

These digests provide content integrity and deterministic identity. They do not
authenticate a cache against an actor with the same filesystem permissions.

## Registry cache

HTTP registry entries live under `target/loom/registry/http`, partitioned by
registry identity, package, version, and digest. They are independent of
`--cache-dir` and `--no-cache`.

Every registry cache hit revalidates the raw downloaded bundle digest and all
materialized files. A sidecar record alone is insufficient. `--offline` can
use only such a validated entry and never converts a corrupt cache entry into
trusted package content.

See [Packages and registries](packages-and-registries.md) for transport and
credential rules.
