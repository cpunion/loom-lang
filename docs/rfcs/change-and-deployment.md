# Changes, library evolution, and deployment

Status: Accepted direction — 2026-09-06

This is design direction, not implemented features or CLI grammar.
It complements [Language foundation](language-foundation.md); the
[roadmap](../../ROADMAP.md) sets implementation order. These acceptance stories
do not require a new version-control or deployment control plane.

## Story 1: Evolve a library without hiding dependency changes

An application uses a search module and an export module. Both depend on a codec
module, possibly through different versions or forks.

A directory is a package; a manifest manages a versioned module of packages.
Multiple versions may coexist. Prefer one locked instance satisfying all version,
source, and behavior-affecting configuration requirements. Normalization selects
a permitted version, without proving different versions behaviorally equivalent
or prescribing a selection algorithm.

- Overlapping version requirements may share an instance.
- Disjoint requirements may retain separate instances.
- Ordinary builds use the locked graph without silently refreshing or deduplicating
  it. Explicit resolution updates report the resulting changes.
- Equal names and version labels do not justify merging different sources or
  contents. Respect explicitly selected forks and instance configuration.
- A fork selection is local by default. A whole-graph override is explicit in
  that dependency entry, without a separate `replace` mechanism; incompatible
  requirements are reported rather than silently overridden.

Internal codec instances need not cause type errors. If search returns one
instance's public `Doc` while export expects another's, equal names or fields do
not unify nominal types. Use a shared API, explicit adapter, or unified instance.

Native ABI and process-singleton contracts may restrict coexistence. Module
identity does not isolate external resources: check actual resource keys and
contracts. Sharing is not inherently an error; duplicated modules are not
automatically isolated copies.

Public generic requirements remain explicit. Overloads use argument types and
arity. Updates may change a call's selected definition without deleting a
declaration; report changed bindings as semantic changes. Stable identity and
type checking alone do not prove deployment compatibility.

## Story 2: Merge ordinary source and retain useful feedback

One developer moves a definition; another edits its body in ordinary `.loom`
files. Unambiguous identity and bindings permit the edited body at the new location.

Users retain their file organization, comments, and layout. Loom parses syntax
and resolves bindings for semantic change analysis; it does not require an AST
editor or make a semantic database the source representation.
One Loom change entry point reuses an existing version-control engine for
history and storage. The exact Git/jj adapter is not settled here.

Lightweight versioned metadata outside source preserves declaration identity
across renaming and movement. Ordinary compilation does not depend on this
history. Copying creates a new identity; split/merge records retain lineage.
Changed package scope or visibility is semantic, even when identity is retained.

- Independent edits combine without treating adjacent source lines as conflicts.
- Move plus edit follows the identified declaration, not its obsolete path.
- Split, merge, copy, and substantial rewrite must not be guessed from similarity
  when several identity mappings are plausible.
- Delete versus edit or a new reference retains the conflicting intent.
- Comments follow their identified declaration or body region; file-level text
  stays with the file. Ambiguous attachment is reported, not silently discarded.
- Analysis uses the actual common baseline. A stale location or identity mapping
  cannot overwrite later work or create a duplicate definition silently.

Ambiguous drafts and conflicting work may be saved. A merge that relies on an
unresolved identity, or deployment of an affected closure, must resolve it first.
Unrelated editing and builds need not wait for that resolution.
Semantic review includes reference and overload-binding changes, not merely
declaration names or AST similarity.

Feedback should quickly refresh affected checks and pure computations. Retained
old results are visibly stale and retain their program basis. Editing, saving,
or restoring a session must not automatically replay external effects.
A Workbench UI is not a prerequisite.

## Story 3: Upgrade the database that is actually deployed

A service has persisted orders under an older schema and Loom mapping. Its next
deployment tightens a constraint, changes storage, or applies a hotfix.

Compatibility obligations arise from effective deployed versions and the data
they established, not every source commit. No active old process does not mean
no old obligation: records, archived data, and unfinished operations can still
depend on old definitions.

A deployment basis identifies closed definitions, resolved dependencies, sources,
configuration, and tracked file, environment, and target compilation inputs.
A commit provides provenance, not a complete basis. Compare the previous effective
basis and mappings with the actual candidate artifact, not merely version labels.

### Inspection and records

Deployment tooling records transitions so users need not remember a separate
registration step. Dry runs default to offline inspection of deployment-store
metadata. Live inspection or execution is explicit. Specifications and records
contain references and necessary summaries, not secrets, raw data, or raw logs.

Keep these states distinguishable:

- Not deployed.
- Started, with outcome not yet known.
- Failed, retaining partial-progress information.
- Complete, with checked completion conditions.
- Rollback started.
- Rollback complete.
- Historical state explicitly assumed rather than observed.

Manual corrections preserve provenance and the previous record. Marking a
deployment absent does not undo database effects. An offline assumption of
completion remains an assumption, never an observed successful execution.

### A complete migration package

The migration package covers the incompatibilities between its declared starting
states and target. Users need not supply a separate proof for every source
fragment, but an omitted conflict is not a successful package-level check.

For example, changing order amounts from `amount >= 0` to `amount > 0` requires
handling old zero amounts. Finding no zero amounts today does not eliminate this
schema incompatibility or justify omitting its migration handling.
A hotfix without migration needs compatibility validation or proof against the
deployed basis; a familiar version label is insufficient.

Direct upgrades may skip intermediate versions and unnecessary transformations.
They must satisfy their own preservation and target contracts, not pretend to
restore information already lost. A partially migrated database is a starting
state to account for, not simply one completed version.
Online coexistence, stopped-service migration, throttled background work, hot
migration, and blocking incompatible accesses are selectable policies.

Failure handling is explicit and resolved before execution; tooling must not
guess between continuing, stopping, or rolling back after a failure.
Rollback support is required, backed by an executable strategy including
data retention and recovery, not merely a command or a relabeled status.

Suppose v2 adds order notes and the service returns to v1. Keep the new table and
data, or preserve them in a recoverable archive, unless loss was explicitly
allowed. Re-upgrading must include retained notes and subsequent v1 writes in
compatibility and conflict handling. A successful backup command alone does not
establish recoverability.

A proved migration plan is not a completed migration. Completion conditions
cover actual data state and the writers that can produce subsequent data;
reaching the end of a backfill is insufficient if old writers can reintroduce
incompatible records. Partial failure must not be recorded as completion.

### Evidence and system responsibilities

Keep proof, runtime check, test, and assumption distinct. Testing examples is not
a universal proof. A conditional proof applies only when its actual starting
conditions hold. The language's required `ensures` proofs block builds when
unknown, timed out, or disproved; runtime boundary checks do not replace them.
See [Language foundation](language-foundation.md) for contract rules.

Reconciliation and durable recovery belong to Loom libraries and systems, not a
built-in Core operator runtime or automatically durable ordinary tasks.
An eligible action needs an authoritative query sufficient to decide the next
safe step, or an idempotent operation. Retries of one logical action reuse its
action identity; resource keys describe contention, not retry identity.
Unknown outcomes cannot be blindly rerun. Issued actions retain their original
contract and interpretation across code changes; adopting new logic requires a
compatible safe boundary.

Cross-cutting/AOP composition remains undecided backlog work. This record does
not enable implicit aspect activation or implementation replacement.
