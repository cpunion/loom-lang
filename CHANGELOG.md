# Changelog

All notable user-visible changes to Loom will be documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Loom is experimental and does not yet promise stable source, standard-library,
artifact, or runtime compatibility.

## [Unreleased]

### Added

- `NativeRoutePolicy::LcirOnly`, which turns incomplete whole-artifact LCIR
  coverage into a structured `NativePreparationUnsupportedLcir` error and
  retains the complete deterministic `SupportReport` for tooling and gates.
- An English documentation structure for installation, first use, core
  language features, packages, contribution, security, and project direction.
- A pull-request-only contribution policy and private security-reporting path.
- Verified Linux and macOS native CI coverage, plus a complete Windows native
  job whose support claim remains gated on successful runner evidence.
- A Windows x86-64 release-matrix entry that stages the native `.exe` tools and
  runtime `.lib`, runs release smoke gates, and produces a checksummed `.zip`;
  availability remains gated on successful Windows runner evidence.
- Trusted Linux and macOS base-versus-candidate benchmark comments with one
  exact comparison table and visible runtime-index charts.
- Explicit, checksummed runtime bundles for native linking, including strict
  target, ABI, archive, and linker validation.
- A side-by-side typed moving-heap ABI with exact fixed-pointer descriptors,
  an independent direct-pointer shadow stack, strict shared root limits, and
  forced-relocation evidence without constructing an executor.
- A versioned `typed-repeated-v1` allocator for tagless monomorphized
  containers. The collector copies bounded header and per-element pointer
  maps, owns capacity outside object bytes, and precisely rewrites repeated
  managed cells. This advances the collector identity to `gc-v9` and native
  runtime ABI to component 13 with `runtime-v7` while retaining the fixed
  `typed-gc-v1` wire.
- A typed `Text.get` runtime boundary which stages one Unicode scalar before
  its allocation safepoint, returns missing indices without allocating, and
  publishes a direct managed Text pointer without a universal `Value`. This
  advances Text identity to `text-v3` and native runtime ABI to component 14
  with `runtime-v8`; the collector remains `gc-v9`.
- A compiler-distributed, read-only `standard` source package compiled through
  the ordinary frontend, MIR, reachability, and native pipelines. The initial
  source set includes `standard.int`, which provides `minimum` and `maximum`,
  and `standard.resource`, which publicly declares `Dispose`, `MustScope`, and
  `NoSuspend`. Resource declarations now pass through the ordinary source
  pipeline, while their fixed shapes and lexical static rules remain in the
  language core and add no runtime registry. Unreachable functions are absent
  from native artifacts. Exact embedded source bytes and the language version
  form the versioned `loom-source-stdlib-v1/<sha256>` cache identity; the
  `standard` package name, dependency alias, and complete `standard.*` module
  namespace are reserved from user replacement.
- `TextMap.entry_at(index) -> Option[(Text, V)]`, exposing checked read-only
  enumeration in canonical UTF-8 key order. Negative and out-of-range indices
  return `None`; typed LCIR reuses the existing exact indexed-entry operation,
  so this adds no JSON special case or runtime ABI.

### Changed

- Interpreted MIR advances to version 26. Generic compiler-cache artifacts now
  require a null `entry`, while executable artifacts require a fixed string
  entry. The two decoders reject the opposite artifact kind explicitly before
  validating its program body.

- Interpreted MIR advances to version 25. Semantic analysis now resolves
  `Dispose`, `MustScope`, and `NoSuspend` only from the compiler-owned standard
  package, lowering consumes those nominal `DefId` identities without
  reconstructing source names, and assigns distinct MIR identity tags. Tags
  paired with prelude ids grant resource semantics; module/name and fixed
  shapes are consistency checks only. Interpreted artifact encoding and
  decoding require the complete canonical identity trio.

- Portable `.loomlib` artifacts now use source-package format version 2. They
  contain the resolved non-standard package graph, exact Loom sources, and
  canonical public interfaces, but no checked MIR, producer-local proof state,
  or compiler-owned standard-library implementation. The decoder validates
  bounded structure, identities, portable paths, and interface fingerprints;
  artifact reads are bounded before allocation, and per-package Merkle
  identities close lockfile checks over shared transitive source content;
  the consumer supplies its matching standard library and repeats parsing,
  type checking, proof search, lowering, and MIR validation. Version 1
  artifacts must be rebuilt. The final-artifact cache layer advances to
  `portable-library-artifact-v3`; this redesign does not change `.loomi` proof
  replay or further advance persistent cache schema 4 or the checked-MIR cache
  envelope.
- Source snapshots now distinguish filesystem, portable-library, and
  compiler-owned standard-library provenance. LSP navigation and mutation
  errors preserve read-only policy while reporting the actual source owner
  instead of describing compiler-owned standard code as a portable artifact.
- Native debug compile units choose a root workspace source before dependency
  or compiler-owned sources, preserving user source paths after the standard
  library became an ordinary embedded Loom package.
- Recursive structural equality now closes through compiler-generated,
  type-specialized `StructuralEquality` LCIR functions. Each helper expands one
  representation layer and uses ordinary effect-free direct calls for nested
  products, sums, Lists, and TextMaps, so recursive user records and `Json`
  require no universal comparison value or JSON-specific runtime ABI. The
  recursive-equality fixture executes deep Node/List and Json/TextMap cases in
  interpreter and native main/test modes. Callable identity changes advance
  the LCIR dump to 33, artifact schema to 34, native-object domain to v30, and
  LLVM object-cache domain to v35.
- `format_json` now lowers through the typed LCIR `json.format` instruction.
  The LLVM backend supplies an exact `LoomTypedJsonLayout` for the canonical
  recursive `Json` carrier and calls `loom_runtime_json_format_typed_v1`;
  traversal and canonical byte staging do not construct a universal value,
  and only the final Text publication crosses one typed-GC allocation
  safepoint. `DepthLimit` and `NonFiniteNumber` remain ordinary `JsonError`
  values in the exact `Result[Text, JsonError]` sum. The
  `lcir-json-format` fixture covers all six Json variants, canonical object-key
  order, string escaping, negative zero, the depth limit, NaN, and both
  infinities through real main/test `check/build/test/run` closure. This
  advances the LCIR dump to 32, artifact schema to 33, native-object domain to
  v29, LLVM object-cache domain to v34, and native runtime ABI to component 22
  with `typed-json-v1` and `runtime-v16`; the public standard-library ABI
  remains v4.
- `Unit` remains the user-visible zero-sized type and value, including in
  fields, parameters, tuples, `Task[Unit]`, `Result[Unit, E]`, and `Ok(Unit)`.
  Callable syntax now treats only the fixed `Unit` result as implicit:
  functions, tests, async functions, methods, and concept requirements must
  omit a bare `Unit` return annotation, and callable bodies must omit a direct
  final bare `Unit` expression. An omitted return is still statically fixed to
  `Unit`; it is not inferred from the body. The parser reports both prohibited
  forms as ordinary syntax errors while preserving lossless recovery. This is
  a source-syntax change only and does not change HIR/MIR/LCIR, serialized
  artifacts, native ABI, runtime ABI, or cache schemas.
- Synchronous functions may now construct and return typed Tasks while staying
  ordinary non-suspending LCIR functions. Exact `NEEDS_EXECUTOR` effects add a
  compiler-private executor parameter after any fault context, and direct or
  fallible synchronous calls forward the current coroutine's executor through
  arbitrarily nested helpers. `TaskCreate`, first-class fixed `Task.all`, and
  `Task.sleep` reuse that borrowed context; helpers never create, drive, or
  destroy an executor, and `.await` remains async-only. Non-coroutine run/test
  roots requiring an executor fail closed before unsupported-route fallback and
  again at the checked-artifact boundary. The `lcir-sync-task-helpers` fixture
  covers interpreter and typed native execution, exact fault propagation,
  debug metadata,
  Linux/MSVC objects, real `check/build/test/run`, and the production quality
  gate without a universal value. Existing LCIR effects and the backend build
  fingerprint already distinguish the admitted ABI, so serialized, object,
  cache, and runtime versions do not change.
- All five canonical structured-logging calls now lower through typed LCIR.
  `LogWrite` is an explicit fallible normal/fault terminator, and LLVM passes
  direct Text plus canonical `TextMap[Text]` entries to the synchronous,
  non-collecting `loom_runtime_log_typed_v1` boundary. No universal Value,
  scheduler, or untyped root ABI is involved. Exact interpreter, legacy, and
  typed JSONL output is gated on host execution and Linux/MSVC object emission.
  This advances the LCIR dump to 31, artifact schema to 32, native-object
  domain to v28, object-cache domain to v33, and native runtime ABI to
  component 21 with `typed-log-v1` and `runtime-v15`; stdlib ABI remains v4.
- `Task.sleep`, `Task.all`, `Task.settled`, `Task.any`, and `Task.race` now stay
  ordinary method calls in HIR. Semantic analysis resolves only canonical Task
  members to stable compiler-owned standard-library identities, and MIR
  specialization consumes those identities without matching source spelling.
  Local variables, parameters, and user methods named `Task` or like a Task
  policy retain ordinary call semantics. Unreachable policy calls create no
  executor or LCIR artifact-identity edge. Typed semantic facts now encode the
  new identity, advancing persistent cache schema/domain to 4/v4 and the embedded
  standard-library identity to v3; checked MIR, LCIR, native-object, and runtime
  ABI versions are unchanged.
- Compiler-generated native run and test output now uses the versioned
  `loom_runtime_stdout_write_v1(data, length)` boundary in both the typed LCIR
  and legacy emitters. Each call supplies the complete UTF-8 line, including a
  literal LF byte, and its exact byte length; the runtime neither scans for
  NUL, appends a delimiter, nor applies C-runtime text-mode translation. A
  failed write or flush may have emitted a prefix, so generated code fails
  closed without retrying: output failure turns an otherwise successful run or
  passed test nonzero, while an already failing path preserves its original
  nonzero result. The boundary first flushes any buffered Rust stdout prefix.
  On Unix it ignores `SIGPIPE` so a closed pipe returns the same failure status
  instead of terminating the generated C entry by signal. A pure LCIR
  executable may therefore reference this one
  output-only runtime symbol without constructing a Loom runtime or executor.
  The obsolete legacy arbitrary-value root printer is removed, and the raw
  native object boundary now independently requires the complete executable
  root signature `() -> Unit`. This advances the native runtime ABI to
  component 20 with `stdout-v1` and `runtime-v14`. Serialized LCIR and artifact
  schemas are unchanged because harness output is not LCIR; object and cache
  domains are unchanged because the exact runtime identity already participates
  in native fingerprints and runtime-bundle validation.
- Typed coroutine frames now admit canonical one-pointer `List[T]` and
  compiler-private `TextMap[V]` values in parameters, results, nested products,
  and suspension-live rows. Their repeated element graphs remain governed by
  the existing exact collection descriptors; the frame records only the
  managed pointer and its per-state liveness. The
  `lcir-async-managed-collections` fixture closes moving-GC pressure, debug
  metadata, 32-bit fail-close classification, Linux/MSVC object emission, and
  real `check/build/test/run`. This is an admission change over existing LCIR
  representations and typed-task/GC wires, so no serialized format, cache
  schema, object domain, or runtime ABI version changes.
- Typed LCIR now carries `MAY_FAULT` through cleanup-free, non-inout stackless
  coroutines. Checked arithmetic, assertions, ordinary fallible invokes, and
  caller-side `requires` or callee-side `ensures` report their exact primary
  source/contract metadata on the active Task; an awaiting parent inherits the
  Task's `Faulted` or `Cancelled` state instead of manufacturing a source
  `Result`. Closed sums, including managed `Result[Text, E]`, remain ordinary
  completed values. The collision-free sum carrier supplies exact static
  pointer offsets for parameters, suspension rows, and completed Task results;
  inactive pointer lanes remain zero. The `lcir-fallible-async` fixture proves
  `Ok` and `Err` completion, active-tag shadow-root rebuilds, completed-result
  and parent-frame relocation under two independent allocation-pressure
  phases, child-fault inheritance, sibling cancellation, balanced typed root
  frames on every callback exit, both native route policies,
  interpreter/legacy/typed differential behavior, Linux/MSVC objects, 32-bit
  fail-closed behavior, and real `check/build/test/run`. This uses the existing
  typed-task and fault-context runtime ABI while advancing the compiler-private
  identity to LCIR dump 23, artifact schema 24, native-object domain v20, and
  LLVM object-cache domain v25.
- Typed LCIR now lowers the first complete stackless-coroutine slice instead of
  routing it through the universal emitter. Infallible async functions whose
  parameters, results, and suspension-live values have direct scalar, product,
  refined, or Text shapes use a checked `CoroutinePlan`, a non-GC `Task[T]`
  handle, explicit `task.create`/`task.await` control flow, and target-laid-out
  frames with immutable exact root descriptors. LLVM resumes those frames
  through the existing typed-task and structured-join runtime ABI; async run
  and test roots own a real executor for the root Task lifecycle. The
  `lcir-typed-async` fixture closes real `check/build/test/run`,
  interpreter/legacy/typed differential execution, Linux/MSVC object emission,
  multiple awaits, deterministic immediate-ready completion of a pre-created
  second child, and forced moving-GC relocation of a parent Text root while its
  child runs. A zero join-suspend result now preserves the current `Running`
  activation and removes its redundant ready-queue entry before inline result
  taking. At that stage, async cleanup, inout/writeback, sleep/readiness, Task
  combinators, List/TextMap frame values, and dynamic concepts remained the
  reviewed Core03 legacy allowance. Together with the previously deferred
  typed-TextMap vocabulary, this advances the LCIR dump to 20, artifact
  identity to schema 21, native-object domain to v17, and LLVM object-cache
  domain to v22. The existing typed-task v1 and native runtime ABI are
  unchanged.
- Tagged LCIR sums now use one general target-data-derived carrier plan that
  prevents managed-pointer bytes from aliasing scalar or padding bytes across
  variants. The bounded deterministic planner places pointer-free variants
  first, then chooses the lowest aligned offset for each pointer-bearing
  payload; bytes of the same class may overlap to retain compact layouts. This
  closes exact moving-GC tracing for arbitrary admitted closed sums in Lists
  and TextMaps, including scalar/Text/product choices and sums nesting the
  canonical recursive `Json`. Json remains 24 bytes on supported 64-bit
  targets rather than growing to a fully disjoint carrier. The
  `lcir-sum-layout-collisions` and `lcir-typed-json` fixtures cover forced
  relocation, opposing pointer-first/scalar-first record variants,
  interpreter/legacy/typed differential execution, Linux/MSVC object emission,
  32-bit fail-closed classification, and real
  `check/build/test/run`. The implementation reuses the existing repeated
  allocator and shadow stack; it adds no universal value, executor, registry,
  or runtime symbol. The compiler-private identity advances monotonically to
  LCIR dump 22, artifact schema 23, native-object domain v19, and LLVM
  object-cache domain v24. Json equality and parsing remain separate typed-LCIR
  slices. Carrier storage is capped at 64 KiB. Independent
  artifact-wide 65,536-step placement and 65,536-payload-byte pack/unpack
  budgets prevent many legal wide sums or construct sites from multiplying
  search and bytewise LLVM IR work; checked source regressions exhaust both
  bounds before an object or partial IR file is produced.
- Typed LCIR now lowers `is_finite`, `parse_int`, `parse_float`,
  `format_float`, `milliseconds`, and `Duration.as_milliseconds` without a
  universal value or executor. Parse results use their exact closed sums;
  Duration is a direct product whose negative check uses canonical
  `Assert + FaultMetadata::Runtime`, preserving lexical cleanup and the first
  active fault. Float formatting publishes a direct managed Text pointer
  through exact live-after roots and remains correct under forced relocation.
  This advances the LCIR dump to 19, artifact identity to schema 20,
  native-object domain to v16, and LLVM object-cache domain to v21. The new
  `loom_runtime_format_float_typed_v1` boundary advances the native runtime ABI
  to component 15 with `format-float-v1` and `runtime-v9`; `text-v3` and
  `gc-v9` are unchanged.
- Typed LCIR now lowers value equality and inequality for concrete tuples,
  generic and nongeneric records, established refined values, closed sums, and
  finite List-backed structural graphs. Products compare fields in order;
  sums dispatch both operands before reading only their active payloads; Lists
  compare length and then elements through a nonallocating checked-read loop.
  The same lowering is available in `requires` and `ensures`. Text remains
  content equality, Float retains IEEE ordered equality/unordered inequality,
  and no universal value, executor, or new runtime boundary is introduced.
  Recursive nominal equality reached again through a List remains atomic
  fallback until LCIR has reusable recursive comparison instances. The new
  `lcir-structural-equality` fixture closes real `check/build/test/run`, host
  interpreter/legacy/typed differential execution, allocation-pressure GC
  evidence, and Linux/MSVC object emission. Existing LCIR, artifact, object,
  cache, and runtime ABI versions are unchanged because equality expands into
  already-versioned typed instructions and control flow.
- Typed LCIR now erases a reachable `dyn C` view directly to its concrete value
  when the closed-world concept-and-associated-binding witness set proves one
  closed nongeneric conformance. Dynamic requirements become direct typed
  calls, mutable interface parameters preserve normal and fault-path writeback,
  and the same erasure now applies recursively inside products, closed sums,
  and managed Lists. A `List[dyn C]` therefore uses the selected concrete
  element layout and repeated pointer map; field projection, sum matching,
  checked List reads, forwarding, and logical copies remain ordinary typed
  value operations. Dead conformances or unused method slots never reach LLVM.
  Missing or open witness sets remain structured whole-artifact fallback; no
  runtime tag, witness pointer, conformance registry, universal value, or
  indirect call was added. The `lcir-dyn-unique` fixture and both
  Core02 main and test routes provide real CLI, host, legacy-differential,
  forced-relocation, copy-independence, and Linux/MSVC object evidence.
  Existing LCIR, artifact, object, cache, and runtime ABI versions are
  unchanged.
- Typed LCIR now closes a competing finite reachable witness set into a
  compiler-private one-pointer dynamic catalog. Every candidate has its own
  exact box layout, ordinal tag, and precise fixed-object GC descriptor;
  dispatch is a finite switch to direct methods, and DCE retains only called
  requirement slots. Readonly copies may share immutable boxes. Mutable calls
  allocate a fresh candidate box and write it back on both normal and fault
  exits, so aliases remain independent under forced moving collection. The
  `lcir-dyn-finite` fixture closes interpreter/legacy/typed differential
  execution, real CLI check/build/test/run, Linux/MSVC objects, and 32-bit
  fail-close without a fat pointer, witness table, registry, universal value,
  or indirect call. Missing witnesses and open, generic,
  prerequisite-dependent, or otherwise incomplete sets still select one
  structured fallback. The LCIR dump advances to 20, artifact schema to 21,
  native-object domain to v17, and object-cache domain to v22; runtime ABI
  component 15 is unchanged.
- Typed LCIR now lowers compiler-private concrete closed `TextMap[V]` values on
  64-bit targets as one managed pointer to exact typed repeated entries.
  Construction, functional insert/replacement/removal, length, containment,
  exact `get -> Option[V]`, and structural equality support scalar, Text,
  product, closed-sum, List, and nested TextMap values. Missing removal reuses
  the source value, successful removal preserves aliases, and equality compares
  canonical sorted entries rather than insertion history. Target-derived
  descriptors trace every Text key and managed value leaf through moving
  collection; collecting operations root and reload their exact inputs. The
  implementation reuses
  `typed-repeated-v1` and adds no universal value, executor, runtime type tag,
  callback registry, or map-specific runtime symbol. The
  `lcir-typed-textmap` fixture closes real `check/build/test/run`, forced-GC
  interpreter/legacy/typed differential execution, and Linux/MSVC object
  emission. The new operations advance the LCIR dump to 24, artifact schema to
  25, native-object domain to v21, and object-cache domain to v26; the native
  runtime ABI is unchanged.
- Typed LCIR now monomorphizes fully concrete generic records, invariant
  records, and refined wrappers into their exact direct product or transparent
  representations. Generic field projection, contract evaluation, calls,
  returns, and managed `Text` leaves remain typed SSA. That slice did not itself
  admit compiler-private `TextMap[V]`; the later typed-TextMap slice above does.
  The new `lcir-generic-products` fixture closes real
  `check/build/test/run`, host execution, legacy differential, and Linux/MSVC
  object evidence without a universal value or executor. Existing LCIR,
  artifact, object, cache, and runtime ABI versions are unchanged because this
  extends accepted source coverage without changing their formats.
- Typed LCIR now lowers concrete closed managed `List[T]` values on 64-bit
  targets through direct typed repeated storage, including literals, immutable
  value-semantic append, length, and `get -> Option[T]`. Exact target-layout
  pointer maps support Text, products, sums, and nested Lists; collecting growth
  roots and reloads old backings and managed elements. A checked-MIR-only,
  independently validated uniqueness certificate gives canonical local append
  loops geometric capacity reuse without exposing mutation to copies or
  aliases. This advances the LCIR dump to 17, artifact identity to schema 18,
  native-object domain to v14, and CLI object cache to v19 while reusing native
  runtime ABI component 14 and `typed-repeated-v1`.
- `ConstraintError` now has one validated compiler-private six-field MIR shape,
  and its `value_summary` is type-only in interpreted and native execution. It
  cannot disclose scalar values, text or byte contents, sizes, enum variants,
  collection counts, or nested business data. This advances interpreted MIR to
  version 23; persistent-cache schema 3 remains valid because checked-MIR cache
  envelopes independently carry the artifact version.
- Serialized refined and concrete invariant-record proof rechecks now remain on
  the typed LCIR route, including generic record instantiations. Generic
  function arguments and record-definition parameters are substituted in
  their separate namespaces, including lexical contract-match bindings. The
  embedded predicate executes before nominal publication, rejection produces
  the canonical `ArtifactProofRejected` `RuntimeFault` with the exact
  construction span, and lexical cleanup uses the existing typed fault edge.
  The original nongeneric slice advanced the LCIR dump to 18, artifact identity
  to schema 19, LCIR native-object domain to v15, and LLVM object-cache domain
  to v20; the generic coverage expansion changes no format or runtime ABI.
- Nongeneric refined and invariant runtime construction now remains on typed
  LCIR and returns the exact `Result[..., ConstraintError]`. Rejection builds
  the validated six-field, disclosure-safe error value; acceptance publishes
  the nominal value only after its predicate succeeds. Generic or
  unsupported-shape runtime construction remains atomic fallback.
- Typed LCIR now places source contracts at explicit checked boundaries.
  Closed-world calls evaluate every argument before checking `requires` with
  exact call-expression blame, then enter an assumed body whose receiver
  invariant is checked at entry. Normal tails and explicit returns run lexical
  cleanup before checking the current receiver invariant and `ensures`, while
  checked contract arithmetic retains its ordinary runtime faults. Typed
  `old` snapshots, managed Text/product/sum values, exhaustive contract matches,
  and concrete static-concept calls remain direct SSA without a universal value
  or executor. This advances the LCIR dump to 16, artifact identity to schema
  17, native-object domain to v13, and CLI object cache to v18 without changing
  native runtime ABI component 14.
- The controlled Core 0.1 evidence now requires both its main and negative-test
  graphs to select typed LCIR; the runtime-construction legacy allowance has
  been removed.
- The controlled quality runner now compiles every native scenario through the
  same prepared Automatic route used by production commands. Its version 2
  evidence records the expected and actual route for each object, requires the
  typed fixture to remain LCIR, and permits legacy only through reviewed named
  allowances for fixtures whose missing typed coverage is explicit. This is an
  evidence-schema change only; LCIR artifacts, object domains, caches, and the
  native runtime ABI are unchanged.
- Typed LCIR now lowers `Text.get(Int)` directly to a collecting scalar-
  selection instruction returning the canonical managed `Option[Text]` sum.
  Missing or negative indices do not allocate; successful Unicode-scalar
  selection uses the typed runtime boundary, exact live-after roots, and a
  fail-closed status guard without a universal value or executor. This
  advances the LCIR dump to 15, artifact identity to schema 16, native-object
  domain to v12, and CLI object cache to v17; native runtime ABI component 14,
  `text-v3`, `runtime-v8`, and `gc-v9` are unchanged.
- Checked MIR now carries source-module provenance and a versioned
  compiler-known identity for the canonical `standard.resource.MustScope`
  marker. Artifact and cache validation independently require that qualified
  concept, its identity tag, the prelude id, and the empty marker shape to
  agree, while same-named concepts in other modules remain ordinary. This
  advances interpreted MIR to version 22 and the persistent compiler cache to
  schema 3 without changing source resource or cleanup semantics, LCIR, the
  native ABI, or the runtime ABI.
- Typed LCIR now keeps closed unboxed sums direct when a variant contains
  managed `Text`, including through nested products and sums. Each live sum
  receives deterministic candidate leaf cells; tag guards publish only the
  active variant and clear every inactive candidate, while post-GC reload
  rebuilds only the active payload from zero-initialized carrier bytes. This
  advances the LCIR dump to 14, artifact identity to schema 15, native-object
  domain to v11, and CLI object cache to v16. It reuses typed-shadow-stack v1
  and does not change native runtime ABI component 13 or `gc-v9`.
- Typed LCIR now expands source assertions, `defer`, and `scoped` disposal into
  direct lexical control flow. Cleanup registers only after its statement or
  scoped initializer succeeds, runs in strict LIFO order on normal block exit,
  return, and fault, preserves the first fault while suppressing later cleanup
  faults, and treats each branch body as an independent scope. Static-concept
  disposal is a monomorphic typed call with functional writeback; File and
  Socket disposal uses a new `typed-resource-v1` close ABI without a universal
  value, runtime cleanup stack, or synchronous executor. This advances the
  LCIR dump to 13, artifact identity to schema 14, native-object domain to v10,
  CLI object cache to v15, and native runtime ABI to component 12 with
  `runtime-v6`.
- Checked MIR now treats exhaustive pattern matching as an affine
  decomposition boundary for resource-bearing carriers. This restores
  `scoped resource = fallible_task.await?` and explicit `Result[File, E]` or
  `Result[Socket, E]` handling while still rejecting wildcard loss,
  unconsumed payloads, projected transfers, and matching an active scoped
  resource.
- The controlled release-quality runner now uses independent loopback I/O
  fixtures for interpreter and native execution, so one backend cannot retain
  a connection or consume the other backend's expected server slot. Native
  failures also retain structured fault metadata, and the Core 0.3 fixture
  digest is synchronized with its reviewed source.
- Failed native links now retain bounded diagnostics written to both standard
  output and standard error. This preserves MSVC `LINK` errors that Clang
  otherwise followed with only a generic failure summary.
- Windows host codegen now uses LLVM's deterministic generic x86-64 policy
  instead of entering LLVM 19's host-feature probe. This
  also aligns compiler and runtime-bundle CPU policy during `runtime pack`.
- Windows x86-64 `runtime pack` now obtains its canonical compiler-target
  identity without constructing an otherwise unused LLVM target machine; the
  embedded LLVM 19 data layout is checked by a cross-target regression test.
- Native object emission now initializes LLVM through the native-target entry
  point. Explicit targets select only their architecture initializer when the
  linked LLVM distribution is partial, so unrelated packaged backends cannot
  affect host object emission.
- The Windows native gate now isolates target-machine construction from first
  object emission and emits bounded, non-sensitive LLVM stage markers, so a
  process-level LLVM failure identifies its exact compiler boundary.
- Windows compiler binaries now link the official LLVM 19 `LLVM-C.dll`
  import library instead of LLVM's static MSVC component closure. This removes
  the private static `libxml2s.lib` rebuild, keeps LLVM allocation and target
  APIs behind one runtime boundary, and packages the required DLL beside the
  release compiler.
- Windows native linking now closes both the writable construction handle and
  its temporary identity clone for each private runtime-archive snapshot before
  invoking Clang/MSVC. An independent read-only, read-shared identity anchor
  keeps every snapshot immutable without preventing parallel `link.exe`
  processes from opening their `.lib` inputs.
- Native `build`, `run`, `test`, and `debug` no longer obtain a runtime archive
  from the compiler build. Source builds must create a portable runtime archive
  and pack it beside `loomc`, or select a validated bundle explicitly.
- Interpreted MIR version 19 now treats source construction proofs as
  process-local: `.loomi` payloads replay their embedded predicate or
  invariant, while proof-bearing persistent compiler-cache layers rebuild from
  source to preserve cold/warm route and optimization behavior.
- Fresh-source proven refinements and record invariants use zero-check typed
  LCIR representations. Serialized proof rechecks preserve concrete nominal
  identity and use guarded typed LCIR whenever their representation and
  contract shape are supported; rejection retains canonical
  `ArtifactProofRejected` behavior.
- Reachable direct generic calls now form one bounded, deterministic LCIR
  instance closure with exact type and static-witness identity. Supported
  instances use direct typed LLVM signatures; nonregular or over-budget
  expansion selects whole-artifact fallback before LCIR construction.
- Eligible closed enums and exhaustive matches now use bounded typed LCIR
  lowering with direct native sum layouts. Float patterns follow IEEE ordered
  equality, including equal signed zeros and non-matching NaNs.
- Typed LCIR now lowers bounded nested record places through exact product SSA.
  Projected mutable receivers reconstruct the latest aggregate root on normal
  and fault edges without universal values, proxy storage, or runtime helpers.
- Typed LCIR now carries literal-proven `Text` values as one pointer to an
  immortal compiler-emitted object on 64-bit targets. Direct `length`,
  `contains`, and content equality require no universal value, GC root, or
  executor. This initially advanced the LCIR dump to 7, artifact identity to
  schema 8, native-object domain to v4, and CLI object cache to v9 without
  changing the native runtime ABI.
- Typed LCIR function effects now use one explicit transitive capability set:
  `MAY_FAULT`, `NEEDS_RUNTIME`, `MAY_COLLECT`, `NEEDS_EXECUTOR`, and
  `MAY_SUSPEND`. Independent validation recomputes the least call-graph fixed
  point and rejects both missing and invented capabilities. This advances the
  LCIR dump to 8, artifact identity to schema 9, native-object domain to v5,
  and CLI object cache to v10; it does not add runtime, GC, executor, or
  suspension operations.
- Typed LCIR fault origins now distinguish integer runtime faults from exact
  `AssertionFault`, `PreconditionFault`, `PostconditionFault`, and
  `InvariantFault` metadata. Contract metadata carries bounded user code,
  canonical messages, and concrete contract/blame spans through validation,
  dumps, identity, and LLVM machine diagnostics. This advances the LCIR dump
  to 9, artifact identity to schema 10, native-object domain to v6, and CLI
  object cache to v11. At that stage, source contracts and assertions still
  selected atomic fallback pending their control-flow and cleanup lowering.
- Bounded concrete static concept calls now resolve to ordinary direct LCIR
  calls. Conformance head arguments, conditional prerequisite proofs, and
  method proofs remain part of exact instance identity, while associated-type
  projections normalize to their concrete binding before representation
  planning. No runtime witness, indirect call, universal value, GC, or executor
  surface is added. Open instance keys are rejected independently by the
  builder and validator. This advances the LCIR dump to 10, artifact identity
  to schema 11, LCIR native-object domain to v7, and CLI object cache to v12.
- Dynamic `Text.concat` and Text-bearing tuples/records now compile through
  typed LCIR. One artifact-wide direct pointer representation covers literals
  and moving results whenever concat or a Text-bearing product is reachable; the
  runtime stages both complete UTF-8 inputs before collection, initializes a
  pointer-free typed leaf, and publishes it last. Exact live-after SSA root
  maps expand an unboxed tuple/record to deterministic managed-leaf cells in
  the typed shadow stack, rebuild aggregate uses from relocated aliases, omit
  dead edge arguments and empty frames, and construct no universal value or
  executor. Products remain exact SSA structs rather than heap objects.
  OOM remains an uncatchable process fault and invalid helper status fails
  closed. `Text.get`, Text inside enums or transparent/refined carriers, and
  managed lists remain atomic fallback. This advances the LCIR dump to 12,
  artifact identity to schema 13, native-object domain to v9, and CLI object
  cache to v14. Product leaf rooting reuses typed-shadow-stack v1 and did not
  change native runtime ABI component 11 or its `runtime-v5` identity.
- Interpreted MIR version 20 permits projected moves. They return the selected
  leaf and consume the complete aggregate root, preserving a simple initialized
  or moved local state without partial-initialization compatibility.
- The native runtime ABI component advances to `10`. Its exact identity uses
  `text-v2` and `runtime-v4`, keeps `gc-v8`, and includes `typed-gc-v1` plus
  `typed-shadow-stack-v1`. Existing legacy GC symbols and behavior remain
  available within the new whole-toolchain ABI identity.

No historical release notes have been reconstructed. Future entries should
describe observable changes, not reproduce the Git commit log.
