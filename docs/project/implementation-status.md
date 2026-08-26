# Implementation status

Loom is usable for its tested examples and experimental programs, but it is a
pre-1.0 language and toolchain. “Implemented” below means there is executable
repository evidence, not that the feature has broad production adoption or a
long-term compatibility guarantee.

## Platform support matrix

| Platform / target | CI-tested | Compiler layers | Native runtime and LLVM closure | Cross-target object | Release archive |
| --- | --- | --- | --- | --- | --- |
| Linux x86-64 (Ubuntu 24.04) | yes | yes | yes | host and tested 64-bit alternate-object path | yes |
| macOS arm64 (macOS 15) | yes | yes | yes | host and tested 64-bit alternate-object path | yes |
| Windows x86-64 (Windows 2025) | yes, limited job | selected platform-independent crates only | no | not a CI claim | no |
| Other LLVM 64-bit triples | no general CI claim | host-dependent | only with a matching validated runtime bundle and linker | possible when LLVM provides the target | no |
| 32-bit triples | no | not a native claim | no runtime/executable support | complete scalar LCIR object only when LLVM provides the target; legacy route rejects | no |

The Windows job checks, lints, tests, and builds `loom-core`, `loom-syntax`,
`loom-hir`, `loom-sema`, `loom-mir`, `loom-codegen-ir`, `loom-lowering`,
`loom-runtime-abi`, and `loom-benchmark`. It does not build `loomc`, the LLVM
backend, interpreter, native runtime, driver, or LSP. Therefore it is evidence
for selected platform-independent compiler layers, not a Windows Loom
toolchain claim.

Linux and macOS build the complete Cargo workspace with Rust 1.88 and LLVM 19.
They also execute native/runtime integration gates. Linux additionally runs
the complete Core example loop and controlled quality runner; macOS runs the C3
multi-package loop and standard-library differential gates.

## Tested vertical slices

The following repository fixtures are run through real compiler stages:

| Fixture | Evidence |
| --- | --- |
| `examples/core01` | constraints/refined values, record invariants, method contracts, check/build/test/run, and built-artifact execution on both backends in Linux CI |
| `examples/core02` | concepts, associated types, static and dynamic dispatch, mutable receiver writeback, and the same dual-backend loop |
| `examples/core03` | moving GC, lexical cleanup, stackless async, joins, cancellation/readiness, and the same dual-backend loop |
| `examples/packages/application` | path dependency, binary/test targets, and dual-backend source/artifact execution |
| `examples/c3/application` | three-package graph, multiple modules, binary/test targets, dual-backend execution on Linux and macOS |
| `fixtures/scalar-lcir` | complete typed scalar route, native execution, Linux DWARF inspection, and macOS dSYM inspection |
| `fixtures/lcir-debug-fallible` | target-laid-out fallible debug ABI, visible and artificial formal parameters, return-only parameter locations, and macOS LLDB parameter/step-out inspection |
| `fixtures/standard-library` | differential interpreter/native checks for structured values, text, maps, JSON, typed file/socket I/O, logging, GC, and async behavior |

The workspace also has focused parser, semantic, MIR-validator, interpreter,
runtime, codegen, CLI, package, registry, cache, formatter, LSP, and hostile
input tests.

## Toolchain features

| Capability | Status and evidence boundary |
| --- | --- |
| `check/build/test/run` | Implemented for the tested Core and package fixtures on both backends. |
| Code generation IR foundation | Production native preparation attempts one atomic whole-artifact scalar MIR-to-LCIR lowering. Complete artifacts use independently checked LCIR, its typed LLVM emitter, and a route-specific cache identity; only reachable `Unsupported` input selects the complete legacy source graph. Source-to-LCIR-to-native differential and CLI route tests cover the scalar slice. Broader value representations and structured contract-fault metadata remain incomplete. |
| Native LLVM executable | Implemented and CI-tested on Linux x86-64 and macOS arm64. |
| Interpreted executable artifact | Implemented, versioned, decoded, validated, and exercised by CLI tests/CI. |
| Portable `.loomlib` | Implemented and release-gated; not a native library or stable ABI. |
| Manifest/lock/features/path dependencies | Implemented with resolver and CLI integration tests. |
| Local and HTTPS registry | Implemented with authentication, digest verification, bounded downloads, offline validated cache, and hostile-cache tests. Registry-version immutability remains a server protocol requirement. |
| Persistent compiler cache | Implemented for parse/interface/typed state/checked MIR/route-specific native object/portable artifacts; native final link intentionally uncached. |
| Debug source info | Linux DWARF and macOS dSYM metadata are checked in CI. Complete scalar LCIR artifacts retain typed emission for `debug` and carry source functions, exact physical signatures, stable `argN` parameter locations, artificial fault-context parameters, and instruction locations; macOS LLDB verifies a fallible parameter and physical step-out result. Unsupported reachable artifacts use the complete legacy route. |
| LSP | Built and tested as a workspace crate; this status does not claim editor-specific distribution. |
| Formatter | Implemented with write/check modes and CLI tests. |
| Native cross object | Tested with an alternate 64-bit Linux triple; arbitrary triples remain conditional on the installed LLVM targets. |
| Cross executable | Implemented only through an exact runtime bundle plus explicit linker; the repository does not publish a general cross-runtime catalog. |

## CI quality evidence

Linux CI runs formatting, workspace check, Clippy with warnings denied, all
workspace tests and builds, dual-backend fixture loops, standard-library
differential tests, runtime-bundle tests, Linux DWARF inspection, the controlled
`loom-quality` runner, and three short fuzz targets. The separate macOS job
verifies the dSYM metadata and runs the LLDB parameter and step-out inspection.

The PR benchmark workflow compares the base and candidate merge revisions on
one Ubuntu x86-64 runner and one macOS arm64 runner. A separate trusted workflow
validates both pairs of reports and posts one informational sticky comment with
an exact table plus separate macOS and Linux runtime-index charts. The charts
are stacked for readability, and each is normalized only to its same-platform
base. The comment is not a cross-platform performance certification or a merge
threshold.

## Not established

The repository does not yet provide evidence for:

- production stability, large external applications, or a stable 1.0
  compatibility policy;
- Windows native execution or Windows release archives;
- 32-bit native execution;
- a stable FFI, dynamic library, plugin, or reflection ABI;
- a multithreaded executor;
- general performance superiority over Go, Rust, C, or C++;
- long-term incremental behavior on independently maintained large projects.

Treat the release archive matrix and current versioned formats as the concrete
support boundary.
