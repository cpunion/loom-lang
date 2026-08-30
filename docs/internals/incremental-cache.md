# Incremental cache

Loom has two complementary forms of reuse:

- in-process query reuse inside a long-lived `AnalysisHost`;
- a persistent content-addressed compiler cache shared by separate `loom`
  processes.

Both are correctness-preserving optimizations. Any incompatible or invalid
entry falls back to fresh analysis.

## Package fingerprints

For each directory package, the driver derives three independent identities:

1. public interface: externally visible declarations and signatures;
2. semantic shape: the declaration graph with bodies removed;
3. body: body content and implementation facts.

Module name, version, Loom language version, and package path are part of the
canonical package identity. Stable serialization and SHA-256 make the keys
independent of process addresses.

When a snapshot changes:

- an identical parse identity can reuse the parsed source;
- an identical public interface preserves dependent-package interface
  compatibility;
- an identical semantic shape permits typed body facts to be considered for
  reuse;
- an identical body reuses that package's checked body facts.

If the package set, declaration shape, or other graph precondition is
incompatible, semantic analysis starts fresh. Reuse is guarded by validation
and a panic boundary; a failed reuse attempt never escapes as a partially
trusted analysis.

## In-process evidence

`loom-quality` constructs a 64-package project, changes one function body, and
requires at least 63 typed-HIR packages to be reused and no more than one to be
rechecked. The gate also has a bounded elapsed-time budget.

This is controlled implementation evidence for a body-only edit. It is not a
claim that every edit in a large project rechecks one module. Public interface,
shape, feature, dependency, language, or compiler changes can invalidate a
larger graph.

## Persistent layers

The persistent cache schema is `15`. Current layers include source parse,
package-interface presence, typed package state, complete checked MIR, target
objects, and deterministic final artifacts.

The typed-package-state key deliberately excludes body fingerprints while its
payload records them per package. This lets a later process load the compatible
graph and retain unchanged packages after one body edit. Cached semantic state
containing error diagnostics is rejected.

Proof-elision dispositions are process-local. Typed states containing them are
not written to disk, and persisted bodies that could eliminate a construction,
assertion, or contract check are conservatively reanalyzed from source rather
than trusted for that disposition. This restriction does not apply to reuse
inside one `AnalysisHost` process.

Compiler-known resource identities are likewise not accepted from persistent
typed-state bytes. A compatible state can supply reusable body facts, but
semantic analysis rederives `Dispose`, `MustScope`, and `NoSuspend` from the
exact current compiler-owned module and HIR package. Checked-MIR cache entries
carry the three resulting identity tags, source-package consistency metadata,
and prelude ids. Cache serialization requires the complete artifact resource
profile, and cache reads cross both ordinary MIR validation and that profile;
inconsistent or incomplete identity metadata is a cache miss.

The canonical source identities for `std.float.is_finite`,
`std.text.DecodeTextError`, `std.path.PathError`, `std.io.IoError`,
`std.io.IoErrorKind`, `std.file.File`, `std.net.Socket`, and
`std.log.LogLevel` follow the same rule. Their exact `DefId` values are
rederived before signature and body checking; cached semantic bytes never grant
a same-named function, record, or enum canonical authority.

Task policy and timer calls currently store a resolved `TaskIntrinsic` in typed
body facts. Cache schema `15` and the `loom-compilation-cache-v15` domain cover
that identity, the current compiler-private Float, logging, file, and network
primitive sets, and exact source identities for standard types. They also
exclude Path-specific file builtin tags: the source wrappers convert Path to
Text before the private primitive.
Whether a body is reused or conservatively reanalyzed, MIR lowering consumes
only the resolved identity; it never reconstructs a policy or canonical
standard-library item from source spelling. The same schema removes the
compiler-private semantic types and public builtin method paths for
`IoErrorKind`, `IoError`, `File`, and `Socket`. Checked types, accessors,
resource methods, and disposal witnesses use exact ordinary source definitions.
The Task cache identity disappears when the temporary catalog is replaced by
ordinary source definitions.

Checked-MIR cache envelopes use artifact version `40` and its exact current
MIR shape. The artifact profile requires the complete compiler-known resource
identity trio, all matching prelude ids, the canonical six-field
`ConstraintError`, the exact source-backed decoding/path error identities and
shapes, and the current builtin set. Generic cache envelopes have a null
`entry`; executable `.loomi` envelopes have one fixed exported entry. Each
decoder rejects the opposite kind before MIR body validation. Integer parsing
is ordinary `std.int` source and enters MIR as ordinary definitions and direct
calls.

The complete compilation key includes the normalized project graph, exact
sources, language and frontend build identities, embedded standard library,
and contract mode. A checked-MIR cache hit still runs the artifact decoder and
MIR validator before execution or code generation. Proof-bearing checked MIR is
not published; a forged or malformed proof-bearing payload loads as a miss.
Source reanalysis reconstructs the same fresh `Proven` MIR as a cold build
instead of permanently replacing its process-local proof with serialized
`.loomi` `Recheck`. Supported nongeneric replay is typed LCIR; generic or
unsupported replay remains checked-MIR.

A `.loomlib` version `3` final artifact is a separate source/interface blob, not
a checked-MIR cache entry. Its `portable-library-artifact-v3` derived cache
identity includes the complete compilation key, selected library target,
library format, and format version. Consequently a compiler or
compiler-distributed standard-library identity change cannot restore an
artifact whose producer checks have not run under the current inputs, even
though the deterministic `.loomlib` bytes themselves omit the standard-library
implementation.

## Native object reuse

Object caching begins after checked MIR and closed-world reachability. Its
identity includes the exact backend/LLVM build, target machine, optimization,
roots, reachable bodies and witness slots, type/concept metadata, and debug
source map. Unreachable private bodies are absent from the fingerprint.

The native final link remains uncached because the compiler cannot yet
identify every SDK, sysroot, CRT, system library, linker subprocess, and debug
companion as one hermetic input. Interpreter executables and `.loomlib` files
are portable deterministic bytes and are cached as final artifacts. The
interpreter's `final-artifact-v3` / `loom-interpreted-artifact-writer-v3`
domain records that `.loomi` writing closes and densely remaps the selected
entry rather than serializing the complete checked-MIR cache payload. The full
cache entry can serve another export.

## Trust model

Reference records and blobs are bounded and digest-checked. Invalid JSON,
unknown schema, wrong namespace/key/size, missing blob, digest mismatch,
malformed diagnostics, malformed semantic state, or invalid MIR produces a
miss. Cache stores are atomic and compilation treats ordinary cache I/O
failure as non-fatal.

The CAS digest is an integrity check, not an authenticity mechanism against a
same-permission local attacker. Portable module authenticity is a separate
registry/distribution concern.

The HTTP module cache is separate and has its own bundle and materialized-file
validation. See [Toolchain caching](../reference/toolchain/caching.md).
