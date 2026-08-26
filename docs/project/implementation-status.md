# Implementation status

Loom is usable for its tested examples and experimental programs, but it is a
pre-1.0 language and toolchain. “Implemented” below means there is executable
repository evidence, not that the feature has broad production adoption or a
long-term compatibility guarantee.

## Platform support matrix

| Platform / target | CI-tested | Frontend | Native runtime and LLVM closure | Cross-target object | Release archive |
| --- | --- | --- | --- | --- | --- |
| Linux x86-64 (Ubuntu 24.04) | yes | yes | yes | host and tested 64-bit alternate-object path | yes |
| macOS arm64 (macOS 15) | yes | yes | yes | host and tested 64-bit alternate-object path | yes |
| Windows x86-64 (Windows 2025) | yes, limited job | selected platform-independent crates only | no | not a CI claim | no |
| Other LLVM 64-bit triples | no general CI claim | host-dependent | only with a matching validated runtime bundle and linker | possible when LLVM provides the target | no |
| 32-bit triples | no | not a native claim | unsupported by current native value ABI | rejected | no |

The Windows job checks, lints, tests, and builds `loom-core`, `loom-syntax`,
`loom-hir`, `loom-sema`, `loom-mir`, `loom-lowering`, `loom-runtime-abi`, and
`loom-benchmark`. It does not build `loomc`, the LLVM backend, interpreter,
native runtime, driver, or LSP. Therefore it is frontend evidence, not a
Windows Loom toolchain claim.

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
| `fixtures/standard-library` | differential interpreter/native checks for structured values, text, maps, JSON, typed file/socket I/O, logging, GC, and async behavior |

The workspace also has focused parser, semantic, MIR-validator, interpreter,
runtime, codegen, CLI, package, registry, cache, formatter, LSP, and hostile
input tests.

## Toolchain features

| Capability | Status and evidence boundary |
| --- | --- |
| `check/build/test/run` | Implemented for the tested Core and package fixtures on both backends. |
| Native LLVM executable | Implemented and CI-tested on Linux x86-64 and macOS arm64. |
| Interpreted executable artifact | Implemented, versioned, decoded, validated, and exercised by CLI tests/CI. |
| Portable `.loomlib` | Implemented and release-gated; not a native library or stable ABI. |
| Manifest/lock/features/path dependencies | Implemented with resolver and CLI integration tests. |
| Local and HTTPS registry | Implemented with authentication, digest verification, bounded downloads, offline validated cache, and hostile-cache tests. Registry-version immutability remains a server protocol requirement. |
| Persistent compiler cache | Implemented for parse/interface/typed state/checked MIR/object/portable artifacts; native final link intentionally uncached. |
| Debug source info | Linux DWARF checked in CI; macOS dSYM generation covered by workspace tests/build and native implementation. |
| LSP | Built and tested as a workspace crate; this status does not claim editor-specific distribution. |
| Formatter | Implemented with write/check modes and CLI tests. |
| Native cross object | Tested with an alternate 64-bit Linux triple; arbitrary triples remain conditional on the installed LLVM targets. |
| Cross executable | Implemented only through an exact runtime bundle plus explicit linker; the repository does not publish a general cross-runtime catalog. |

## CI quality evidence

Linux CI runs formatting, workspace check, Clippy with warnings denied, all
workspace tests and builds, dual-backend fixture loops, standard-library
differential tests, runtime-bundle tests, Linux DWARF inspection, the controlled
`loom-quality` runner, and three short fuzz targets.

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
