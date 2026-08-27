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
| Windows x86-64 (Windows 2025) | configured native job | complete workspace | LLVM 19 native closure configured | host object path | `.zip` workflow entry configured; no verified archive |
| Other LLVM 64-bit triples | no general CI claim | host-dependent | only with a matching validated runtime bundle and linker | possible when LLVM provides the target | no |
| 32-bit triples | no | not a native claim | no runtime/executable support | feature-dependent direct LCIR object only when LLVM provides the target; `Text` and the legacy route reject | no |

The Windows CI job installs LLVM 19.1.7 and Rust 1.88, checks, lints, tests, and
builds the complete workspace, and runs the Core 0.1-0.3 check/build/test/run
loops on both backends. Native builds additionally require the expected `.exe`
and `.pdb` outputs, parse the PDB, and inspect a compiler-emitted COFF object for
CodeView sections before executing the artifact. The release job reuses the
same pinned LLVM bootstrap and is configured to stage `loomc.exe`,
`loom-lsp.exe`, the compiler's `LLVM-C.dll`, and `loom_runtime.lib`, execute
Core, C3, standard-library, and adjacent-runtime gates, and hash a `.zip`.
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
| `examples/core03` | moving GC, lexical cleanup, stackless async, joins, cancellation/readiness, and the same dual-backend loop |
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
| `fixtures/lcir-typed-textmap` | compiler-private direct `TextMap[V]` storage for scalar/Text/product/sum/List/nested-map values, immutable insertion/replacement/removal, containment, exact `Option[V]` lookup, insertion-order-independent structural equality, removal during forced moving-GC relocation, interpreter/legacy/typed differential execution, release IR, Linux/MSVC objects, 32-bit fail-close classification, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-async` | checked stackless coroutine frames for infallible scalar/product/Text async functions, typed Task handles and one-child await joins, exact suspension root maps, forced parent-Text relocation, interpreter/legacy/typed differential execution, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-sleep` | first-class checked `Task.sleep` with `Int` and `Duration` inputs, zero and positive deadlines, managed values live across later awaits, and typed-LCIR main/test native execution |
| `fixtures/lcir-typed-task-all` | direct multi-child awaits and stored exact heterogeneous `Task.all`, one-field and mixed Text/Int/Unit tuples, shared static-shape descriptors, atomic child adoption, moving-GC survival, interpreter/typed execution plus legacy/typed child-fault and sibling-cancellation comparison, release IR, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-task-any` | nonempty immediately awaited fixed homogeneous `Task.any`, deterministic nonzero winner selection, managed Text results, repeated loser cancellation and retirement, typed-object surface inspection, and real `check/build/test/run` commands |
| `fixtures/lcir-async-cleanup` | typed-LCIR `defer` and static-concept `scoped` cleanup across suspension, exact normal/fault/cancel live rows, LIFO normal cleanup, child-fault propagation, sibling cancellation, source-callback cancellation dispatch, interpreter/legacy/typed differentials, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-async-writeback` | synchronous functional inout calls inside typed coroutines, managed-Text writeback across moving collection, normal/fault/cancellation cleanup, fault writeback before lexical cleanup, by-value dynamic View parameters under mutable dispatch, recursive unique-witness `dyn` erasure across async parameters, results, and nested frames, multi-witness finite dynamic calls outside suspension rows, interpreter/typed differentials, Linux/MSVC objects, and real `check/build/test/run` commands |
| `fixtures/lcir-typed-json` | canonical recursive `Json` construction and matching through `List[Json]`/`TextMap[Json]` cycle breakers, the general collision-free closed-sum byte-class carrier, exact repeated tracing, immutable map aliases, forced moving-GC relocation, interpreter/legacy/typed differential execution, Linux/MSVC objects, and real `check/build/test/run` commands; equality, parse, and format remain follow-up slices |
| `fixtures/lcir-sum-layout-collisions` | unrelated and nested closed sums, including opposing pointer-first/scalar-first record variants, with scalar, product, Text, List, TextMap, and recursive Json payloads; one target-data-derived carrier plan drives pack/unpack, exact repeated descriptors, and forced moving-GC relocation across interpreter/legacy/typed differential execution, Linux/MSVC objects, 32-bit fail-closed emission, artifact-wide placement/emission bounds, and real `check/build/test/run` commands |
| `fixtures/lcir-fallible-async` | checked fallible stackless coroutines, ordinary managed `Result` completion, exact child source/contract fault propagation, cancellation, collision-free completed/live sum roots, balanced callback roots, forced moving-GC relocation, interpreter/legacy/typed differential execution, Linux/MSVC objects, 32-bit fail-closed behavior, and real `check/build/test/run` commands |
| `fixtures/lcir-scalar-builtins` | exact parse-result sums, finite checks, managed Float formatting, direct Duration values, typed roots, and real check/build/test/run commands without universal values or an executor |
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
| Code generation IR foundation | Production native preparation attempts one atomic whole-artifact direct MIR-to-LCIR lowering. The current slice covers primitives, literal-only, concatenated, and Unicode-scalar-selected direct `Text` on 64-bit targets, structural tuples, fully concrete acyclic generic and nongeneric records including managed Text leaves, fresh-source proven generic and nongeneric record invariants, established concrete refined values, nongeneric refined and invariant runtime construction returning exact `Result[..., ConstraintError]`, nongeneric portable proof `Recheck` with canonical typed runtime-fault guards, eligible concrete closed enums including managed Text payloads, concrete closed managed Lists with scalar/Text/product/sum/nested-List elements, compiler-private concrete closed TextMaps with scalar/Text/product/sum/List/nested-map values, bounded concrete direct generic function instances, and typed finite checks, integer/float parsing, Float formatting, and Duration operations. Async functions without explicit mutable parameters and with direct scalar/product/refined/Text or collision-free closed-sum frame values use checked stackless coroutine plans, typed Task handles, exact suspension roots, ordered single- and multi-child `AwaitTasks`, direct and stored nonempty fixed-arity heterogeneous `Task.all` with exact `TaskJoinAll` composites and atomic child adoption, nonempty immediately awaited fixed-arity homogeneous `Task.any` with exact winner selection and loser finalization, explicit fallible `Task.sleep` construction over a zero-root typed timer, static LIFO `defer` and admitted `scoped` cleanup across suspension, and real executor-owned run/test root lifecycles. Coroutine bodies may call synchronous functional inout functions; their normal and fault writebacks update the frame-local SSA environment, and fault writeback precedes active lexical cleanup. A dynamic View parameter is copied by value into the Task frame rather than exposed as an inout signature. Every await has explicit normal, fault, and cancel edges over one identical exact live row; normal additionally receives mode-derived exact results, child fault activates source fault and cleans up before `ResumeFault`, and cancellation preserves inactive source fault and cleans up synchronously before `TaskCancelled`. Their `MAY_FAULT` paths preserve checked arithmetic, assertion, ordinary fallible-invoke, caller-side precondition, callee-side postcondition, and child-task primary fault metadata; source `Result` values, including managed Text results, remain ordinary successful Task completion. Direct `==`/`!=` expands through these scalar, Text, product, refined, sum, and finite List/TextMap-backed shapes in ordinary expressions and contracts; sum comparison reads only active payloads, while List and TextMap comparison use nonallocating proved loops over exact option values. Concrete static concept calls become ordinary direct typed calls. A dynamic view whose reachable concept-and-binding witness set proves exactly one closed nongeneric conformance is erased recursively to that concrete LCIR type in signatures, products, closed sums, generic arguments, managed Lists, and coroutine parameters, results, and recursively nested admitted frame shapes. Two or more exact artifact-closed conformances use one managed pointer to a candidate-specific precise box; a compiler-private ordinal switch reaches ordinary direct calls, and unused conformances or requirement slots remain absent. Records, sums, and Lists store the pointer directly. Readonly copies may share an immutable box; mutable dispatch performs concrete inout work followed by fresh-box writeback on normal and fault exits, preserving copy independence under moving GC. No witness pointer, fat pointer, runtime registry, universal value, or indirect call enters the artifact. Dynamic Text concat/get, Float formatting, and supported Text-bearing aggregates select one artifact-wide managed Text provenance mode; products and sums remain unboxed SSA, while exact live-after guarded leaves use the typed shadow stack without a universal value. `Text.get` returns the canonical managed `Option[Text]`, with nonallocating missing indices and a collecting found path. List literals, immutable append, length, and `get -> Option[T]` use exact repeated descriptors; independently validated unique local loops reuse geometric capacity. TextMap construction, functional insert/replace/remove, containment, length, exact `get -> Option[V]`, and structural equality reuse sorted entry semantics and the repeated allocator with precise key/value tracing and no universal map ABI. Tuple/product/sum SSA, bounded nested places, functional mutation/writeback, exhaustive match DAGs, canonical typed assertions, lexical `defer`, and `scoped` StaticConcept/File/Socket disposal remain part of the checked boundary. Source contracts use checked-root/assumed-body instances, exact call-site precondition blame, entry receiver invariants, typed `old` snapshots, post-cleanup exit invariant/postcondition checks, and the same exact managed-root analysis. Cleanup suffixes are static LIFO CFG on normal, return, fault, await-fault, and await-cancel exits; no runtime cleanup stack or synchronous executor is used. Complete artifacts use independently checked LCIR and its typed LLVM emitter; only reachable `Unsupported` input selects the complete legacy graph. Missing witnesses, open or generic/prerequisite-dependent dynamic sets, derived dynamic proof conversions, generic or unsupported-shape proof replay and runtime construction, contracts over unsupported value shapes, recursive nominal equality reached through Lists or TextMaps, Text inside transparent/refined carriers, other managed values, recursive or open sums, async root preconditions, explicit mutable coroutine parameters, raw readiness, dynamically sized Task joins, every `Task.settled` and `Task.race`, stored or otherwise first-class `Task.any`, List/TextMap coroutine frames, finite-catalog or open dynamic-concept coroutine frames, nested protected projected inout, and managed projected inout remain atomic fallback. |
| Native LLVM executable | Implemented and CI-tested on Linux x86-64 and macOS arm64; a complete Windows x86-64 native CI gate is configured and must pass before release support is claimed. |
| Interpreted executable artifact | Implemented, versioned, decoded, validated, and exercised by CLI tests/CI. |
| Portable `.loomlib` | Implemented and release-gated; not a native library or stable ABI. |
| Manifest/lock/features/path dependencies | Implemented with resolver and CLI integration tests. |
| Local and HTTPS registry | Implemented with authentication, digest verification, bounded downloads, offline validated cache, and hostile-cache tests. Registry-version immutability remains a server protocol requirement. |
| Persistent compiler cache | Implemented for parse/interface/typed state/checked MIR/route-specific native object/portable artifacts; proof-bearing typed/MIR layers intentionally rebuild from source to preserve cold/warm proof elimination and route identity, canonical `MustScope` identity is rederived from current module-qualified HIR rather than trusted from typed-state bytes, and native final link is intentionally uncached. |
| Debug source info | Linux DWARF and macOS dSYM metadata are checked in CI. Complete typed LCIR artifacts retain direct emission for `debug` and carry source functions, target-laid-out product and physical return types, stable `argN` parameter locations, artificial status/writeback/fault-context state, and instruction locations; macOS LLDB verifies a fallible parameter and physical step-out result. MSVC objects select LLVM CodeView metadata and links use `/DEBUG` plus an explicit staged `/PDB:` path; the configured Windows gate inspects typed-LCIR COFF/PDB structures, but no source-level Windows debugger session is claimed yet. Unsupported reachable artifacts use the complete legacy route. |
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
The controlled runner prepares every native object through the production
router, and requires Core 0.1, Core 0.2, and the complete C3 repository to
select LCIR for both main and test artifacts. The remaining reviewed legacy
allowances cover async and standard-library gaps.

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
