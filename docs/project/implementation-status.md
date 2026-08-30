# Implementation status

Loom is usable for its tested examples and experimental programs, but it is a
pre-1.0 language and toolchain. “Implemented” below means there is executable
repository evidence, not that the feature has broad production adoption or a
long-term compatibility guarantee.

## Source `std` completion

The compiler deterministically embeds every non-test Loom source below
`library/std`; adding a package does not require a hand-maintained compiler
table. Passing interpreter or native tests does not by itself make an API
source-backed: the table distinguishes ordinary source definitions from public
names still recognized by semantic builtin tables.

| Surface | Ordinary `library/std` source today | Remaining compiler-owned public surface | Status |
| --- | --- | --- | --- |
| `std.int` | `ParseIntError`, `minimum`, `maximum`, `parse_int`, and the complete parser helper graph | none for the current package API | source-backed |
| `std.io` | public `IoError`, `IoErrorKind`, their accessors, `write`, and `write_line` | exact-owner error access/output primitives and runtime byte boundary | source-backed |
| `std.json` | `parse_json` and its bounded iterative parser helper graph | `Json`, `JsonError`, and `format_json` | partial |
| `std.log` | `LogLevel`, `write`, `debug`, `info`, `warn`, `error`, and their helper graph | only the exact-owner private typed write primitive and runtime byte boundary | source-backed |
| `std.resource` | `Dispose`, `MustScope`, and `NoSuspend` declarations | their fixed language-item meaning and static enforcement intentionally remain language core | source declarations complete |
| `std.float` | `ParseFloatError`, `FloatToIntError`, parsing, formatting, finiteness, and both explicit Int conversions | only exact-owner private scalar primitives and the runtime parse/format boundary | source-backed |
| `std.process` | public `arguments` and `environment` wrappers | compiler-private process snapshot primitives and their runtime OS boundary | source-backed |
| `std.text` / `std.path` | public `DecodeTextError` and `PathError` enums | Text, Bytes, Path, and their intrinsic construction and decoding operations remain core mechanisms | partial |
| `std.time` | none | `Duration`, construction, and conversion | not source-backed |
| `std.file` / `std.net` | `File`, `Socket`, all public acquisition and I/O methods, and their `Dispose`/`MustScope` conformances | only exact-owner private typed I/O and close leaves | source-backed |
| `Task.sleep/all/settled/any/race` | none | temporary public-name resolution through `TaskIntrinsic` plus the private scheduler substrate | transitional |

The target boundary gives every public library declaration an ordinary source
`DefId`. Only irreducible GC, scheduler, scalar, platform, output, and bulk
construction operations remain compiler-private, and only compiler-owned
`std` source may call them. Each migration removes its public builtin or
`TaskIntrinsic` path in the same change; no compatibility alias or parallel
implementation remains.

## Platform support matrix

| Platform / target | Automated evidence | Compiler layers | Native runtime and LLVM closure | Cross-target object | Release archive |
| --- | --- | --- | --- | --- | --- |
| Linux x86-64 (Ubuntu 24.04) | release workflow | yes | release closure | host and tested 64-bit alternate-object path | yes |
| macOS arm64 (macOS 15) | default development gate and release workflow | yes | development and release closure | host and tested 64-bit alternate-object path | yes |
| Windows x86-64 (Windows 2025) | release workflow entry; successful archive run pending | release build | release closure pending | host object path | `.zip` workflow entry configured; no verified archive |
| Other LLVM 64-bit triples | no general CI claim | host-dependent | only with a matching validated runtime bundle and linker | possible when LLVM provides the target | no |
| 32-bit triples | no | not a native claim | no runtime/executable support | feature-dependent direct LCIR object only when LLVM provides the target; `Text` and the checked-MIR route reject | no |

The Windows release job installs LLVM 19.1.7 and Rust 1.88, builds the compiler
and runtime, and runs representative source, standard-library, and controlled
quality gates. It is configured to stage `loom.exe`, `loom-lsp.exe`, the
compiler's `LLVM-C.dll`, and `loom_runtime.lib`, execute Core, C3,
standard-library, and adjacent-runtime gates, and hash a `.zip`.
This configuration becomes Windows toolchain and archive evidence only after
successful Windows runner evidence; source-level cross-checks on another host
are not described as Windows execution.

The default development workflow runs only on macOS. Linux and Windows are
exercised by tagged or manually dispatched release runs rather than every pull
request.

## Tested vertical slices

The following repository fixtures are run through real compiler stages:

| Fixture | Evidence |
| --- | --- |
| `examples/constraints-contracts` | constraints/refined values, record invariants, method contracts, check/build/test/run, and built-artifact execution on both backends in repository tests and release gates |
| `examples/concepts-polymorphism` | concepts, associated types, static and dynamic dispatch, mutable receiver writeback, first-class dynamic product/sum/List storage, and typed-LCIR main and test artifacts in the same dual-backend loop |
| `examples/async-resources` | moving GC, lexical cleanup, stackless async, all four static Task join policies, cancellation/readiness, and typed-LCIR main/test artifacts in the same dual-backend loop |
| `fixtures/async-generic-contracts` | bounded generic async instances, precondition/postcondition task faults captured by fixed `Task.settled`, `TaskFault` inspection, `Task.any` cancellation, interpreter execution, and a typed-LCIR native test artifact |
| `examples/packages/application` | path dependency, directory packages, `_test.loom` discovery, binary target, and dual-backend source/artifact execution |
| `examples/c3/application` | three-module graph, multiple directory packages, `_test.loom` discovery, binary target, and dual-backend execution on Linux and macOS |
| `fixtures/typed-lcir` | complete typed direct route, native execution, Linux DWARF inspection, and macOS dSYM inspection |
| `fixtures/lcir-debug-fallible` | target-laid-out fallible debug ABI, visible and artificial formal parameters, return-only parameter locations, and macOS LLDB parameter/step-out inspection |
| `fixtures/lcir-generics` | bounded concrete generic instances with exact type/proof identity, direct host execution, and direct MSVC object ABI inspection |
| `fixtures/lcir-generic-products` | concrete generic records, invariant records, and pointer-free or managed refined wrappers through projection, contracts, calls, moving-GC roots, debug metadata, real check/build/test/run, host differential execution, and Linux/MSVC object emission |
| `fixtures/lcir-structural-equality` | typed value equality for tuples, generic and nongeneric records, refined values, active sum payloads, Lists, and contracts; real check/build/test/run, allocation-pressure GC, interpreter/checked-MIR/typed differential execution, and Linux/MSVC object emission |
| `fixtures/lcir-projected-places` | exact nested and generic-instance projected receiver writeback, sibling evaluation order, aggregate loop phis, and real `check/build/test/run` commands |
| `fixtures/lcir-text` | literal-proven one-pointer `Text`, allocation-free length/containment/content comparison, direct generic flow, host execution, and direct 64-bit Linux/MSVC object emission |
| `fixtures/lcir-managed-text` | artifact-wide direct managed `Text`, dynamic concat, executable literal matching, exact typed shadow roots, semantic alias preservation, and real check/build/test/run commands |
| `fixtures/lcir-managed-bytes` | one-pointer typed `Bytes`, zero-allocation Text-backed UTF-8 sharing, content equality, Unicode byte indexing, checked negative and upper-bound misses, append, decode, moving-GC publication, and real check/build/test/run commands |
| `fixtures/lcir-typed-path` | exact invariant-protected one-field Text-backed Path values, non-collecting construction/extraction, portable lexical join, rejected raw MIR/LCIR construction and mutation, preserved Unicode and `.`/`..`/repeated-separator spelling, ordinary `ContainsNul`/`AbsoluteJoin` errors, live aliases and joined results through moving-GC pressure, and real check/build/test/run commands without checked-MIR path helpers or an executor |
| `fixtures/lcir-managed-products` | unboxed nested record/tuple products with managed Text leaves, direct product calls/returns, semantic aliases, and real check/build/test/run commands |
| `fixtures/lcir-managed-sums` | closed unboxed sums with active-variant Text roots, nested product payloads, contract matches over managed leaves, forced collection between call arguments, and real check/build/test/run commands |
| `fixtures/lcir-managed-lists` | direct repeated storage for scalar/Text/product/sum/nested-List elements, immutable aliases, checked reads, geometric unique append, moving-GC roots, and real check/build/test/run commands |
| `fixtures/lcir-typed-textmap` | compiler-private direct `TextMap[V]` storage for scalar/Text/product/sum/List/nested-map values, immutable insertion/replacement/removal, containment, exact `Option[V]` lookup, insertion-order-independent structural equality, removal during forced moving-GC relocation, interpreter/checked-MIR/typed differential execution, release IR, Linux/MSVC objects, 32-bit fail-close classification, real `check/build/test/run` commands, and a one-MiB compiler-stack closure for its long logical chain |
| `fixtures/lcir-typed-async` | checked stackless coroutine frames for infallible scalar/product/Text and managed-refined Text/Bytes async functions, typed Task handles and one-child await joins, exact suspension root maps, forced parent-Text relocation, debug metadata, interpreter/checked-MIR/typed differential execution, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-async-managed-collections` | exact one-pointer `List[T]` and compiler-private `TextMap[V]` values in async parameters, results, nested products, and live suspension rows; moving-GC pressure, checked frame bitmaps, debug metadata, Linux/MSVC objects, 32-bit fail-close classification, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-sleep` | first-class checked `Task.sleep` with `Int` and `Duration` inputs, zero and positive deadlines, managed values live across later awaits, and typed-LCIR main/test native execution |
| `fixtures/lcir-sync-task-helpers` | effect-derived hidden executor forwarding through direct, nested, fallible, and fixed-composite synchronous Task-producing helpers; exact debug ABI, canonical sleep faults, interpreter/typed execution, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-all` | direct multi-child awaits and stored exact heterogeneous `Task.all`, one-field and mixed Text/Int/Unit tuples, shared static-shape descriptors, atomic child adoption, moving-GC survival, interpreter/typed execution plus checked-MIR/typed child-fault and sibling-cancellation comparison, release IR, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-any` | nonempty immediate and stored fixed homogeneous `Task.any`, exact first-class composite results, deterministic nonzero winner selection, managed Text roots, repeated loser cancellation and retirement, typed-object surface inspection, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-outcomes` | immediate and stored fixed plus sole-List-literal `Task.settled`/`Task.race`, exact first-class composite outcomes, canonical completed/faulted/cancelled values, fault Text roots across later capture safepoints, deterministic nonzero race winners, loser cleanup, typed-LCIR main/test native execution, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-lists` | empty, stored, computed, and runtime-width homogeneous `List[Task[T]]` policies through exact `TaskJoinList` composites; affine carrier transfer, borrowed length, direct atomic child adoption, source/result moving-GC roots, canonical empty/failed selection faults, and real `check/build/test/run` commands without universal join helpers |
| `fixtures/lcir-affine-task-carriers` | direct and proven transparent Task handles moved inside structural tuples, closed sums, and Options across synchronous calls, returns, suspension, atomic tuple splitting, and exhaustive matches; exact managed-sibling frame roots and real `check/build/test/run` commands without universal values |
| `fixtures/lcir-async-cleanup` | typed-LCIR `defer` and static-concept `scoped` cleanup across suspension, exact normal/fault/cancel live rows, LIFO normal cleanup, child-fault propagation, sibling cancellation, source-callback cancellation dispatch, interpreter/checked-MIR/typed differentials, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-async-writeback` | synchronous functional inout calls inside typed coroutines, managed-Text writeback across moving collection, normal/fault/cancellation cleanup, fault writeback before lexical cleanup, by-value dynamic View parameters under mutable dispatch, unique-witness `dyn` erasure and finite multi-witness managed catalogs across async parameters, results, and suspension frames, interpreter/typed differentials, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-json` | canonical recursive `Json` construction and matching through `List[Json]`/`TextMap[Json]` cycle breakers, source-backed parsing, generated structural equality, canonical typed formatting, exact repeated tracing, immutable map aliases, forced moving-GC relocation, interpreter/MIR/LCIR differential execution, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-json-format` | direct typed formatting of all six canonical `Json` variants, canonical TextMap key order, exact string escaping and negative-zero spelling, ordinary `DepthLimit`/`NonFiniteNumber` errors for deep, NaN, and infinite inputs, and real main/test `check/build/test/run` commands without a universal value or executor |
| `fixtures/lcir-json-parse` | ordinary source-backed iterative JSON parsing, complete-document and Unicode escape checks, numeric range errors, canonical bulk TextMap construction, and lexicographically smallest duplicate-key selection; LCIR-only real `check/build/test/run` commands and object inspection without a universal value or executor |
| `fixtures/lcir-sum-layout-collisions` | unrelated and nested closed sums, including opposing pointer-first/scalar-first record variants, with scalar, product, Text, List, TextMap, and recursive Json payloads; one target-data-derived carrier plan drives pack/unpack, exact repeated descriptors, and forced moving-GC relocation across interpreter/checked-MIR/typed differential execution, Linux/MSVC objects, 32-bit fail-closed emission, artifact-wide placement/emission bounds, and real `check/build/test/run` commands |
| `fixtures/lcir-fallible-async` | checked fallible stackless coroutines, ordinary managed `Result` completion, exact child source/contract fault propagation, cancellation, collision-free completed/live sum roots, balanced callback roots, forced moving-GC relocation, interpreter/checked-MIR/typed differential execution, Linux/MSVC objects, 32-bit fail-closed behavior, and real `check/build/test/run` commands |
| `fixtures/lcir-scalar-builtins` | source-backed integer parsing and exact parse-result sums, typed Float parsing and formatting, explicit source-backed Int/Float conversions and errors, finite checks, direct Duration values, typed roots, and real check/build/test/run commands without universal values, integer/conversion runtime symbols, or an executor |
| `fixtures/lcir-typed-logging` | canonical structured logging through typed LCIR, including exact JSONL stderr, escaping, empty fields, and canonical TextMap key order |
| `fixtures/lcir-typed-io` | typed File and Socket Tasks, exact recoverable I/O results, deterministic resource cleanup, and real `check/build/test/run`; native objects require only the typed I/O/task/resource ABI and contain no universal File, Socket, close, or `Value` symbols |
| `fixtures/lcir-lexical-cleanup` | direct typed assertions and source contracts, checked-root and assumed-body boundaries, mutable invariant writeback, lexical `defer`, static-concept `scoped` disposal, exact LIFO/fault behavior, and real check/build/test/run commands without universal values or an executor |
| `fixtures/lcir-static-concepts` | concrete static method selection, conditional proof forwarding, associated-type normalization, direct host execution, and MSVC COFF emission without runtime witness or universal-value surfaces |
| `fixtures/lcir-dyn-unique` | closed-world unique-witness `dyn` erasure, direct calls, aggregate/List storage, dead conformance and method-slot elimination, real check/build/test/run, host execution, and Linux/MSVC object emission without runtime witness data |
| `fixtures/lcir-dyn-finite` | closed finite `dyn` catalogs as exact single-pointer immutable boxes, direct switch dispatch, copied-value isolation, nested projected mutable receiver writeback on normal and fault edges, moving-GC pressure, dead method-slot elimination, host differential execution, and Linux/MSVC object emission without witness pointers or a runtime registry |
| `fixtures/std` | differential interpreter/native checks for structured values, text, maps, JSON, logging, GC, and async behavior; its native half is prepared through the production `Automatic` route, is required to select LCIR, and exercises real filesystem plus loopback-socket I/O |

Every admitted payload-bearing closed sum now uses the same bounded
target-data-derived byte-class carrier plan. Pack/unpack, active managed roots,
and List/TextMap repeated descriptors share that plan; recursive Json's compact
24-byte 64-bit representation is a consequence rather than a source-type
special case.

The workspace also has focused parser, semantic, MIR-validator, interpreter,
runtime, codegen, CLI, package, registry, cache, formatter, LSP, and hostile
input tests. Current cleanup evidence independently rejects noncanonical or
kind-mismatched `ResourceClose` inputs and checks that LLVM accepts only close
status `0`, trapping on every nonzero ABI status. File and Socket records carry
process-monotonic capability tokens; raw OS handles, stale tokens, sibling
tokens, and wrong-kind tokens cannot reach resource operations.
Runtime ownership tests cover successful completed-result transfer before
child retirement, root-Task retention in the executor-owned task registry, no
transfer for fault/cancellation/loser/unconsumed paths, opposite-kind close
rejection, deterministic ledger cleanup before retired-child memory
reclamation, and cleanup even when a typed disposer reports a defect.

## Toolchain features

| Capability | Status and evidence boundary |
| --- | --- |
| `check/build/test/run` | Implemented for the tested Core and package fixtures on both backends. |
| Finite value layouts | Semantic analysis and independent checked-MIR validation reject direct, mutual, generic, and non-regular by-value nominal cycles. `List`, `TextMap`, `Task`, and `dyn C` storage are the explicit indirection boundaries; no backend inserts a hidden box. |
| Code generation IR foundation | Implemented for the direct slices listed below. Native preparation is atomic. Unsupported artifacts may select the complete checked-MIR route only when the reachable graph contains no LCIR-only primitive; reachable File/Socket I/O fails closed instead of using that fallback. |
| Native LLVM executable | Implemented for Linux x86-64 and macOS arm64 release closures, with macOS also covered by the development gate; the Windows x86-64 release entry must pass before release support is claimed. |
| Interpreted executable artifact | Implemented with strict cache/executable kind separation, selected-entry definition closure, dense identity remapping, deterministic bytes, complete decode validation, and CLI tests. |
| Portable `.loomlib` | Source/interface format v3 is implemented and release-gated; consumers recompile embedded module source, and the artifact is not a native library or stable ABI. |
| Directory packages and dependencies | Manifest and lock schema 2 derive packages from directories, exclude dependency tests, and cover path, exact Git/fork, registry, and portable-artifact sources. |
| Local and HTTPS registry | Implemented with authentication, digest verification, bounded downloads, offline validated cache, and hostile-cache tests. Registry-version immutability remains a server protocol requirement. |
| Persistent compiler cache | Implemented for parse/interface/typed state/checked MIR/route-specific native object/portable artifacts; proof-bearing typed/MIR layers intentionally rebuild from source to preserve cold/warm proof elimination and route identity, canonical `MustScope` identity is rederived from current module-qualified HIR rather than trusted from typed-state bytes, and native final link is intentionally uncached. |
| Debug source info | Complete typed LCIR artifacts retain direct emission for `debug` and carry source functions, target-laid-out product and physical return types, stable `argN` parameter locations, artificial status/writeback/fault-context state, and instruction locations. Focused Linux DWARF, macOS dSYM/LLDB, and MSVC CodeView/PDB checks remain available, but debugger-container inspection is not part of the lean development gate. Eligible unsupported reachable artifacts use the complete checked-MIR route; LCIR-only boundaries fail closed. |
| LSP | Built and tested as a workspace crate; this status does not claim editor-specific distribution. |
| Formatter | Implemented with write/check modes and CLI tests. |
| Native cross object | Tested with an alternate 64-bit Linux triple; arbitrary triples remain conditional on the installed LLVM targets. |
| Cross executable | Implemented only through an exact runtime bundle plus explicit linker; the repository does not publish a general cross-runtime catalog. |

### Typed LLVM route

Production native preparation performs one whole-artifact MIR-to-LCIR attempt
and independently validates the result before typed LLVM emission. Current
direct coverage includes:

- scalar, managed Text, typed Bytes, and one-field typed Path operations,
  structural tuples, concrete records, refined values, closed sums, Lists,
  compiler-private TextMaps, and bounded concrete generic instances;
- canonical recursive Json formatting into the exact
  `Result[Text, JsonError]`, including typed Text publication and ordinary
  depth/non-finite error values;
- canonical structured logging with direct Text and `TextMap[Text]` values;
- all seven File/Socket operations in both recoverable
  `Task[Result[T, IoError]]` and faulting `Task[T]` forms, using one typed runtime
  request/outcome wire and exact compiler-generated coroutine frames;
- supported contracts and proof replay, static concepts, closed dynamic-concept
  catalogs, exact moving-GC roots, and static lexical cleanup, including exact
  cataloged canonical `File`/`Socket` close validation and fail-closed status
  classification;
- checked stackless coroutines, typed Task handles, exact suspension rows,
  fallible timers, executor-owned roots, and whole-value affine Task-bearing
  products, sums, and proven transparent wrappers, including atomic consuming
  splits of ordinary direct structural tuples into all fields;
- fixed and runtime-width homogeneous List forms of the four standard-library
  Task policies, including stored heterogeneous `Task.all`, sole-List-literal
  specialization, affine `List[Task[T]]` transfer, exact `TaskOutcome[T]`
  capture, completed-result resource transfer before child retirement, winner
  finalization, cancellation, and draining.

Async `requires` checks run in child state zero. A created Task carries its
creation-site blame, an async root carries its declaration span, and
`TaskCreate` does not inherit child fault effects. Core gains no `all`, `any`,
`settled`, or `race` syntax. HIR keeps these as ordinary method calls; semantic
resolution currently maps only canonical, unshadowed Task API members through
the temporary catalog to `TaskIntrinsic`, and MIR consumes that identity
without re-reading source names. This is implementation debt, not the target
library boundary. The source migration resolves each public member to an
ordinary `std` source `DefId`, lets reachability follow its body into private
scheduler primitives, and deletes the catalog and `TaskIntrinsic` rather than
mapping source definitions back to them.

Remaining atomic fallback includes dynamic producers with unresolved parameters
or projections, unsupported proof or contract value shapes, raw readiness, and
protected or otherwise unrepresentable projected inout shapes.
Finite closed dynamic catalogs are exact managed-pointer coroutine parameters,
results, and suspension-live values, including nested aggregate and List
carriers. Pure reads through already-established
invariant-protected products lower to exact typed `ProductExtract` chains;
protected interior mutation and moves remain closed.

## Automated quality evidence

The macOS development gate runs formatting, Clippy with warnings denied, the
workspace tests once, standard-library tests on both backends, and repository
documentation checks. It prepares one explicit runtime sidecar for native
integration tests. The cross-platform release matrix owns Linux, macOS, and
Windows archive closure. Fuzz campaigns, debugger-container inspection, and
base-versus-candidate benchmarks are explicit rather than universal pull-request
jobs.
The controlled runner prepares every native object through the production
router, and requires the constraints/contracts, concepts/polymorphism,
async/resources, async generic-contract,
typed logging, typed JSON formatting, and complete C3 fixtures to select LCIR
for their prepared main or test artifacts. The typed logging gate also checks
the exact JSONL standard-error bytes for both run and test artifacts; the typed
JSON gate executes all canonical formatting and error cases through each
artifact.
The broader standard-library fixture, including JSON parsing, selects LCIR with
no route allowance. `std.json.parse_json` and its bounded helper graph are
ordinary Loom source; the compiler and runtime contain no parser builtin,
opcode, or ABI. Recoverable file and socket operations create typed Tasks with
exact `Result[T, IoError]` coroutine frames, and `IoError.kind()` and
`IoError.message()` are ordinary direct product projections in LCIR.
The dedicated typed-I/O CLI fixture closes real `check`, object `build`,
`test`, and `run`, and inspects the object for the typed task/I/O/resource
symbols and the absence of every former universal I/O symbol. The integrated
standard-library native test prepares through `NativeRoutePolicy::Automatic`,
asserts the production route is LCIR, and runs real file round trips and
loopback Socket reads and writes. These are host test results, not a new
cross-platform release claim.
The format-neutral `Text.from_utf8_units(List[Int])` source API, interpreter
semantics, typed LCIR instruction, direct LLVM lowering, and typed runtime ABI
support efficient source-defined text construction.
The typed Path slice likewise closes `Path.from_text`, `Path.as_text`, and
`Path.join` without a runtime Path object: construction and extraction are
non-collecting, join stages both Text inputs before moving-GC allocation, and
the native object contains neither untyped path-helper symbols nor an executor.

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
