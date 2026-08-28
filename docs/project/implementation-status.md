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
| Windows x86-64 (Windows 2025) | real runner, full pass pending | check/lint and most workspace tests verified | runtime pack plus typed object/link/run paths verified; complete job pending | host object path | `.zip` workflow entry configured; no verified archive |
| Other LLVM 64-bit triples | no general CI claim | host-dependent | only with a matching validated runtime bundle and linker | possible when LLVM provides the target | no |
| 32-bit triples | no | not a native claim | no runtime/executable support | feature-dependent direct LCIR object only when LLVM provides the target; `Text` and the legacy route reject | no |

The Windows CI job installs LLVM 19.1.7 and Rust 1.88, checks, lints, tests, and
builds the complete workspace, and runs the Core 0.1-0.3 check/build/test/run
loops on both backends. A real Windows runner has verified the LLVM bootstrap,
compiler check/lint, runtime packing, target-machine construction, and most
typed native CLI loops. Its last incomplete run exposed a one-MiB process-stack
failure while lowering a long logical chain; lowering now balances associative
short-circuit chains and the job runs that exact TextMap closure before the full
workspace. Native builds additionally require the expected `.exe` and `.pdb`
outputs, parse the PDB, and inspect a compiler-emitted COFF object for CodeView
sections before executing the artifact. The release job reuses the same pinned
LLVM bootstrap and is configured to stage `loomc.exe`, `loom-lsp.exe`, the
compiler's `LLVM-C.dll`, and `loom_runtime.lib`, execute Core, C3,
standard-library, and adjacent-runtime gates, and hash a `.zip`.
These configurations become Windows toolchain and archive claims only after
successful Windows runner evidence; source-level cross-checks on another host
are not described as Windows execution.

Linux, macOS, and the configured Windows job build the complete Cargo workspace
with Rust 1.88 and LLVM 19 and execute native/runtime integration gates. Linux
additionally runs the controlled quality runner; macOS runs the C3
multi-package loop and standard-library differential gates.

## Tested vertical slices

The following repository fixtures are run through real compiler stages:

| Fixture | Evidence |
| --- | --- |
| `examples/core01` | constraints/refined values, record invariants, method contracts, check/build/test/run, and built-artifact execution on both backends in Linux CI |
| `examples/core02` | concepts, associated types, static and dynamic dispatch, mutable receiver writeback, first-class dynamic product/sum/List storage, and typed-LCIR main and test artifacts in the same dual-backend loop |
| `examples/core03` | moving GC, lexical cleanup, stackless async, all four static Task join policies, cancellation/readiness, and typed-LCIR main/test artifacts in the same dual-backend loop |
| `fixtures/async-generic-contracts` | bounded generic async instances, precondition/postcondition task faults captured by fixed `Task.settled`, `TaskFault` inspection, `Task.any` cancellation, interpreter execution, and a typed-LCIR native test artifact |
| `examples/packages/application` | path dependency, binary/test targets, and dual-backend source/artifact execution |
| `examples/c3/application` | three-package graph, multiple modules, binary/test targets, dual-backend execution on Linux and macOS |
| `fixtures/typed-lcir` | complete typed direct route, native execution, Linux DWARF inspection, and macOS dSYM inspection |
| `fixtures/lcir-debug-fallible` | target-laid-out fallible debug ABI, visible and artificial formal parameters, return-only parameter locations, and macOS LLDB parameter/step-out inspection |
| `fixtures/lcir-generics` | bounded concrete generic instances with exact type/proof identity, direct host execution, and direct MSVC object ABI inspection |
| `fixtures/lcir-generic-products` | concrete generic records, invariant records, and refined wrappers through projection, contracts, calls, managed Text roots, real check/build/test/run, host differential execution, and Linux/MSVC object emission |
| `fixtures/lcir-structural-equality` | typed value equality for tuples, generic and nongeneric records, refined values, active sum payloads, Lists, and contracts; real check/build/test/run, allocation-pressure GC, interpreter/legacy/typed differential execution, and Linux/MSVC object emission |
| `fixtures/lcir-projected-places` | exact nested and generic-instance projected receiver writeback, sibling evaluation order, aggregate loop phis, and real `check/build/test/run` commands |
| `fixtures/lcir-text` | literal-proven one-pointer `Text`, allocation-free length/containment/content comparison, direct generic flow, host execution, and direct 64-bit Linux/MSVC object emission |
| `fixtures/lcir-managed-text` | artifact-wide direct managed `Text`, dynamic concat, exact typed shadow roots, semantic alias preservation, and real check/build/test/run commands |
| `fixtures/lcir-managed-products` | unboxed nested record/tuple products with managed Text leaves, direct product calls/returns, semantic aliases, and real check/build/test/run commands |
| `fixtures/lcir-managed-sums` | closed unboxed sums with active-variant Text roots, nested product payloads, contract matches over managed leaves, forced collection between call arguments, and real check/build/test/run commands |
| `fixtures/lcir-managed-lists` | direct repeated storage for scalar/Text/product/sum/nested-List elements, immutable aliases, checked reads, geometric unique append, moving-GC roots, and real check/build/test/run commands |
| `fixtures/lcir-typed-textmap` | compiler-private direct `TextMap[V]` storage for scalar/Text/product/sum/List/nested-map values, immutable insertion/replacement/removal, containment, exact `Option[V]` lookup, insertion-order-independent structural equality, removal during forced moving-GC relocation, interpreter/legacy/typed differential execution, release IR, Linux/MSVC objects, 32-bit fail-close classification, real `check/build/test/run` commands, and a one-MiB compiler-stack closure for its long logical chain |
| `fixtures/lcir-typed-async` | checked stackless coroutine frames for infallible scalar/product/Text async functions, typed Task handles and one-child await joins, exact suspension root maps, forced parent-Text relocation, interpreter/legacy/typed differential execution, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-async-managed-collections` | exact one-pointer `List[T]` and compiler-private `TextMap[V]` values in async parameters, results, nested products, and live suspension rows; moving-GC pressure, checked frame bitmaps, debug metadata, Linux/MSVC objects, 32-bit fail-close classification, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-sleep` | first-class checked `Task.sleep` with `Int` and `Duration` inputs, zero and positive deadlines, managed values live across later awaits, and typed-LCIR main/test native execution |
| `fixtures/lcir-sync-task-helpers` | effect-derived hidden executor forwarding through direct, nested, fallible, and fixed-composite synchronous Task-producing helpers; exact debug ABI, canonical sleep faults, interpreter/typed execution, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-all` | direct multi-child awaits and stored exact heterogeneous `Task.all`, one-field and mixed Text/Int/Unit tuples, shared static-shape descriptors, atomic child adoption, moving-GC survival, interpreter/typed execution plus legacy/typed child-fault and sibling-cancellation comparison, release IR, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-any` | nonempty immediately awaited fixed homogeneous `Task.any`, deterministic nonzero winner selection, managed Text results, repeated loser cancellation and retirement, typed-object surface inspection, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-outcomes` | fixed and sole-List-literal `Task.settled`/`Task.race`, canonical completed/faulted/cancelled outcomes, fault Text roots across later capture safepoints, deterministic nonzero race winners, loser cleanup, typed-LCIR main/test native execution, and real `check/build/test/run` commands |
| `fixtures/lcir-async-cleanup` | typed-LCIR `defer` and static-concept `scoped` cleanup across suspension, exact normal/fault/cancel live rows, LIFO normal cleanup, child-fault propagation, sibling cancellation, source-callback cancellation dispatch, interpreter/legacy/typed differentials, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-async-writeback` | synchronous functional inout calls inside typed coroutines, managed-Text writeback across moving collection, normal/fault/cancellation cleanup, fault writeback before lexical cleanup, by-value dynamic View parameters under mutable dispatch, recursive unique-witness `dyn` erasure across async parameters, results, and nested frames, multi-witness finite dynamic calls outside suspension rows, interpreter/typed differentials, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-json` | canonical recursive `Json` construction and matching through `List[Json]`/`TextMap[Json]` cycle breakers, the general collision-free closed-sum byte-class carrier, exact repeated tracing, immutable map aliases, forced moving-GC relocation, interpreter/legacy/typed differential execution, Linux/MSVC objects, and real `check/build/test/run` commands; equality and parse remain follow-up slices |
| `fixtures/lcir-json-format` | direct typed formatting of all six canonical `Json` variants, canonical TextMap key order, exact string escaping and negative-zero spelling, ordinary `DepthLimit`/`NonFiniteNumber` errors for deep, NaN, and infinite inputs, and real main/test `check/build/test/run` commands without a universal value or executor |
| `fixtures/lcir-sum-layout-collisions` | unrelated and nested closed sums, including opposing pointer-first/scalar-first record variants, with scalar, product, Text, List, TextMap, and recursive Json payloads; one target-data-derived carrier plan drives pack/unpack, exact repeated descriptors, and forced moving-GC relocation across interpreter/legacy/typed differential execution, Linux/MSVC objects, 32-bit fail-closed emission, artifact-wide placement/emission bounds, and real `check/build/test/run` commands |
| `fixtures/lcir-fallible-async` | checked fallible stackless coroutines, ordinary managed `Result` completion, exact child source/contract fault propagation, cancellation, collision-free completed/live sum roots, balanced callback roots, forced moving-GC relocation, interpreter/legacy/typed differential execution, Linux/MSVC objects, 32-bit fail-closed behavior, and real `check/build/test/run` commands |
| `fixtures/lcir-scalar-builtins` | exact parse-result sums, finite checks, managed Float formatting, direct Duration values, typed roots, and real check/build/test/run commands without universal values or an executor |
| `fixtures/lcir-typed-logging` | canonical structured logging through typed LCIR, including exact JSONL stderr, escaping, empty fields, and canonical TextMap key order |
| `fixtures/lcir-lexical-cleanup` | direct typed assertions and source contracts, checked-root and assumed-body boundaries, mutable invariant writeback, lexical `defer`, static-concept `scoped` disposal, exact LIFO/fault behavior, and real check/build/test/run commands without universal values or an executor |
| `fixtures/lcir-static-concepts` | concrete static method selection, conditional proof forwarding, associated-type normalization, direct host execution, and MSVC COFF emission without runtime witness or universal-value surfaces |
| `fixtures/lcir-dyn-unique` | closed-world unique-witness `dyn` erasure, direct calls, aggregate/List storage, dead conformance and method-slot elimination, real check/build/test/run, host execution, and Linux/MSVC object emission without runtime witness data |
| `fixtures/standard-library` | differential interpreter/native checks for structured values, text, maps, JSON, typed file/socket I/O, logging, GC, and async behavior |

Every admitted payload-bearing closed sum now uses the same bounded
target-data-derived byte-class carrier plan. Pack/unpack, active managed roots,
and List/TextMap repeated descriptors share that plan; recursive Json's compact
24-byte 64-bit representation is a consequence rather than a source-type
special case.

The workspace also has focused parser, semantic, MIR-validator, interpreter,
runtime, codegen, CLI, package, registry, cache, formatter, LSP, and hostile
input tests.

## Toolchain features

| Capability | Status and evidence boundary |
| --- | --- |
| `check/build/test/run` | Implemented for the tested Core and package fixtures on both backends. |
| Code generation IR foundation | Implemented for the direct slices listed below. Native preparation is atomic and fails closed to the complete legacy route when any reachable operation is unsupported. |
| Native LLVM executable | Implemented and CI-tested on Linux x86-64 and macOS arm64; a complete Windows x86-64 native CI gate is configured and must pass before release support is claimed. |
| Interpreted executable artifact | Implemented with strict cache/executable kind separation, selected-entry definition closure, dense identity remapping, deterministic bytes, complete decode validation, and CLI tests. |
| Portable `.loomlib` | Source/interface format v2 is implemented and release-gated; consumers recompile packaged source, and the artifact is not a native library or stable ABI. |
| Manifest/lock/features/path dependencies | Implemented with resolver and CLI integration tests. |
| Local and HTTPS registry | Implemented with authentication, digest verification, bounded downloads, offline validated cache, and hostile-cache tests. Registry-version immutability remains a server protocol requirement. |
| Persistent compiler cache | Implemented for parse/interface/typed state/checked MIR/route-specific native object/portable artifacts; proof-bearing typed/MIR layers intentionally rebuild from source to preserve cold/warm proof elimination and route identity, canonical `MustScope` identity is rederived from current module-qualified HIR rather than trusted from typed-state bytes, and native final link is intentionally uncached. |
| Debug source info | Linux DWARF and macOS dSYM metadata are checked in CI. Complete typed LCIR artifacts retain direct emission for `debug` and carry source functions, target-laid-out product and physical return types, stable `argN` parameter locations, artificial status/writeback/fault-context state, and instruction locations; macOS LLDB verifies a fallible parameter and physical step-out result. MSVC objects select LLVM CodeView metadata and links use `/DEBUG` plus an explicit staged `/PDB:` path; the configured Windows gate inspects typed-LCIR COFF/PDB structures, but no source-level Windows debugger session is claimed yet. Unsupported reachable artifacts use the complete legacy route. |
| LSP | Built and tested as a workspace crate; this status does not claim editor-specific distribution. |
| Formatter | Implemented with write/check modes and CLI tests. |
| Native cross object | Tested with an alternate 64-bit Linux triple; arbitrary triples remain conditional on the installed LLVM targets. |
| Cross executable | Implemented only through an exact runtime bundle plus explicit linker; the repository does not publish a general cross-runtime catalog. |

### Typed LLVM route

Production native preparation performs one whole-artifact MIR-to-LCIR attempt
and independently validates the result before typed LLVM emission. Current
direct coverage includes:

- scalar and managed Text operations, structural tuples, concrete records,
  refined values, closed sums, Lists, compiler-private TextMaps, and bounded
  concrete generic instances;
- canonical recursive Json formatting into the exact
  `Result[Text, JsonError]`, including typed Text publication and ordinary
  depth/non-finite error values;
- canonical structured logging with direct Text and `TextMap[Text]` values;
- supported contracts and proof replay, static concepts, closed dynamic-concept
  catalogs, exact moving-GC roots, and static lexical cleanup;
- checked stackless coroutines, typed Task handles, exact suspension rows,
  fallible timers, and executor-owned roots;
- nonempty static forms of the four standard-library Task policies, including
  stored heterogeneous `Task.all`, sole-List-literal specialization, exact
  `TaskOutcome[T]` capture, winner finalization, cancellation, and draining.

Async `requires` checks run in child state zero. A created Task carries its
creation-site blame, an async root carries its declaration span, and
`TaskCreate` does not inherit child fault effects. Core gains no `all`, `any`,
`settled`, or `race` syntax. HIR keeps these as ordinary method calls; semantic
resolution maps only canonical, unshadowed Task API members through the current
embedded catalog to stable `StandardLibraryItem` identities, and MIR
specialization consumes those identities without re-reading source names.
Trusted source-library definitions remain the next replacement for the catalog
lookup, not a change to the language or MIR boundary.

Remaining atomic fallback includes open or prerequisite-dependent dynamic
concepts, unsupported proof or contract value shapes, recursive nominal
equality through managed collections, finite/open dynamic managed carriers,
explicit mutable coroutine parameters, raw readiness,
empty/stored/computed/runtime-sized Task List joins, first-class
`Task.any`/`Task.settled`/`Task.race` results, and unsupported projected inout
shapes.

## CI quality evidence

Linux CI runs formatting, workspace check, Clippy with warnings denied, all
workspace tests and builds, dual-backend fixture loops, standard-library
differential tests, runtime-bundle tests, Linux DWARF inspection, the controlled
`loom-quality` runner, and three short fuzz targets. Before the full workspace
test, Linux repeats the typed TextMap `check/build/test/run` loop with a one-MiB
process stack; Windows runs the same focused closure on its native compiler
stack. The separate macOS job
verifies the dSYM metadata and runs the LLDB parameter and step-out inspection.
The controlled runner prepares every native object through the production
router, and requires Core 0.1, Core 0.2, Core 0.3, the async generic-contract,
typed logging, typed JSON formatting, and complete C3 fixtures to select LCIR
for their prepared main or test artifacts. The typed logging gate also checks
the exact JSONL standard-error bytes for both run and test artifacts; the typed
JSON gate executes all canonical formatting and error cases through each
artifact.
The remaining reviewed legacy allowance covers the broader standard-library
fixture's JSON parsing and typed external I/O operations.

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
- verified Windows native execution or a successfully produced Windows release
  archive;
- 32-bit native execution;
- a stable FFI, dynamic library, plugin, or reflection ABI;
- a multithreaded executor;
- general performance superiority over Go, Rust, C, or C++;
- long-term incremental behavior on independently maintained large projects.

Treat the release archive matrix and current versioned formats as the concrete
support boundary.
