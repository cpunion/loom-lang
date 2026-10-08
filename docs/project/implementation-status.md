# Implementation status

Loom has one self-hosted native compiler, a source-written standard library and
usable CLI/editor tooling. It is suitable for the documented programming trials,
not a complete implementation of the [project goals](charter.md).
The [roadmap](../../ROADMAP.md) gives the remaining gates; the
[compiler guide](../../compiler/README.md) is the detailed reference for today's
syntax, APIs and supported proof fragment.

## At a glance

| Area | Current evidence | Important boundary |
| --- | --- | --- |
| Native compilation | Real check/build/test/run; Loom stages 2/3 agree on macOS, Linux and Windows. | LLVM is the only backend; bootstrap agreement is not a correctness proof. |
| Language | Generics, concepts/dyn, associated types, closures, tuples, recursive List-backed data and pattern matching. | Not every accepted generic/pack combination is implemented. |
| Guarantees | Checked constrained construction, safe weakening, bounded mandatory postconditions and selected invariant-preserving operations. | General loop proofs and mutable-alias preservation remain open. |
| Metaprogramming | Pure compile-time execution, type/value/function parameters, packs, typed macros, reflection and tracked inputs. | Staging and visibility still apply; no arbitrary compile-time I/O. |
| Memory/resources/async | Moving GC, lexical cleanup, stackless Tasks, real timers, files, DNS/TCP/TLS, tuple/List joins and explicit shared CPU workers. | Conservative shared-build instrumentation and interference proofs; no concurrent/generational GC. |
| Programming tools | Directory tests, formatter, LSP/VS Code, public parser/analysis libraries, checked function/type/concept/record-field edits. | Unaccounted/generated uses, external-consumer API edits and broader erroneous-source queries remain open. |
| Evolution tools | Reviewed single-package semantic merge and a real offline SQLite migration trial. | Bounded prototypes, not general semantic VCS or deployment compatibility proof. |

## Build and programming experience

The [Loom frontend](../../compiler/loom/README.md) handles loading, syntax,
binding, types, contracts and checked-program emission. A Rust LLVM 22/Inkwell
tool lowers checked programs and links native artifacts; it does not parse or
check source again. Rust also supplies GC and narrow platform primitives. There
is no maintained interpreter or legacy universal-value backend.

An existing compiler builds the current source. Cold builds on all three hosts
start from the same source-bound checked input, reproduced by Unix CI. No
historical compiler/checkpoint chain is built. Production compiler sources deliberately use a conservative
subset. Development uses a one-stage rebuild; CI retains the stage 2/3 gate.
See [bootstrap instructions](../../compiler/README.md#build-and-try-it).

A directory is a package, with `pub` controlling external visibility. Both
`*_test.loom` and embedded `test fn` can use their package's private declarations;
library compilation excludes them. `loom test --recursive` selects each package
as an independent test root, includes test-only packages and continues after a
package failure. It skips nested modules, hidden/build directories and directory
symlinks. Dependencies are still loaded without tests. `--no-run` emits separate
package executables. The compiler's integration gate discovers `std` packages
through this command instead of a hand-maintained list.

`loom init`, argument-forwarding `run --`, and shared `loom fmt` support the
[multi-package file-tool trial](../../compiler/examples/wordcount/README.md).
Formatting preserves comments/literal spelling and expands function/record
bodies. Record fields require newlines or semicolons; whitespace adjacency
rejects. Raw multiline Text is supported. No-result functions omit the return
type and final return; source `Unit` is rejected. A value must be used or
explicitly discarded, and Task/MustScope obligations cannot be discarded.

The [VS Code extension](../../editors/vscode/README.md) uses the real compiler
for unsaved diagnostics, formatting, hover/navigation, name/member/import
completion, checked import fixes, parameter/local-pattern rename and checked
function/type/concept rename. One public top-level function, record, enum,
constrained type or concept can be renamed throughout its local module, including import declarations,
checked signatures and field/payload annotations, constructors, enum patterns,
qualified calls, callbacks, embedded
tests and unopened test files. Each directory retains its private test scope;
the complete virtual edit is checked before offering it. Nested modules and
directory aliases are excluded, as in recursive tests. Unknown occurrences,
overloads and generated references refuse edits; dependency snapshots and
external consumers are not rewritten. Resident workers reuse valid analyses and definition checks while
revalidating loaded source and observed inputs. Completion recovery is virtual;
it supplies neither build success nor proof evidence.
Runtime parameter declarations and match payload/whole bindings retain exact
checked definition locations. Renames include contracts, preserve shadowing and
recheck the virtual package. Runtime-local renames also cover selected compile-time
branches when checked specializations account for every occurrence. Compile-time
parameter hover/navigation/references/rename use a fresh
optional binding trace before specialization erases uses; values, callbacks and
covered compile-time blocks/branches work without executable IR metadata. Hover
retains concrete parameter types and navigation follows the source binding,
including specialized callbacks.
Concrete annotations navigate through checked types and package bindings, not
same-spelled specialized parameters. Private nominal types use the existing
test-inclusive package edit check. Unobserved branches, packs, type aliases and
unaligned annotations still refuse edits. Record fields now use checked
receiver/initializer identities, including updates, explicit destructuring,
contracts and type-constraint templates. Distinct field owners and locals stay
unchanged; unaccounted labels or structural accesses in any module package block
the edit. Functions and concepts share a lazy declaration-binding trace. Folded
compile-time calls and named callbacks retain their checked source targets,
including generic and pack specializations; covered uses participate in navigation
and checked rename. Concept rename uses the same trace for bounds, `impl`, `dyn`,
associated-type qualifiers, selected type values, `implements` guards and explicit
method calls. Associated-type navigation follows the checked concept member,
including generic families and `Self` projections. Fresh source rebinding excludes
instances appended by earlier checks; ordinary compilation does not collect this trace.
Independent inspection and member completion also rebind source without changing
the caller's symbols. Checked query tables carry their own binding snapshot,
including generated closures and pack instances; analysis reuses this same view.
Folded queries cache checked ordinary-function closures; generic/static owners
retain the full application-instance check, not invented specialization inputs.
Each package must account for its own references, including tests; unvisited
branches refuse edits. The trace stays outside executable IR and build caches.
External-consumer API migration and broader recovery/query support remain open.

Public `std.loom` syntax, fragment parsing, project/binding and typed-analysis
libraries are the same implementation used by the compiler. In-memory syntax
and semantic consumers need no compiler process, filesystem or LLVM. IDs/spans
belong to a selected snapshot, not persistent source history or a stable schema.

## Language and compile-time programming

Native code supports Int/Float/Bool/Text/Bytes, records/enums, tuples, shared
Lists, overloads and generic data/functions. Recursive structures through Lists
work; infinite inline layouts reject. Destructuring, nested/literal/guarded
patterns, List indexing and new-value record updates use ordinary typed
operations. Record updates do not mutate the base or inherit its refinement
without validation. See the [implemented subset](../../compiler/README.md#implemented-subset).

Required Float contracts now reuse exact IEEE comparisons from constrained
values, typed reads, verified returns and guarded entry observations. Literal
operations use binary64 evaluation; symbolic arithmetic preserves source order
and IEEE semantics. NaN, signed zero and rounding do not inherit integer/real algebra.
Declared concept guarantees also compose Float/refined-Float results through
generic and dynamic calls and variadic induction, using this same value theory.
The bounded IEEE order graph also proves comparison chains, wider constant
ranges, guarded reflexivity/complements and contradictory short-circuit paths.
These rules support contracted clamp functions without arithmetic substitution
of equal signed zeros. Unresolved supported arithmetic uses the same compile-time
SMT backend with binary64 IEEE comparisons, negation and RNE-rounded `+`, `-`,
`*` and `/`, plus truncating `%`, in source order; it does not replace Float
with Real arithmetic. Primitive classification rules and bounded branch-local
substitution reduce arithmetic circuits without identifying signed zeros.
The [arithmetic example](../../compiler/examples/smt_contracts/floats.loom)
proves a guarded wrapper of source `std.float.abs`, bounded scaling, subnormal
doubling and a rounded unit addition. Pure body expansion and the wrapper's
checked guarantee compose into a nonnegative refinement without a native
intrinsic or application-specific proof rule.
The [remainder example](../../compiler/examples/smt_contracts/float_remainders.loom)
proves range/refinement composition, dynamic divisor alternatives, signed-zero
preservation, exceptional inputs, subnormals and nested remainders.
General Float loop-invariant inference and exhausted/unknown solver obligations
remain unsupported.

Concepts require explicit conformances. Static/default/generic methods,
associated bounds/defaults/families and exact dyn bindings are implemented.
Dynamic calls use statically established evidence and sparse used slots, not
`any`-based runtime discovery. Explicit `impl D for dyn C` adapters work;
dynamic associated families and zero-allocation witness upcasts do not.
Named/captured callbacks retain typed environments; closures cannot conceal
live Task or scoped-resource obligations.
Generic instances retain nominal type domains and prerequisites of supplied
concrete conformance evidence, including test-scope inputs. Unrelated test-only
implementations remain undiscoverable from production code.

Compile-time execution uses a bounded evaluator over the checked model, with
pure functions, loops, recursion and fresh shared graphs. Type values remain
compile-time-only. Source `std.meta` exposes a function value or declared function
type's ordered parameter types and optional return type through ordinary Loom
helpers and library `Signature` conformance; callbacks are not
executed, captured data is not inspected, and an omitted result is `None`, not
`Unit`. Static scalar/immutable-aggregate/function parameters,
selected branches, function/data type packs and fixed-shape `comptime for/map` share
ordinary typing. Heterogeneous tuple packs are not runtime-sized Lists.
Source `std.meta.tuple`/`function` construct canonical structural types from
compile-time-computed `List[type]` inputs, including nested shapes and omitted
function results. The existing checker interner owns identities and validation;
no type factory or type list becomes a runtime value.
Pure construction also accepts symbolic generic elements in local annotations
and callback types. Structural comparison keeps equality, distinctness and
unresolved relationships separate; unknown selection and required proofs still
reject. Computed aliases retain explicit public bounds and ordinary constraints.
Associated projections also supply type values, including generic families,
qualified concepts, `Self` and immutable selected receiver types. They retain
ordinary bounds, visibility and value-shadowing rules.
Selected type bindings and explicit static type parameters also participate in
compile-time equality/conformance guards. Unknown guards remain deferred;
selected enum constructors are still values, not types.
Static tuple/record type iteration also accepts immutable compile-time-selected
type bindings, with the same lexical visibility and ordinary generated code.
Record/enum packs infer arity from structural initializers, preserve nominal
identity and check selected arities with abstract elements. Recursive data,
Task payloads, compile-time values and incremental restoration use the same
typed aggregate path. Elementwise associated types/families retain declared
bounds and do not infer receiver types from projected results.
Function packs also infer arity from callback parameters/results and nested
tuple or nominal patterns; contextual function references and overloads use
ordinary inference after expansion. The
[native example](../../compiler/examples/data_packs/functions.loom) covers
shared captures and once-only input effects, without a runtime pack.
One structural tuple can repeat the same pack around fixed fields. Its width
determines arity algebraically; all occurrences retain the same ordered element
types. Native callbacks, CTFE, dyn methods, shared aliases and Task transfers use
ordinary expanded signatures; see the
[repeated pattern example](../../compiler/examples/variadics/repeated.loom).
Packs used only in results or compile-time generation are also accepted. Expected
results select arity for ordinary/async calls, function references and static/dyn
methods; explicit arguments select type generators without sample inputs.
Unknown arity still rejects. The
[factory example](../../compiler/examples/data_packs/factories.loom) covers these
paths and shared generated callbacks through native check/build/test/run.
Method-local packs support default/override methods, ordinary generic impl
headers, associated projections, structural inference, CTFE and async. Family
headers match structurally for every arity; selected bodies are checked under
abstract declared bounds. Dynamic calls use finite ordinary slots, not a new
runtime pack ABI. The [method example](../../compiler/examples/variadics/methods.loom)
also checks sharing and once-only effects. Impl-header packs infer arity from
nominal, tuple and function target shapes, including nested/repeated expansions.
Selected shapes check methods and associated bindings with abstract elements;
method-local packs retain their own widths.
Direct value packs retain ordinary parameter order between fixed
parameters; all occurrences of the same type pack share a width, uniquely
selected by the argument-count equation. Nonintegral widths and mismatched
element types reject. Empty/static packs,
contextual function references, fixed-scalar contracts, dyn defaults/overrides,
Task suspension and restored cache recipes have native/checker evidence in the
[position example](../../compiler/examples/variadics/positions.loom).
Repeated direct values, mixed static/runtime packs, nested iteration and typed
suspension reuse that expansion in the
[repeated example](../../compiler/examples/variadics/repeated.loom).
Overlap checking preserves
expansions and conservatively rejects unknown intersections, without sampling.
Private cache recipes retain the original member and both width vectors. The
[impl example](../../compiler/examples/data_packs/implementations.loom) exercises
native/dyn calls, declared contracts, shared values, CTFE and async payloads.
Pack-independent postconditions use dependency erasure and ordinary abstract
proofs, including uncalled families and inherited method contracts; the
[contract example](../../compiler/examples/variadics/contracts.loom) exercises
constrained construction, CTFE and dyn dispatch. Element-independent pack
iteration now reuses scalar/List loop induction over symbolic widths, including
zero iterations, indices, independent nested packs and inherited methods. The
[induction example](../../compiler/examples/variadics/induction.loom) covers CTFE,
shared List updates and native/dyn defaults. Direct elements now compose declared
scalar/inline aggregate concept method guarantees through the same loop rules.
Methods with omitted returns compose declared heap guarantees without a dummy
result or an explicit `Unit` type. Possibly resized handles use existing
entry-length induction even without local rebinding; repeated updates need no
sampled iteration counts. The
[observation example](../../compiler/examples/variadics/observations.loom) covers
mixed/empty packs, generic and variadic methods, Text/tuple/record guarantees, inherited bounds,
nested independent packs, CTFE, function references and native/dyn dispatch.
Ordinary generic parameters, method-local packs and static arguments reuse existing
method checking and width inference, including independent tuple groups;
unknown static values cannot supply proof facts. Observations cannot depend on
receiver identity/types;
repeated calls remain independent and opaque effects invalidate heap facts.
Element-independent induction also accepts pack-bearing structural tuples: their
extent composes fixed fields, repeated widths and independent expansion groups
through ordinary affine arithmetic. A nested fixed field contributes one element;
elementwise expansions retain the shared width selected by shape checking. The
[structural example](../../compiler/examples/variadics/structural_induction.loom)
covers global indices, List effects, CTFE, function references and inherited
native/dyn methods. Mixed elements do not inherit a single pack's concept bounds,
and the existing scalar invariant fragment is unchanged.
Named pack-bearing inputs can forward into a synchronous family and compose
universally checked postconditions. Bare, structural tuple and nominal patterns
share constructor identities with impl overlap checks. Bounded symbolic-word
matching jointly binds fixed types and ordered sequences across all inputs;
repeated/independent groups and corresponding elementwise constructors retain
their source schema. Ambiguity, exhausted search and splitting unknown words
reject, without enumerating expansion widths. Fixed inputs and effects use
ordinary call rules; type bindings resolve in the header scope.
The [forwarding example](../../compiler/examples/variadics/forwarding.loom) covers
declared bounds, independent groups, CTFE, callbacks and inherited dyn methods.
The [structural forwarding example](../../compiler/examples/variadics/structural_forwarding.loom)
covers fixed fields, nested constructors, nominal inputs and generic/default
method scopes. Mixed captured sequences cannot supply element requirements.
Overloaded/indirect/async callees, explicit type arguments, newly assembled
sequences, unresolved associated callee patterns and unknown fixed layouts
still reject in family proofs.
Type-dependent induction, opaque element escape, pack-dependent results and
iteration over non-tuple structural sources still reject. A
single type pack can
have fixed parameters before and after it, including concept methods and impl
families; the same ordered expansion and width rule applies to each.
Functions and concept methods now select independent sequence widths through
bounded mathematical affine equations over input/contextual shapes. Known
widths locate middle parameters and nested expansions; elementwise combinations
require equal widths. Empty zero totals, coupled equations, ordinary inference,
CTFE, dyn defaults/overrides and Task frames have checker/native evidence in the
[independent example](../../compiler/examples/variadics/independent.loom).
Restored body/dyn recipes retain the complete width vector, so equal totals do
not merge different groups. Ambiguous, inconsistent, nonintegral or exhausted
inference rejects, without specialization sampling. Records/enums also retain
independent widths in nominal identity and cache recipes. Explicit `...` groups
distinguish a sequence of types from a single tuple type; field/payload and
contextual inference remain structural. The
[group example](../../compiler/examples/data_packs/groups.loom) covers overloads,
recursive data, CTFE, shared graphs and Task payloads. Independent impl headers
reuse these shape equations, abstract checks and nominal group boundaries. The
[independent impl example](../../compiler/examples/data_packs/independent_impls.loom)
also exercises associated bindings, dyn defaults/overrides and shared values
through suspension. General content/type induction beyond declared method
observations remains unsupported.
Compile-time execution is not proof by sampling.

Independent nominal declarations do not share a global type-count budget.
Recursive type expansion stays depth-bounded; associated-evidence search has
a per-request work budget, independent of unrelated interned types.

Typed expression macros receive inferred schemas and return checked hygienic
Text/AST expressions. Explicit top-level declaration generation runs before
binding; it is not an implicit fixed-point expansion. Reflection exposes
visibility-filtered type graphs, field reconstruction and direct predicate
syntax, not extra access or conformance evidence. See
[compile-time programming](../../compiler/README.md#compile-time-execution).

Tracked text/binary inputs, explicit build options and observed backend target
properties bind checking, editor freshness, caches and build receipts. Ordinary
pure execution cannot hide external I/O. Reified mutable graphs are fresh per
evaluation and retain their internal sharing; static immutable values specialize
without runtime argument slots.

## Constraints and required proofs

Construction returns the refined value when established statically; an unknown
value predicate uses checked Result construction, including immutable Text
refinements with byte equality. Safe weakening and supported
implications avoid redundant checks while retaining input evaluation and faults.
Predicates may call pure functions, but cannot rely on external mutable state.
Generic refinements retain their type arguments and explicit concept bounds in
predicate typing, helper analysis, construction and proof expansion. Constructors
infer arguments from the base or an expected refinement, or accept explicit
arguments. Each new instance checks resource and mutable-observation restrictions;
shared generic siblings are allowed, not observations of their mutable contents.
Partial record construction proves known immutable fields independently of
unobserved shared siblings. Nested structure and stable scalar flow facts are
retained; unknown calls and mutable observations become independent proof-only
snapshots, without replaying or reordering executable initializers.
Verified factory contracts accept unmodeled inputs without an opaque-type
whitelist; such inputs contribute no tag, payload or finiteness assumptions.
The [shared view example](../../compiler/examples/shared_views) uses `Pair[T]`
with Int, Text and Bool elements. Extent-only List constraints accept generic and
shared elements; the [native example](../../compiler/examples/list_contracts/refinements.loom)
checks generic replacement, append, checked copying and suspension. Fresh factories
may retain shared elements while proving an independent, unpublished outer header;
input resizing cannot change the copy's extent, and nested updates remain visible.
Content constraints may observe immutable element fields beside unobserved shared
siblings. The checker tracks reads through elements and rejects nested mutable
observations, including same-type recursive headers; this is not a deep freeze.
Direct `self[index]` predicates use these same observation rules as pure getter
helpers; nested shared mutable reads remain rejected.
For mutable element slots, the example instead composes an extent constraint
with a constrained element type. Already validated replacements preserve element
invariants; reading an immutable amount supplies its required positive-return
proof while unobserved notes remain writable through aliases and suspension.
Content predicates can now admit one atomic element replacement using the same
state/contract algebra, including bounded pure scans over `List[Int]`. The complete
predicate, replacement type/literal or stable scalar caller conditions, and successful
native bounds check must prove preservation. Preconditions, assertions, selected
branches, early exits and immutable copies reuse the existing construction-fact
algebra, including bounded pure helpers. Mutable bindings and heap reads supply
no stable evidence. Checked argument-preserving `set` forwarders share this rule; normal-return
promises cannot authorize transient invalid contents. Unknown arguments/effects reject.
Finite multi-step source helpers, branches and supported loops now use the same
engine to require the complete predicate after each actual store. Loop heads reuse
that continuing invariant, not arbitrary old-cell facts or return promises. Shared
builds independently re-prove these bodies with interference between source
accesses; stale-cell copies can be accepted sequentially and rejected with workers.
Unknown effects, unsupported bodies and mutating fault cleanup remain conservative.
The [content example](../../compiler/examples/record_refinement/lists.loom) retains
alias identity, moving collection and Task handoff without repeated runtime checks.
Raw writable outer aliases cannot escape. Shared elements can return through checked reads when their stored
type graph cannot contain the protected outer header; recursive backreferences,
opaque captures and unresolved types remain conservative. Cyclic invariant construction rejects
before recursive expansion exhausts the compiler stack.

Fresh local List drafts can now be populated through finite non-publishing
helpers, aliases and loops before a checked publication boundary.
Existing origin analysis validates publication effects; raw aliases and captures
must have no later use, including cleanup. The built header is retained without
copying or a runtime monitor, and unknown predicates still return checked Results.
The [draft example](../../compiler/examples/record_refinement/drafts.loom) covers
CTFE, shared constrained updates and suspension. Publication follows blocks,
branches, matches, eager expressions and returns, including ordinary local
copies and unrelated waits. Pending operands and active defer/scoped cleanup
cannot transport raw aliases across that boundary, including fault-only exits.
Loop-local publication, mutable binding origins and general nested graph
publication remain open.

Every declared `ensures` requires a static proof. The CLI submits remaining
supported obligations to a compile-time Z3 process after the fast rules fail.
One query combines a function's unresolved return paths. Bool, mathematical
integers and UTF-8 byte sequences share logical symbols; Text byte lengths
are tied to sequence lengths. Unknown, timeout, malformed output and solver
failure never establish a proof. The public checker remains I/O-free unless its
host supplies `ProofBackend` through `BuildInputs`; this is a trusted capability,
like importing trusted cache evidence, not a source axiom. Native programs do not
link the solver. The [example](../../compiler/examples/smt_contracts/main.loom)
exercises sequence cancellation, empty content, Unicode byte lengths and integer
feasibility. Unresolved arithmetic, concatenation-size and modeled access safety
become separate path-guarded obligations in the same query as truth. Short-circuit
guards do not guard eager call arguments, and hypothetical operations supply no
successful-check assumptions. Symbolic integer `+`, `-` and `*` preserve general
expression terms alongside the affine fast path. Signed interval transfer over
these terms handles bounded safety/range obligations before SMT, with mathematical
endpoints and unchanged eager operand checks. Polynomial identities, safety
bounds and verified call summaries compose with Text lengths; see the
[polynomial example](../../compiler/examples/smt_contracts/polynomials.loom).
Symbolic integer division and remainder now preserve signed truncation toward
zero. Zero and `Int.min / -1` guards remain independent obligations for both
operations; hypothetical arithmetic supplies no successful-check assumption.
Fixed-sign divisor intervals prove quotient and remainder ranges locally,
including negative divisors and `Int.min` endpoints, without launching SMT.
Zero-containing or wider quotient intervals remain conservative; independent
fault obligations still apply. See the
[range example](../../compiler/examples/smt_contracts/division_ranges.loom).
Structural integer-term identity bypasses interval/SMT search without bypassing
evaluation safety. See the [division example](../../compiler/examples/smt_contracts/division.loom).
Immutable `std.text.byte` observations now compose with this same integer and
UTF-8 sequence algebra. Normal-return bounds of 0–255 prove locally, including
short-circuit branches; Text equality and concrete byte contents use SMT.
Hypothetical accesses independently require a nonnegative, in-range byte index.
See the [byte example](../../compiler/examples/smt_contracts/text_bytes.loom).
Signed Int bit operations now share this expression model: mask ranges and XOR
parity normalize locally; remaining supported word relationships use SMT.
Shift counts and eager arithmetic still need independent safety proofs.
See the [bit example](../../compiler/examples/smt_contracts/bitwise.loom).
General collection induction and content/type-dependent variadic proofs remain
unsupported; nonlinear solver queries may still be unknown or time out.
Constrained construction, stable scalar facts, pure-helper implication and
supported List-append preservation now share the same optional backend and
batched safety/truth obligations. Unknown or failed implication retains the
boundary check; eliminating it preserves input evaluation and faults. The
[refinement example](../../compiler/examples/smt_contracts/refinements.loom)
exercises sequence-length weakening, integer roots and nonlinear local facts.

Current reasoning includes
scalar/inline-aggregate identities, guarded preconditions/assertions, bounded
integer difference relations, input-type invariants, inferred scalar/inline-aggregate loop
invariants, finite pure helpers and verified callee summaries. Finite helpers
support local reassignment, conditional expression operands and
short-circuit writes while retaining prior value snapshots and evaluation
obligations, including overwritten calculations. This adds no runtime proof
state and does not treat shared-storage mutation as pure.
Constrained construction can combine stable immutable scalar flow facts with a
pure helper predicate in a private proof queue. Actual binding identities stay
distinct, and helper requirements and hypothetical arithmetic still need proof.
Established scalar guards can themselves call bounded pure helpers. Their
selected type/static instances are rebuilt in the private proof closure, and
guarded checks are retained without adding executable roots or replaying calls.
Opaque candidates do not hide independent supported facts.
Compile-time results preserve admitted constrained List types while rebuilding
fresh private graphs. Aliases, nested storage and callback captures retain their
identities; original construction, escape and mutation checks still run before
evaluation. Restoration is not another source construction boundary.
Immutable Text participates through byte equality and copied-value identity,
including helper-defined refinements, without storage-address assumptions.
Mutable bindings, heap reads and caller effects are not imported as stable facts;
unknown results retain the checked `Result` boundary.
Unmodeled pure scalar facts no longer hide supported conjunction terms at entry,
flow construction or weakening/helper-requirement boundaries.
Dropping evidence proves neither that arithmetic nor its safety; disjunctions,
unaccounted effects and storage retain their conservative boundaries.
Expression composition now shares operation descriptions for operand shape,
conditional evaluation, heap observations and totality. Theory-specific value
and safety rules remain separate from expansion and traversal; adding a primitive
does not require enumerating its conditional operand combinations. Correlated
guards normalize without a larger budget, while unrelated guards, eager faults
and entry-state bounds remain independent obligations. This is not unrestricted
theory combination or a complete proof procedure.
Bounded affine equality elimination now composes multiple established equations
and inequalities over the same mathematical scalar symbols, regardless of
whether they originated as parameters, fields, callee results or length
observations. Fraction-free positive scaling keeps relation signs and definedness
obligations; overflow, disjunctions, Text contents and invalidated List state
gain no invented evidence. Nonlinear terms and the bounded quantified scans below use SMT.
Bounded inequality elimination additionally combines multiple upper/lower bounds
using positive scaling and addition, including weighted terms and strict integer
gaps. It refutes the negated goal without assuming hypothetical arithmetic is
defined, treating alternatives as conjunctions or carrying stale heap facts across
mutation. Ordinary interval and equality queries remain the first paths; this is
not complete integer feasibility or unrestricted theory combination.
Synchronous scalar
and inline aggregate concept calls use declared contracts through generic/associated or dyn receivers,
not a guessed implementation or hidden receiver knowledge. Private abstract
summaries are neither executable CTFE bodies nor native functions. Unspecified
leaves remain independent; shared siblings supply no content or alias facts.
Immutable Text values, exact literals and established equality facts participate
in required proofs, including Text leaves beside shared siblings. Distinct unknown
values never imply unequal contents. Established equality chains and inequalities
between equal-value classes compose; disjunctions and unequal chains supply no
invented equality or inequality. Immutable UTF-8 byte lengths, their nonnegative
bounds and equality consequences compose with entry values, pure helpers and
input-type constraints. Proved length-based weakening omits the second check;
unknown strengthening retains it. Equal lengths do not establish equal contents.
Bounded concatenation proofs preserve ordered contents, byte-length sums, empty
identity, reassociation and established atom equalities. Pure helpers, entry
values and constraint weakening compose without runtime proof allocations;
hypothetical sizes and eager helper obligations must still be proved. This is
not a word-equation solver. Substring reasoning is not implemented; Bytes remain
opaque.
List lengths belong to the current proof state: exact aliases share extent
updates, while unknown overlap and opaque calls forget current-length facts.
Scalar reads remain snapshots. Length helpers, literals/allocation, indexed
reads/writes and append compose with generic/inline values and bounded loops.
Fresh allocations are disjoint from existing handles; later unknown reads or
returns may alias them. Copy loops infer output-length/index equalities and
check them inductively, preserving input extents only with that separation.
Input length refinements supply entry facts; callee preconditions are not reused
as post-call heap facts. Element observations now track literals, appends, writes
and repeated reads at proved-equal indices, bounded to 64 per handle. Other
indices survive writes under inequality guards that may be proved by a later
branch. Multiple writes, including cleanup, conjoin their separation conditions;
rebound indices do not change these snapshots. Unknown handle overlap and
opaque calls discard content facts. Contracted length-preserving calls retain
untouched observations using bounded may-write analysis of their checked bodies,
including immutable parameter/inline aliases, nested calls and ordinary cleanup.
All branches contribute writes; mutable locals and heap-derived targets are
unknown. Changed helper bodies invalidate dependent cached proofs, even when
their declared contracts stay unchanged. No-result helpers with finite checked
effects need no additional contract to frame untouched storage.
Mutating loops use fresh element values and
retain only entry/backedge-proved bounds and source-contract relations when
lengths are preserved. Lost candidates trigger dependent rechecks; ordinary
cleanup and loop jumps retain their order. This covers bounded element counters
and two-element sum conservation, not arbitrary-index array invariants. Indexed entry
checks and storage postconditions compose; hypothetical postcondition reads
must prove bounds and have an established observation or modeled scalar content version. Unknown reads never
supply non-aliasing evidence. Immutable entry-element snapshots require entry
bounds and survive writes, including through per-invocation summaries. See the
[indexed example](../../compiler/examples/list_elements) and
[List contract example](../../compiler/examples/list_contracts).
The optional solver models List content versions with guarded reads
and `select`/`store` equations for Int, Bool, Text and Float elements and immutable field
paths through nested inline records/tuples. Arbitrary valid parameter indices can prove
write-then-restore and swap/frame contracts, without deciding index equality at
each write or enumerating concrete indices. The finite observation cache stays
the fast path. Alias uncertainty, opaque effects, loop havoc and shared
interference discard current versions; immutable old equations cannot restore
them. This adds no runtime copying.
See the [heap example](../../compiler/examples/smt_contracts/heap.loom).
The [projection example](../../compiler/examples/list_contracts/projections.loom)
also proves replacement, append and source-loop preservation of a multi-field
constraint. Bounded scans support local element observations and Boolean tails;
their access domains and short-circuit guards remain proof obligations.
The [mixed-scalar example](../../compiler/examples/list_contracts/scalars.loom)
uses the same rules for Bool, UTF-8 byte sequences and IEEE Float fields, importing
pure helper constraints from typed replacements. Storage equality is distinct
from Float numeric equality: restoring a value does not establish reflexivity
without excluding NaN. Local proofs precede helper preparation and SMT; queries
receive the established helper facts before search. Propositional tautologies
are folded after evaluation safety has been modeled, and quantified conjunctions
split by logical equivalence rather than sharing a cross-column trigger.
Unobserved mutable siblings remain shared. Field equality
supplies neither whole-record equality nor permutation; mutable graphs remain opaque.
Whole List entry values now capture their logical length and outer content
version through `old(values)`, including inline parameter fields, refined Lists
and finite pure helper selections. Int, Bool, Text and Float columns reuse the
current-array theories, including nested inline fields beside shared siblings.
Queries can use post-state indices such as
`result`, but must establish bounds against the entry length. Checked read-only
calls retain versions; possible writes retain only sound partial frames. Shared
workers require private or validated read-only storage for a coherent whole entry
value. This is not an alias, runtime copy or arbitrary graph snapshot. See the
[snapshot example](../../compiler/examples/smt_contracts/snapshots.loom) and
[typed entry columns](../../compiler/examples/list_contracts/snapshots.loom).
Ordinary bounded pure Boolean scans derive scoped universal predicates and
their existential duals from uniform constant early returns. Multiple/nested
conditions and per-iteration local rebinding retain their source guards; mixed
or computed Boolean returns select the first exiting iteration. Outer-state
rebinding remains unsupported. See the
[branch example](../../compiler/examples/smt_contracts/scan_branches.loom).
Guarded tails, immutable
typed columns and loop-write proofs use the same rules; see the
[search example](../../compiler/examples/smt_contracts/searches.loom).
Complete equality scans over `List[Int]` also derive occurrence counts.
Finite histogram laws compose with array stores and
verified call summaries; loop candidates derived from quantified postconditions
must pass entry/backedge proofs and rechecking after removal. The
[quantified example](../../compiler/examples/smt_contracts/quantified.loom)
proves ordering and permutation for a returned in-place sort, plus swap and
reverse permutation. Terminal local/field return paths propose loop hints, not
assumed result identities; each actual return retains its own proof. Wrong
ordering, lost duplicates, stale entry reads and unproved
full-range safety reject. No helper names, runtime copies or sampled tests supply
proof evidence. General scan shapes, mutable element graphs and unprotected
shared observations remain outside this fragment.
The [copy example](../../compiler/examples/smt_contracts/copies.loom) proves
fresh-output element equality and permutation, then composes the returned value
with the sort's guarantees. Loop havoc uses checked may-write targets and
allocation-time disjointness; unknown aliases still invalidate observations.
Affine output-length candidates also cover appending to a nonempty target,
including a captured-length self-append. Repeated facts and inherited quantified
contexts are shared logically rather than weakening safety obligations.
Direct source calls can now reuse checked guarantees at constrained construction,
without replaying argument effects or bypassing fresh-storage validation. The
[construction example](../../compiler/examples/smt_contracts/constructions.loom)
returns a read-only constrained sorted copy directly; unknown ordering keeps
the checked `Result` boundary. CTFE ignores statically proved `ensures` metadata
while retaining executable preconditions, bodies and faults.
Completed sequential proofs are shared across private queues in the same checked
snapshot, not across edits or via pending promises. Shared verification
remains separate.
Recursive body calls now use a closed typed group: every source member must prove
its declared normal-return guarantees before any group result escapes. False
base cases reject in either member order and after cached edits. This covers
self, mutual and generic recursion, including scalar guarantees from List-backed
tree traversal and loops, in the existing proof fragment. This is not termination
or recursive predicate expansion. See the
[recursive contract example](../../compiler/examples/recursive_contracts/main.loom).
Bounded content refinements now establish sequential List entry observations,
including nested fields and `old(values[0])`, without duplicate preconditions or
runtime checks. Unknown alias writes invalidate them; disjunction alternatives
and length-only refinements supply no invented element facts. Shared interference
requires separately validated storage evidence.
Successful typed observations, conversions, verified returns and loop-local
replacement values also reuse supported scalar/inline/Text invariants. This
allows contracts over `List[Positive]` elements without duplicate assertions.
Guarded `old` and hypothetical current reads retain their access-validity
premises; an impossible element type in an inactive branch supplies no false
proof. Shared observations remain independent, as in the
[typed worker example](../../compiler/examples/workers/typed.loom).
Unsupported or exhausted required proofs reject;
they never become runtime postcondition checks. `old` composes immutable entry
parameter paths, aggregates, arithmetic and finite pure helpers, retaining
definedness obligations. List entry lengths and immutable elements, including
pure helper observations,
survive mutation and compose using each callee's invocation state. Scalar, inline
and supported List proof snapshots require no runtime allocation. `old` operands
cannot use mutable element handles, callbacks or body-local/result bindings. Entry
element bounds may use immutable entry guards from short-circuit clauses and
pure helper branches. Post-state storage and results cannot justify entry reads;
conditional observations remain conditional after growth and across calls.

Immutable observed record fields can coexist with unobserved mutable siblings.
Fixed-shape List views retain element identities across removal/regrowth,
moving GC and suspension. Explicit range capture has a verified normal-return
length contract; it composes with fixed-length construction without another
predicate check. Range registration discards private-storage/frame evidence,
not granting content invariants or an unchanged mutable source length.
Private range metadata has a scalar type constraint, so the verified `length`
contract also supplies nonnegativity for arbitrary `View[T]` parameters.
Explicitly isolated Lists with immutable elements
support constrained construction; copies of the constrained value share.
Length-only predicates permit element writes, and bounded preservation proofs
admit some appends. Content-dependent predicates admit proved atomic replacements,
appends and multi-step helpers whose every store preserves the predicate; unproved writes
remain forbidden. Shared builds withdraw read-only content evidence
for nominal types with such writes in the checked program, including after cached edits.
Admitted appends also withdraw fixed-extent evidence. Shared helper preservation
does not treat a sampled length/content guard as a reservation against other aliases.
There is no implicit copy, monitor or alias-triggered runtime failure.

Scalar, List and supported inline aggregate loops infer entry/guard bounds and
check inductiveness to a fixed point. Rebound List leaves supply length bounds,
not retained handle identities. Mixing
rebinding with possible resizing freshens all tracked extents before induction.
Zero-iteration paths and early returns retain separate obligations. This supports
whole-value record/tuple reassignment with independently fresh leaves,
branches, nested loops, break/continue paths and existing
direct-call proof rules. List extent-changing loops freshen affected lengths before
checking inductiveness. Scalar lexical cleanup retains its checked order and
return snapshots. Guard calls use fresh checked results, not stable syntactic
invariant terms. Resource cleanup and general heap-content proofs remain unsupported. See the
[native loop example](../../compiler/examples/loop_contracts).
Written Int guard cursors propose mathematical entry-difference and weighted
pair relations after existing storage/quantifier candidates. Constant affine
translations supply coefficients, never proof: every relation needs entry and
all-backedge verification. The [difference example](../../compiler/examples/loop_contracts/differences.loom)
covers symbolic offsets, helper/cleanup updates and structural pack minimums;
the [affine example](../../compiler/examples/loop_contracts/affine.loom) covers
different-rate and opposite-direction counters and CTFE. Source arithmetic
definedness is unchanged. Finite pure helpers and exact checked scalar/inline-field
summaries supply the same coefficients through an isolated optional expansion;
unspecified leaves and exhausted hints establish nothing. The
[helper example](../../compiler/examples/loop_contracts/helper_affine.loom) covers
once-only effects, generic/dyn guarantees and structural packs. Generation remains
bounded, guard-anchored and pairwise, not general affine invariant synthesis.

Unrestricted content mutation, strengthening existing mutable alias graphs,
general loop/recursive proofs, arbitrary `old` snapshots and general Float induction
remain open. The bounded sorting/permutation story above does not close these
broader gates. Exact supported rules and examples are in the
[contract reference](../../compiler/README.md#contract-boundary).

## Standard library, memory and async

Source `std` includes text/numeric/byte operations, List algorithms, Map/Set,
Option/Result, typed JSON, storage descriptors/codecs, reflection, environment,
filesystem, process and I/O APIs. JSON has no special runtime implementation.
`std.list.new[T](capacity)` reserves storage for an empty, normally shared List;
source copies and fixed-size maps avoid repeated growth without changing the
default allocator policy. Compiler cache copies use the same source API.
Source [`std.iter`](../../compiler/std/iter/README.md) provides an associated-type
pull protocol, shared-view List/range sources, lazy map/filter/take and
collect/fold/try_fold/any/all consumers, including dynamic iterators. Fallible
folds preserve callback effects and stop before pulling a remaining suffix.
Callbacks retain pull order, faults and lexical cleanup.
[`std.file.lines`](../../compiler/std/file/lines/README.md) provides incremental
synchronous UTF-8 file lines with explicit error items and scoped closure,
including direct generic consumers. Resource factory overloads of map/filter/take
compose owned scoped pipelines without copying a live borrow; nested disposal
uses the existing compiler rules. Collection hash/order laws remain caller
obligations. Source [`std.stream`](../../compiler/std/stream/README.md) now supplies
generic/dynamic async pulls, map/filter/take and sequential consumers. Callback
errors, short-circuiting and cancellation retain ordinary Task semantics.
[`TCP chunks`](../../compiler/std/net/tcp/chunks/README.md) borrow an explicitly
closed socket, yield fresh buffers and distinguish errors from EOF.
Source JSON now has an incremental byte decoder and an async Stream consumer,
including typed construction from the parsed tree without a Text round trip.
UTF-8, escapes and numbers may cross chunk boundaries; container state and the
result tree grow without buffering the full document text. EOF/trailing input,
producer errors and cancellation use existing library/Task semantics. The
[real TCP trial](../../compiler/examples/json_stream/main.loom) also runs at
O0/O2 under moving-GC stress. A source encoder now emits bounded fresh byte chunks;
the async writer awaits each sink with checked full-chunk counts. JSON errors,
sink errors and cancellation preserve partial-output and caller-managed resource
semantics. Open containers retain fixed-shape views, not deep snapshots.
Typed streaming encoding uses specialized source callbacks without a complete
intermediate Value tree or runtime schema registry. Source Stream resource
factories now compose owned `from_iter`/map/filter/take pipelines with a final
scoped owner. Directly awaited borrows keep that owner alive while child Tasks
drain on cancellation and faults; saving or forwarding a borrowing Task rejects.
The [file pipeline](../../compiler/examples/stream_lines/README.md) covers real
input, early termination and errors at O0/O2 under moving GC. Lifting synchronous
file lines does not provide nonblocking file I/O. Document sequences, general
borrow-retaining adapters, async resource acquisition and application-grade
networking remain open.

Stop-the-world copying GC preserves precise typed roots, sharing and cycles;
large-object storage is separate and stress tests relocate all sizes. Ordinary
nonallocating functions stay root-free. Checked control-flow liveness retires
dead local roots at block entry and complete statement boundaries; pending
operands retain separate snapshots. Cleanup captures remain conservatively
rooted. Expression-internal retirement and generational/concurrent collection
are not implemented. Source exposes no
addresses, finalizers, weak references or ownership/borrow syntax.
Small allocation and forwarding metadata lives in private heap headers, with
page-indexed base bitmaps rather than per-object hash tables; large objects remain
separately tracked. This avoids object-table capacity cliffs during large restores.

The private [shared-heap boundary](../../compiler/runtime/src/shared_heap.rs)
separates mutator root chains from object storage. Native tests cover concurrent
collectors, parked waits, checkpoint reloads and thread-local fault rollback on
one moving heap. Private per-object access guards keep their identity through
relocation, park contended waiters and release before fault cleanup. Idle Task
reactors and native-I/O drain waits also park, allowing another mutator to collect
before publishing completion. Source `std.task.worker.run` now lowers callbacks
to typed worker frames. A separate bounded CPU pool attaches mutators; stable
traced slots retain queued and completed frames across collection. Generated
List/Bytes/captured-cell access guards protect typed publication and bounds.
Native blocking I/O snapshots managed inputs and parks before OS waits.

Lexical `defer` and `scoped` handle normal exits, propagation, loop exits,
language faults, suspension and cancellation. MustScope freshness/escape checks
and typed cleanup cover nested records/enums/Lists and recursive resource trees.
Cleanup drains after secondary faults while retaining the first diagnostic;
OOM/external termination offer no guarantee. Scoped resources cannot transfer
into Tasks, but directly awaited calls may borrow them until all child cleanup
finishes. NoSuspend guards retain their stronger suspension restriction.
See [cleanup](../../compiler/README.md#lexical-cleanup).

Source `std.sync.mutex` provides shared mutex identities and lexical
`MustScope`/`NoSuspend` guards. Normal exit, returns and faults release the lock;
same-thread reentrance faults. Private resource payloads prevent reconstructing
duplicate guards through field projection. Native tests exercise cross-thread
updates and moving GC; source tests cover O0/O2 cleanup and rejected guard use.
The native worker activation boundary now interrupts blocked mutex acquisition,
resumes the mutator before lexical drain and keeps cancellation distinct from a
fault. Cleanup remains non-cancellable; a cleanup fault is retained. A focused
test drains a cancelled child while its parent still holds the requested lock,
including moving collection and cleanup allocation. Source worker tests cover
the same cancellation/fault drain, forced lost updates, synchronized captured
updates and aggregate publication at O0/O2 under moving-GC stress. See
[workers](../../compiler/README.md#shared-workers).

Stackless Tasks lower into typed state machines and GC-traced frames, using one
owner-thread ready queue. Only needed suspension state spills. One-shot handles
transfer through functions, callbacks, dyn methods and aggregates. Outcomes,
draining cancellation and tuple/List joins are source library policy over narrow
completion notifications. Tuple `.await` joins and scalar `.await?` preserve
ordinary typed result rules.
Source `std.task.deadline` composes cancellation with a monotonic deadline,
preserving completed results and draining both work and timer subtrees. It
requests cancellation, not a hard return-time guarantee for blocking OS calls.

Real timer/readiness/completion registration wakes the owner without per-Task
threads or busy polling. Nonblocking TCP/UDP, OS DNS, asynchronous files and process
capture are implemented; blocking I/O uses bounded native workers with copied native data,
not managed pointers. Cancellation of a running OS call waits for completion.
Process capture adds native pipe-drain threads and unbounded output buffering;
cancellation reaps the direct child without terminating process trees.
TCP exposes numeric local/peer endpoints, TCP_NODELAY, explicit keepalive
idle/interval/retry configuration and write half-close,
preserving pending receive registrations for EOF-delimited exchanges. Text-to-Bytes
encoding is source-library policy and returns an independent buffer.
UDP preserves datagram boundaries and numeric senders, including empty packets;
oversized receives consume the packet and return an explicit truncation error.
Source policy uses the existing reactor, cancellation and explicit close/abort,
tested on IPv4/available IPv6 at O0/O2 with moving GC. Fixed-peer UDP Connections
reuse the same identities and waits; kernel peer filtering, empty sends, numeric
endpoints and alias revocation have source tests. Creation is not a remote
handshake or delivery guarantee. Unconnected IPv4 sockets expose explicit
default-off broadcast configuration/query with alias and stale-token tests;
configuration does not prove delivery under host routing/firewall policy.
Explicit multicast join/leave accepts IPv4 interface addresses or IPv6 indices,
with family, range and stale-identity checks. Separate source configuration sets
the outbound interface, loopback and hop limit over existing socket2 operations;
OS failure may apply only part of that policy. Native tests query configured
options and retain active leases. The native example/source tests use TTL zero
for real local IPv4 multicast packets, delayed readiness, deadlines and moving
GC. This is not routed or IPv6 multicast delivery evidence; default-interface
support remains host-dependent.
UDP hostname connect composes existing async DNS with ordered endpoint selection;
it does not probe peer reachability. The numeric overload still omits the
resolver operation from native IR. Source/native tests cover resolved fixed-peer
packets and invalid/exhausted inputs, without a new runtime boundary.
TCP hostname connections interleave address families with configurable bounded
concurrency, stagger and a total cancellation deadline. Numeric address races
share that source policy. Sockets are registered before suspension, so completed
losers and unextracted child results cannot escape cancellation cleanup.
`std.net.tls` supplies verified client/server streams, custom PEM roots, ALPN,
binary I/O, required mutual certificate authentication and TLS close-notify.
Client/server identities and trust are explicit; verified peer leaf certificates
can be copied as DER for application policy. Rustls owns protocol/cryptography in a separately
linked provider; Loom owns TCP, suspension and cancellation policy. It verifies
certificate chains, time and names, and distinguishes truncation from clean EOF.
One reader and one writer can run concurrently per shared TLS connection, with
ordered encrypted writes and generation-checked completion wakes. Cancellation
retires the connection and its socket waits, waking the other direction to fail.
O0/O2 tests include simultaneous 8 MiB transfers, moving-GC traffic, rejected peers,
and independent Rustls interoperability with both-direction cancellation under
backpressure. Revocation policy and broader socket options remain open.
The [accepted shared-worker semantics](../rfcs/tasks.md#shared-workers) allow
memory-safe logical races with explicit synchronization for compound updates.
Only explicit workers request parallel execution. No-worker executables retain
their direct lowering; worker-enabled builds conservatively instrument accesses
and roots. Checked escape analysis now removes guards for invocation-local
List/Bytes/frame accesses, including aliases and fresh return buffers. Container
and capture edges, all assignments and cleanup participate. Direct-call alias,
publication and return summaries reach a fixed point, including recursion;
helper parameters lose guards only when all their call contexts are private.
Mixed, callback, dynamic and external-entry contexts stay conservative, without
cloning helper bodies. GC roots and cancellation checkpoints remain.
Required proofs are revalidated for interference: scalar snapshots and private
List observations remain stable; finite private factory summaries compose.
Validated content-constrained inputs also retain their storage observations,
including refined record/tuple fields, bounded sorted predicates and `old`.
The existing construction/write/escape analysis supplies this evidence. It
neither promises private/disjoint storage nor changes runtime worker guards.
Validated replacement-only refinements retain length snapshots, not elements;
append-capable refinements re-establish their shape predicate for each independent
length observation, including `old`. Arbitrary read-only bodies supply no storage
guarantee. The [shared shape example](../../compiler/examples/workers/shapes.loom)
exercises both policies and nested refined fields.
Checked source allocation/return origins recognize explicit shallow copies
from shared inputs, including `std.list.clone` through inline wrappers. No value
contract or deep copy is inferred; multiple returned mutable leaves may alias.
Fresh outer headers remain private when initialized with shared elements;
mutable elements read from a copy do not inherit that privacy. Source helper
calls are checked for publication even when their result is scalar or Unit.
Mutable shared entry, old and current observations are independent: nonnegative
lengths are provable, but repeated reads, stale index guards and hypothetical overflow
gain no evidence. Once-only helper arguments and local bindings retain scalar
snapshots through substitution, inline fields, guarded branches and cache reuse;
independent invocations receive separate observation identities.
Storing mutable graphs into shared storage and opaque calls conservatively lose
privacy, and loop backedges cannot restore it. General synchronized heap
reasoning remains open. Source library
compound operations still require caller synchronization when racing mutations
would violate the desired application semantics.
See [Tasks and I/O](../../compiler/README.md#source-tasks).

## Modules, caching and performance

Path and exact HTTPS Git/fork dependencies support importer-local module
instances and offline locked builds. Git `subdir` selects a monorepo module and
binds its exact directory to the lock; modules at the same URL/commit share one
whole-repository snapshot. Sibling path dependencies stay within that snapshot.
`resolve` alone fetches sources. Cache validation checks actual bytes and
membership, not sidecars. Distinct source instances retain distinct nominal types.
Resolution defaults to anonymous; an explicit trusted credential helper enables
private HTTPS sources without persisting credentials or exposing remote output.
Real loopback HTTPS Git tests cover authenticated fetching, helper failures and
offline cache reuse. Root dependency entries with `scope = "graph"` explicitly
unify matching declared edges on one path/Git source; transitive declarations
cannot select another application's graph policy or grant extra imports.
Effective Git choices are locked per importing edge. Explicit SemVer resolution
discovers lightweight/annotated tags, checks module versions and both original
and graph-selected requirements, and searches candidate-dependent import closures.
Overlapping ranges prefer a shared source instance; disjoint ranges retain distinct
nominal types. Locks pin requests, labels, commits and verified content. Ordinary
commands remain offline. `std.semver` owns parsing/range policy; no runtime or
host-language resolver is added. Conflicting-graph search can be combinatorial.

Opt-in trusted-local object reuse hashes checked input/backend identity and
always relinks. Frontend reuse binds compiler bytes, loaded sources, modes,
options and observed inputs; discovery/parsing still run. On closure misses,
resident or persistent definition evidence can reuse eligible abstract checks
and concrete bodies with current bindings, types, calls and source locations.
Generated/staged/variadic/method/dyn/async/scoped bodies have native regressions.
Captured-frame bodies rebuild their environments; expression-macro consumers
remain conservative. Failed checks do not publish evidence. Native objects are
still whole-closure, and trusted-local snapshots are not portable proof artifacts.

The [edited-source benchmark](../../compiler/README.md#frontend-cache) uses
fresh processes and requires a whole-closure miss. Its edited helper now has a
required contract and a call from `main`; each warmed compiler/cache must reject
a false-contract edit before measurements. Earlier uncalled-probe timings below
describe the older workload, not this stronger one. One-shot CLI checks serialize
and retire temporary source/body aliases before returning mutable results;
resident editor caches still keep isolated copies. Body eligibility and concrete
callee reconstruction are memoized only within the current check, never across
edits. Private snapshots also retain validated source/test selection: export can
skip reparsing only when both text and the complete AST, including spans, match.
Public AST-only edits cannot inherit this evidence. Two five-pair alternating
macOS runs measured edited-source checks at 1460/1310 ms and 1550/1453 ms before/after
this syntax-validation reuse, with identical definition/body reuse counts. Peak
memory remained about 1067–1124 MiB. An uncached pair measured 1221/1200 ms and
472/482 MiB. Restoration, replay and writeback remain expensive; caching stays
opt-in. This is not a universal speedup or completion of the compiler-latency goal.
Separately, five alternating fresh-process checks of the same compiler sources
measured 2749 ms / 779 MiB before and 1188 ms / 486 MiB after removing repeated
trusted `Result` discovery and package-label allocations. No incremental cache
was enabled. Verified source shapes are reused only within the current check;
an edited binding snapshot validates them again. This is one macOS workload,
not a latency or memory guarantee for other projects.
Trusted bundles hash their existing payload range before decoding. Text snapshots
use a UTF-8 checksum trailer to avoid an intermediate full-buffer copy; native
object bundles copy only the validated object, not a second metadata buffer.
The source `std.hash.sha256` range overloads retain fixed scratch storage and
compile-time execution. Integrity and observed-input validation remain mandatory;
these changes do not make local snapshots portable proof artifacts.
Private decoded IR shares empty lists only within its read-only snapshot. Replay
and resident-cache detachment still create independent mutable public lists.
Raw bundle buffers and decoded metadata retire before restoring cached IR; only
the authenticated snapshot text remains live across that phase boundary.
Nine alternating macOS pairs with the same private source edit measured
1828/1006 ms and 795/458 MiB before/after checked local-root retirement and inline
small-object metadata. Both compilers reused 2189 definitions and 3494 bodies;
each edit required a whole-closure miss. Small packages were mostly unchanged.
An uncached nine-pair self-check measured 1704/1244 ms and 439/274 MiB.
These are individual macOS workloads, not completion of the feedback gate.
See [native benchmarks](../../benchmarks/basic/README.md) separately; compiler
latency is not interpreter or application runtime performance.

## Semantic changes, deployment and delivery

The [semantic change trial](../../tools/semantic_change_trial/README.md) uses
explicit stable-ID sidecars and directory/Git snapshots for reviewed move-plus-edit,
private nongeneric function rename-plus-edit and one-sided additions. Rename
normalization compares checked references through exact token edits in production
and test views; local shadows, conflicting names and incomplete evidence reject.
Public/generic rename composition remains unsupported. Pinned contexts and checked type, initializer and
value-path targets prevent silent nominal or intermediate-field binding drift.
Applying creates a new tree; arbitrary edits, cross-package
merges and automatic identity tracking are not implemented.

The [SQLite migration trial](../../tools/deployment/sqlite_migration_trial/README.md)
executes one offline orders upgrade, data-preserving downgrade and re-upgrade.
Append-only events distinguish assumptions and observations; plans bind retry
inputs, preflights recheck within transactions, and artifact receipts are rehashed.
The operator still supplies storage mappings. Hotfix inspection reports proof
as unknown and cannot execute a hotfix. General migration packages, application
mapping proof, online coexistence and generalized recovery remain goals.

CI packages relocatable development archives for macOS/Linux/Windows and tests
them outside the checkout. Host LLVM/linker dependencies remain external; these
are not self-contained published releases or evidence for other targets.

New status updates should replace the relevant boundary above, not append a
second completion narrative. Preserve detailed semantics in the compiler guide,
examples and accepted design records; preserve historical progress in Git/PRs.
