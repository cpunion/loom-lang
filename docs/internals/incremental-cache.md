# Incremental cache

Loom has two complementary forms of reuse:

- in-process query reuse inside a long-lived `AnalysisHost`;
- a persistent content-addressed compiler cache shared by separate `loomc`
  processes.

Both are correctness-preserving optimizations. Any incompatible or invalid
entry falls back to fresh analysis.

## Module fingerprints

For each module, the driver derives three independent identities:

1. public interface: externally visible declarations and signatures;
2. semantic shape: the declaration graph with bodies removed;
3. body: body content and implementation facts.

Package name, version, Loom language version, and module name are part of the
canonical module identity. Stable serialization and SHA-256 make the keys
independent of process addresses.

When a snapshot changes:

- an identical parse identity can reuse the parsed source;
- an identical public interface preserves dependent-module interface
  compatibility;
- an identical semantic shape permits typed body facts to be considered for
  reuse;
- an identical body reuses that module's checked body facts.

If the module set, declaration shape, or other graph precondition is
incompatible, semantic analysis starts fresh. Reuse is guarded by validation
and a panic boundary; a failed reuse attempt never escapes as a partially
trusted analysis.

## In-process evidence

`loom-quality` constructs a 64-module project, changes one function body, and
requires at least 63 typed-HIR modules to be reused and no more than one to be
rechecked. The gate also has a bounded elapsed-time budget.

This is controlled implementation evidence for a body-only edit. It is not a
claim that every edit in a large project rechecks one module. Public interface,
shape, feature, dependency, language, or compiler changes can invalidate a
larger graph.

## Persistent layers

The persistent cache schema is `4`. Current layers include source parse,
module-interface presence, typed module state, complete checked MIR, target
objects, and portable final artifacts.

The typed-module-state key deliberately excludes body fingerprints while its
payload records them per module. This lets a later process load the compatible
graph and retain unchanged modules after one body edit. Cached semantic state
containing error diagnostics is rejected.

Proof-elision dispositions are process-local. Typed states containing them are
not written to disk, and persisted bodies that could eliminate a construction,
assertion, or contract check are conservatively reanalyzed from source rather
than trusted for that disposition. This restriction does not apply to reuse
inside one `AnalysisHost` process.

Compiler-known resource identities are likewise not accepted from persistent
typed-state bytes. A compatible state can supply reusable body facts, but
semantic analysis rederives `Dispose`, `MustScope`, and `NoSuspend` from the
exact current compiler-owned package and HIR module. Checked-MIR cache entries
carry the three resulting identity tags, source-module consistency metadata,
and prelude ids. Cache serialization requires the complete artifact resource
profile, and cache reads cross both ordinary MIR validation and that profile;
inconsistent or incomplete identity metadata is a cache miss.

Task policy and timer calls store their resolved `StandardLibraryItem` in typed
body facts. Cache schema 4 and `loom-compilation-cache-v4` reject earlier
semantic entries that lacked this standard-item identity. Whether a body is
reused or conservatively reanalyzed, MIR lowering consumes only the resolved
identity; it never reconstructs a policy from source spelling.

Checked-MIR cache envelopes use artifact version 26. Version 24 added the
compiler-known `TextMap.entry_at` builtin; version 25 assigns distinct
compiler-known tags to the three resource concepts resolved from the
compiler-owned package. Tags paired with prelude ids grant MIR authority;
module/name and fixed shapes are independent consistency checks. The artifact
profile rejects a missing or partial trio at both encode and decode.
Version 26 separates generic cache envelopes, whose `entry` is null, from
executable envelopes, whose `entry` is a fixed string. Each decoder rejects the
opposite kind before MIR body validation. Artifact-version validation rejects
older encodings rather than guessing their meaning. The earlier canonical
six-field `ConstraintError` shape remains validator-enforced. These changes
require no cache-schema advance because typed semantic cache payloads do not
encode MIR builtins, canonical concept authority, or executable entry state.
The separate standard-item change above is why the current cache schema is 4.

The complete compilation key includes the normalized project graph, exact
sources, language and frontend build identities, embedded standard library,
and contract mode. A checked-MIR cache hit still runs the artifact decoder and
MIR validator before execution or code generation. Proof-bearing checked MIR is
not published; a forged or legacy proof-bearing payload loads as a miss. Source
reanalysis reconstructs the same fresh `Proven` MIR as a cold build instead of
permanently replacing its process-local proof with serialized `.loomi`
`Recheck`. Supported nongeneric replay is typed LCIR; generic or unsupported
replay remains legacy.

A version 2 `.loomlib` final artifact is a separate source/interface blob, not
a checked-MIR cache entry. Its `portable-library-artifact-v3` derived cache
identity includes the complete compilation key, selected library target,
library format, and format version. Consequently a compiler or
compiler-distributed standard-library identity change cannot restore an
artifact whose producer checks have not run under the current inputs, even
though the deterministic `.loomlib` bytes themselves omit the standard-library
implementation. This format replacement does not change persistent cache
schema 4 or the checked-MIR cache envelope.

## Native object reuse

Object caching begins after checked MIR and closed-world reachability. Its
identity includes the exact backend/LLVM build, target machine, optimization,
roots, reachable bodies and witness slots, type/concept metadata, and debug
source map. Unreachable private bodies are absent from the fingerprint.

The native final link remains uncached because the compiler cannot yet
identify every SDK, sysroot, CRT, system library, linker subprocess, and debug
companion as one hermetic input. Interpreter executables and `.loomlib` files
are portable deterministic bytes and are cached as final artifacts.

## Trust model

Reference records and blobs are bounded and digest-checked. Invalid JSON,
unknown schema, wrong namespace/key/size, missing blob, digest mismatch,
malformed diagnostics, malformed semantic state, or invalid MIR produces a
miss. Cache stores are atomic and compilation treats ordinary cache I/O
failure as non-fatal.

The CAS digest is an integrity check, not an authenticity mechanism against a
same-permission local attacker. Portable package authenticity is a separate
registry/distribution concern.

The HTTP package cache is separate and has its own bundle and materialized-file
validation. See [Toolchain caching](../reference/toolchain/caching.md).
