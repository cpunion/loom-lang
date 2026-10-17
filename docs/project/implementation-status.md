# Implementation status

Loom has one self-hosted native compiler, a source-written standard library and
usable CLI/editor tooling. N0 and N1 are met for the documented subset on macOS,
Linux and Windows; N2 remains in progress and N3 has bounded prototypes.
This is not completion of the [project goals](charter.md).
The [roadmap](../../ROADMAP.md) defines the remaining gates; the
[compiler guide](../../compiler/README.md) is the detailed implementation reference.

## At a glance

| Area | Available today | Important boundary |
| --- | --- | --- |
| Native compilation | Real check/build/test/run; LLVM 22; agreeing Loom stages 2/3 on three hosts. | LLVM is the only backend; bootstrap agreement is not a correctness proof. |
| Language | Generics, concepts/dyn, associated types, closures, tuples, recursive List-backed data and patterns. | Not every accepted generic/pack combination is implemented. |
| Guarantees | Checked constrained construction, safe weakening, mandatory postconditions, bounded loop/content proofs. | General induction and mutable-alias preservation remain open. |
| Metaprogramming | Pure CTFE, type/value/function parameters, packs, typed macros, reflection and tracked inputs. | Visibility and staging apply; no arbitrary compile-time I/O. |
| Resources and async | Moving GC, lexical cleanup, stackless Tasks, real I/O and explicit shared CPU workers. | Resource transfer and shared-state proofs remain conservative. |
| Programming tools | Directory tests, formatter, LSP/VS Code and public parser/analysis libraries. | Unaccounted/generated references and broader erroneous-source queries remain open. |
| Evolution tools | Reviewed semantic merges and a real offline SQLite migration trial. | Not general semantic VCS or deployment compatibility proof. |

## Build and programming experience

Follow [build and try it](../../compiler/README.md#build-and-try-it).
Cold builds start from the portable, source-bound checked stage 0; Unix CI
reproduces it from its fixed source pin. The current native bridge builds the
compiler on all three hosts, without a historical compiler/checkpoint chain.
Daily development uses a one-stage `--dev` rebuild; CI compares stages 2/3.
Production compiler sources deliberately retain a conservative seed subset.

The [Loom frontend](../../compiler/loom/README.md) owns loading, syntax, binding,
typing, contracts and checked-program emission. Rust supplies LLVM lowering,
linking, GC and narrow platform primitives, not a parallel source frontend.
There is no maintained interpreter or universal-value backend.

A directory is a package, with `pub` controlling external visibility.
Colocated `*_test.loom` and embedded `test fn` can use private declarations;
normal library builds and imported dependencies exclude tests.
Recursive testing uses independent package roots and skips nested modules and
directory aliases. Filters select executable test roots without disabling checks.
Automatic run/test outputs are invocation-local; persistent outputs belong to
`build` or `test --no-run`.

No-result functions omit the return type and final return; source `Unit` rejects.
Unused values need explicit `discard`; Task/MustScope obligations cannot be
discarded. Record fields require newlines or semicolons. Multiline Text and
comment-preserving `loom fmt` are implemented.
The [wordcount trial](../../compiler/examples/wordcount/README.md) exercises
initialization, private packages, resources, tests and argument forwarding.

The [VS Code extension](../../editors/vscode/README.md) uses the real compiler for
unsaved diagnostics, formatting, hover/navigation, references, completion and
checked import fixes. Checked renames cover local/parameter/pattern bindings and
accounted function/type/concept/record-field uses across a local module, including
each package's private tests. Virtual edits recheck before application.
Private overloads can be individually renamed to an unused name, preserving
checked sibling calls and typed callbacks. Public overloads, unvisited/generated
uses and external-consumer migrations remain conservative; dependency snapshots
are not rewritten.
Private declaration edits reject generated references and exported macro
generators whose definition-context uses are not a complete editable source map.
Query-only pattern evidence retains nested record fields and enum variants
through decision-tree lowering. Semantic merge compares match guards/bodies and
destructuring without treating the new pattern bindings as external references;
missing or ambiguous checked definitions still reject.
Constrained aliases now compare pure helper/field references in a pinned standard
library context. Dynamic associated labels retain the actual concept member,
including when a same-spelled generic type parameter is in scope. This is source
binding evidence, not behavioral compatibility or migration proof.

Resident workers reuse valid analyses while revalidating source and observed
inputs. Query-only declaration headers support ordinary/generic and impl
signatures despite body errors, including original static parameter slots.
Independent body queries isolate unrelated ordinary/template errors while
validating the selected template, CTFE and witness dependency closure.
Uninstantiated generic/comptime bodies retain checked local types and source
bindings with lexical type labels, without inventing runtime instances.
Nested closures retain their parameters, captures and enclosing type labels too.
Dependent type values, unexpanded packs and broader body recovery remain limited.
Recovered queries provide neither build success nor proof evidence.
Member completion inside `comptime` execution blocks uses staged prefix checking
and the ordinary runtime-capture boundary; it does not evaluate an unfinished body.
Contract navigation lazily checks source clauses in a separate query snapshot,
retaining parameter, field, helper and captured-binding locations lost during
proof normalization. Concrete generic observations share this source view;
uninstantiated or generated uses remain conservative. Executable contracts and
ordinary compilation do not retain this query evidence.
Public `std.loom` parser/project/analysis libraries share the compiler's
implementation without requiring a compiler process, filesystem or LLVM.
Hosts can explicitly supply the source `std.loom.proof.z3` adapter for unresolved
supported proofs; default analysis stays I/O-free and ordinary programs gain no
solver dependency.
Independent queries on a successful analysis replay its observed inputs and
retain at most one checked source scope. Header and body evidence stay separate;
failed or ambiguous scopes cannot supply cached binding facts.
Public reference queries share a token stream for a batch or exact-name scan,
retaining unanswered and ambiguous candidates. Editor and semantic-change
consumers share this evidence without treating it as complete source coverage.
Compiler-owned reference obligations also expose unsupported source ranges;
semantic merge uses them for bounded generics and cleanup blocks instead of
maintaining a second syntax whitelist. All required binding checks still apply.
Default methods use their implicit abstract `Self` scope; valid abstract evidence
is required without inventing implementations or dropping observed-target checks.
Their IDs and spans belong to a snapshot, not persistent semantic history.

## Language and compile-time programming

Native code supports scalar types, Text/Bytes, records/enums, tuples, shared
Lists, overloads, generic data/functions, closures and pattern matching.
Tuple equality composes comparable leaves through ordinary scalar rules,
including CTFE and contracts; operands remain eager, once-only snapshots.
List-backed recursive data works; infinite inline layouts reject.
Record updates construct new values and validate any requested refinement.
See the [implemented subset](../../compiler/README.md#implemented-subset).

Concepts require explicit conformances. Static/default/generic methods, bounded
associated types/families and exact dyn bindings work. Dynamic dispatch uses
statically established witnesses and reachable slots, not runtime `any`
discovery. Transparent explicit dyn adapters reuse the payload and a contiguous
witness slice when ABI, receiver and arguments agree and no other work occurs.
Nontransparent/noncontiguous adapters retain boxes. Dynamic associated families
accept scoped structural templates, including outer captures and nested bindings;
conversion checks arbitrary admitted inputs rather than sampling reachable slots.
Eligible body snapshots rekey closed templates, nested captures and symbolic
projections without exporting their local evidence. Dependent compile-time type
computations remain unsupported. See [concepts](../../compiler/README.md#concepts).

[Bound method values](../../compiler/examples/bound_methods/README.md) retain one
receiver evaluation and use ordinary closure environments. Exact contextual
signatures select overloads and generic/variadic methods. Binding an async method
schedules nothing; invoking it enqueues a hot Task. Shared children remain shared
and scoped/resource captures reject.

[Variadic functions](../../compiler/README.md#variadic-functions) and generic
records/enums support structural inference, independent/repeated words, explicit
`...` groups, callbacks, methods, CTFE and suspension.
Unique bounded affine width equations select instances; unknown partitions reject.
Symbolic family contracts use induction and word entailment, not sampled widths.
Constructed and nested words can derive capabilities through checked impl
prerequisites without granting hidden bounds to raw elements.
Declaration headers check lexical expansion scope before proof-input erasure.
Type-dependent content induction, opaque-layout inspection and remaining
unsupported combinations still reject.

[Compile-time execution](../../compiler/README.md#compile-time-execution) uses a
bounded evaluator over the checked model. Pure functions, loops, recursion,
selected branches and fresh internally shared graphs work; it is not a second
runtime backend or a proof by sampling.
Type/value/function parameters, structural type construction, associated
projections, hygienic expression macros and explicit declaration generation use
ordinary typing and visibility. Declaration generation is not an implicit
fixed-point expansion. Reflection exposes filtered schemas, not extra access
or conformance evidence.
Tracked files, build options and target properties bind checks, caches, editor
freshness and receipts; pure evaluation cannot hide external I/O.

## Constraints and required proofs

The [contract reference](../../compiler/README.md#contract-boundary) describes the
supported theories and boundaries. The central rules are:

- A statically established constructor returns the refined value; an unknown
  predicate retains checked Result construction. Safe weakening can omit a
  redundant check while preserving evaluation and faults. Pure predicates cannot
  depend on external mutable state.
- Every declared `ensures` needs a static proof. Cheap local rules run first;
  unresolved supported obligations use compile-time SMT. Unknown, exhausted or
  failed reasoning never supplies evidence or becomes a runtime postcondition.
  Native programs do not link the solver; public checking remains I/O-free unless
  its host supplies the trusted proof backend.
- Integer value algebra retains separate arithmetic/access safety obligations.
  Float reasoning preserves binary64 IEEE semantics, including NaN, signed zero
  and rounding. Text uses immutable UTF-8 bytes; equal lengths do not imply equal
  contents. Enum facts are guarded by the selected nominal tag.
- Callee/concept guarantees compose through generic/dyn calls, bounded recursive
  groups and successful Task completion. Faults and cancellation supply no
  normal-return guarantee. Inputs evaluate once; suspension invalidates
  unprotected heap observations.
  Closure contracts can use complete read-only capture slots and established
  captured type constraints. Mutable bindings share the ordinary storage-version
  rules, including callback writes, loop induction and entry observations through
  captured List handles and inline records. Unobserved generic fields stay opaque.
  Shared children retain interference and alias rules; general mutable-frame graphs
  remain open. Bounded synchronous recursive groups retain checked storage frames
  only after all reachable bodies and actual arguments pass the same effect rules.
- Loop candidates need entry and every-backedge proof; losing a candidate
  rechecks dependents. Logical `old` snapshots preserve supported entry values
  and outer content versions without runtime copies or freezing mutable children.
  General termination, recursive predicates and arbitrary graph snapshots are
  not proved. Signed affine pair bounds also compose nonconstant callee bounds.
- List/Bytes extent, indexed observations and supported immutable columns share
  versioned storage rules. Quantified valid-index views retain the ordinary
  element domains, including Int/byte ranges and immutable inline fields; no
  fact grants an out-of-range access or transfers to a different storage version.
  Checked append preserves existing cells and immutable columns even through
  possible aliases, while only the exact receiver gets its precise new length
  and tail. Histogram equality transfers only for count extents proved inside
  the preserved prefix, not for an unobserved tail. This frame does not restore
  observations lost to argument effects
  or shared interference. Other unknown alias writes and opaque effects
  invalidate facts. Constrained writes must preserve the complete predicate after
  every actual store, not merely at normal return. Both sequence types support
  fresh constrained construction and checked representation borrows; byte-range
  faults precede stores, and opaque content predicates grant no unproved writes.
  Recursive element results may retain the same nominal sequence guarantee;
  raw or differently constrained backreferences do not inherit it. The
  [recursive data example](../../compiler/examples/recursive_constraints) combines
  shared child Lists, a proved recursive fold and real suspension.
  Dependent store arguments preserve checked block/match bindings and the
  receiver evaluated before argument-side rebinding; unsupported scopes reject.
  Proved identity stores preserve even unexpanded pure predicates. Every store
  compares the current representation, not IEEE equality or a cached shared read.
  Pure argument helpers may modify fresh unpublished scratch; an allocation
  label or local variable alone does not establish that separation.
  Literal stores also use proved scalar/quantified cell facts at the atomic
  snapshot, including beside an unexpanded pure conjunct.
  Float identity distinguishes signed zeros; numerical equality alone is not
  enough, and an unknown NaN replacement supplies no identity evidence.
  Shared atomic element reads can use the continuing predicate at that snapshot;
  separate reads still cannot assume equal contents.
- Fresh drafts and selected known aliases can publish a constrained outer header
  after checked construction and escape analysis. Raw writable aliases cannot
  escape. Shared builds independently revalidate preservation; no implicit copy,
  monitor, ownership syntax or sampled reservation is introduced.
  General nested-graph publication and changing loop-carried identities remain
  conservative.

Representative native trials include
[constrained shared views](../../compiler/examples/shared_views),
[constrained Bytes](../../compiler/examples/byte_constraints/README.md),
[draft publication](../../compiler/examples/record_refinement/drafts.loom),
[loop contracts](../../compiler/examples/loop_contracts),
[sorting/permutation](../../compiler/examples/smt_contracts/quantified.loom),
[copy/sort composition](../../compiler/examples/smt_contracts/copies.loom),
[alias-safe append](../../compiler/examples/list_append/README.md) and
[async guarantees](../../compiler/examples/async_contracts/main.loom).
These are bounded proved stories, not general mutable-graph or content induction.

## Standard library, memory and async

Source `std` provides collections, text/numeric/byte operations, Option/Result,
typed JSON, storage descriptors/codecs, reflection, filesystem/process APIs,
iterators and async streams. JSON policy has no special runtime implementation.
[Iterators](../../compiler/std/iter/README.md),
[file lines](../../compiler/std/file/lines/README.md) and
[streams](../../compiler/std/stream/README.md) support ordered, short-circuiting
consumers and owned scoped pipelines. [Binary file chunks](../../compiler/std/file/chunks/README.md)
provide lazy bounded worker reads without text decoding or retaining the whole
file. JSON streaming handles chunk boundaries,
backpressure and LF-framed records without requiring a full document Text.
Source-written [length-prefixed framing](../../compiler/std/stream/length32/README.md)
handles bounded binary messages over arbitrary chunks, including scoped factories
and real TCP. [UTF-8 line framing](../../compiler/std/stream/lines/README.md)
adds explicit byte limits, CRLF handling and strict decoding across arbitrary
chunks; errors/cancellation retire the shared cursor without suffix prefetch.
Other framing and general borrow-retaining adapters remain open.

Stop-the-world copying GC preserves precise roots, sharing and cycles.
Nonallocating functions stay root-free; liveness retires dead roots at checked
boundaries. Large inline managed values use local storage directly as GC roots;
pending arguments retain independent snapshots. Large objects use separate
storage; stress tests relocate all sizes.
Source exposes no addresses, finalizers, weak references or ownership/borrow
syntax. Expression-internal root retirement and generational/concurrent GC
remain open.

[Lexical cleanup](../../compiler/README.md#lexical-cleanup) handles block exits,
returns, propagation, faults, suspension and cancellation.
MustScope freshness/escape checks and typed cleanup protect nested and recursive
resources, partial aggregate construction and unextracted Task results.
Cleanup retains the first fault while draining remaining work; OOM/termination
offer no cleanup guarantee. Existing scoped resources cannot generally transfer
into Tasks; directly awaited borrows drain before scope exit.
NoSuspend guards reject suspension. Mixed resource/Task containers remain
conservative where typed transfer is unsupported.

[Tasks and I/O](../../compiler/README.md#source-tasks) use typed stackless frames,
one owner-thread ready queue and real completion notifications, not a thread per
Task or busy polling. One-shot handles, outcomes, cancellation, deadlines and
tuple/List joins are implemented; join policy lives in Loom.
DNS, asynchronous files, TCP/UDP, selected socket policies/multicast, process
capture and verified client/server TLS work through source policy and narrow
native boundaries. TLS uses a separately linked Rustls provider.
Revocation and broader application networking remain open. Blocking OS calls
use bounded workers and copied native data; cancellation waits for completion.
Process capture has unbounded output buffering and reaps only the direct child.

[Shared workers](../../compiler/README.md#shared-workers) explicitly request CPU
parallelism. No-worker executables retain direct lowering.
Shared builds conservatively guard accesses and independently revalidate proofs
for interference; checked private accesses can omit guards.
Mutexes provide lexical MustScope/NoSuspend guards.
Memory-safe logical races are allowed; compound operations need synchronization.
General synchronized-heap reasoning remains open.

## Modules, caching and performance

[Dependencies](../../compiler/README.md#module-dependencies) support path, exact
HTTPS Git/forks, monorepo subdirectories, explicit graph overrides and SemVer
tag resolution. Compatible requests prefer a shared source instance; disjoint
ones retain separate nominal types. Locks bind source identity, commits, requests
and verified contents. Ordinary commands stay offline; only `resolve` fetches.
Authenticated sources require an explicit trusted helper. Credentials are not
persisted or exposed in diagnostics, and caches validate real bytes/membership,
not sidecars alone. Conflicting graph search can be combinatorial.

[Frontend reuse](../../compiler/README.md#frontend-cache) binds compiler bytes,
source/test selection, current bindings, modes and observed inputs.
Eligible definitions and concrete bodies can survive edited-source closure
misses; failed checks publish no evidence.
Trusted-local snapshots are not portable proof artifacts.
Native object reuse is whole-closure and always relinks.

Compiler latency/memory remain delivery goals, not completed optimizations.
The edited-source harness uses fresh processes, a reachable contracted-helper
edit, a rejected false-contract canary and verified definition/body reuse.
Unchanged cache hits are not edit benchmarks; restoration/writeback can outweigh
savings, especially for small packages, so caching remains opt-in.
[Native application benchmarks](../../benchmarks/basic/README.md) measure a
separate concern. Keep individual historical measurements in Git/PR evidence,
not a second performance changelog here.

## Semantic changes, deployment and delivery

The [semantic merge trial](../../tools/semantic_change_trial/README.md) supports
reviewed move-plus-edit, function rename-plus-edit and independent
additions within one package or a fixed-package module, using explicit identities
and pinned source/test bases.
Complete module batches can rename public functions across checked imports,
calls and independent private tests, including partial overload-group renames
and contextual function values. Checked imports split without reformatting the
original lines or comments; production and private-test scopes stay separate.
Visibility stays fixed. Unrelated import changes, missing module consumers and
known external-module consumers require resolution; unknown external consumers
are not updated and API compatibility is not proved.
Selected overload renames use the same checked binding normalizer; names alone
neither identify a declaration nor justify an overload conflict.
Function rename composition rejects exported macro providers whose generated
definition-context references cannot be completely compared.
All merge APIs accept an optional explicit proof capability; the CLI uses the
shared Z3 adapter for ordinary contract checks, including rename/binding checks
and each package's private tests. Defaults stay I/O-free; unknown proofs reject.
Ambiguities can persist as source-bound drafts; compilation ignores that metadata.
Checked bindings prevent silent nominal/field drift.
Generic reference comparison requires checked abstract bodies and agreement
with observed concrete instances; missing or ambiguous bindings and unselected
static branches still reject.
Other public declaration renames, automatic identities, arbitrary edits and
package/import-graph changes remain open.

The [SQLite migration trial](../../tools/deployment/sqlite_migration_trial/README.md)
executes an offline orders upgrade, data-preserving downgrade and re-upgrade.
Append-only events distinguish assumptions from observations; plans bind retry
inputs, transactional preflights and rehashed artifact receipts.
Operators still supply storage mappings. Hotfix proof is unknown and cannot
authorize execution. General migration packages, compatibility/mapping proof,
online coexistence and generalized recovery remain goals.

CI verifies relocatable development archives on macOS/Linux/Windows.
The [draft-release workflow](../../distribution/README.md) promotes successful
main artifacts after checking all host gates, checksums and embedded identities;
it defaults to dry-run and never publishes a release.
Host LLVM/linker dependencies remain external. These are not self-contained
published releases or evidence for additional targets.

Update current capabilities and limitations here, detailed semantics in the
compiler guide and examples, and delivery gates in the roadmap.
History belongs in Git/PRs, not appended completion narratives.
