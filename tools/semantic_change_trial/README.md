# Semantic change trial

This tool previews a three-way merge of one Loom package or a fixed-package
module. Preview reads snapshots and **does not write** source or identity files:

```sh
target/loom run tools/semantic_change_trial -- \
  tools/semantic_change_trial/fixtures/base \
  tools/semantic_change_trial/fixtures/left \
  tools/semantic_change_trial/fixtures/right
```

## Module snapshots

For a complete named module, use:

```sh
target/loom run tools/semantic_change_trial -- --module compiler/std BASE LEFT RIGHT
target/loom run tools/semantic_change_trial -- --apply TOKEN NEW_MODULE_DIR \
  --module compiler/std BASE LEFT RIGHT
```

Each snapshot has regular `loom.toml` and `loom.lock` files and a `.loom-ids`
sidecar in every directory package, including test-only packages. For example,
initialize `sample` at the module root and `sample.parser` in its `parser`
directory with the existing `init` command; carry those sidecars between branches.
The package set and import graph remain fixed. Manifest and lock bytes must match
in all three snapshots. Other assets use whole-file three-way byte comparison:
one-sided edits/additions/deletions and identical concurrent edits combine;
different concurrent edits, deletion versus edit and file/directory conflicts
require explicit resolution. There is no guessed textual or binary merge. Asset
hashes appear in review and output retains the selected bytes, not file modes.
`.git`, build `target`, `node_modules` and identity recovery
directories are excluded. Nested modules require a separate merge; symlink and
nonregular snapshot entries reject.

The ordinary offline project loader reads all selected package roots in one ID
space. All candidate trees are combined before checking production and each
package's independent private test scope. Unchanged consumers are checked for
binding changes too. The review token covers every source, sidecar, asset and the
pinned production/test context. Application stages the complete result in a new
sibling directory, re-reads inputs/context, then publishes by native rename.
Failed staging is retained for recovery and never replaces the requested output.
As with single-package apply, publication assumes a trusted local workspace;
native rename is neither no-clobber against racing writers nor crash-durable.

The output includes its manifest and lock, not external dependencies or ignored
build caches. Keep relative path dependencies valid at the new location and
provision locked Git snapshots before an offline build. Module Git-object input,
package/import-graph changes and batch rename composition are not implemented.

## Single-package snapshots

The same preview can read existing commits directly from Git's object database,
without checkout, worktree creation, or changes to Git state:

```sh
target/loom run tools/semantic_change_trial -- --git /path/to/git \
  /path/to/repo package/directory BASE_REV LEFT_REV RIGHT_REV
```

Each revision must contain `package/directory/.loom-ids` and its root-level
`.loom` files. Revisions are resolved to commits before reading trees and blobs.
The Git executable path is explicit; the tool never invokes a shell.
`REPO` may be the repository root or a subdirectory. Use `.` for a package at
that location; source blobs are read by their tree object IDs.

Import-bearing packages require an explicit offline project context. This must
be the exact base package in a named module with `loom.toml` and a regular
`loom.lock` (an empty lock is `loom-lock 3` followed by a newline). The tool
loads the production and test import closures without resolving or fetching
dependencies, checks both the merged and baseline packages against them, and
includes selected source bytes, module manifests, lock bytes, resolved import
edges, and canonical project/standard-library roots in the review token. Apply
reloads that context and rejects changes before creating an output directory:

```sh
target/loom run tools/semantic_change_trial -- --context \
  tools/semantic_change_trial/fixtures/project compiler/std \
  tools/semantic_change_trial/fixtures/project \
  tools/semantic_change_trial/fixtures/project_left \
  tools/semantic_change_trial/fixtures/project_right
```

Use `--apply TOKEN OUTPUT_DIR --context PACKAGE_DIR STD_ROOT BASE_DIR LEFT_DIR
RIGHT_DIR` after review. This mode requires each file's imports to remain
unchanged, including the implicit `std.result` edge introduced by refined
types. Imports are package-wide: a move can retain its bindings when the
existing package import graph stays unchanged. It can add or remove package
source files, but does not merge import-graph changes or output the dependency
closure. The Git-object mode still rejects imports rather than borrowing a live
checkout. The context and output paths must be trusted local paths. This does
not make review and publication race-free against hostile concurrent filesystem
writers. For move-plus-edit proposals, retained references in supported function
and record declarations are checked against the branch that supplied each unchanged body
and the proposed merge. Definition locations must normalize to the same stable
sidecar declaration ID and relative span, or to the same location in the pinned
dependency closure. Concrete type annotations, record initializer labels and
each member of a value path use compiler binding evidence; equal field names
do not unify distinct record owners. Missing or ambiguous checked evidence,
unsupported source forms and reference-bearing generic
declarations require explicit resolution. This preserves checked reference
bindings only in that subset; it does not prove behavior equivalence or
arbitrary semantic merges. A dependency closure that invalidates the exact
baseline is rejected even if a proposed package edit would type-check against
that changed dependency.

Each directory contains ordinary root-level `.loom` files and a `.loom-ids`
sidecar. The sidecar requires exactly one `package NAME` line and exactly one
`next N` positive allocator line, followed by one
`id STABLE_ID KIND DECLARATION_NAME FILE.loom KEY` line per declaration. `KIND` is
`fn`, `record`, `enum`, `type`, or `concept`. IDs must be explicitly carried
between revisions; names and source offsets are not durable identities.

`KEY` distinguishes same-name overloads in the same file. It hashes framed
parameter-type ASTs, compile-time parameter markers and generic declarations,
excluding comments, whitespace, parameter names, result types, contracts and
bodies. Nullary nongeneric functions and non-functions use `-`. This is a
conservative source locator, not semantic type equivalence or stable identity;
alias/type-parameter spelling changes can require an explicit mapping. The
parser normalizes grouping and optional trailing commas; declaration order never selects an
overload. Missing or stale keys reject, without a name-only fallback. This
prototype format replaces the earlier five-field identity lines.

You do not need to write the sidecar yourself. `init PACKAGE DIR` creates it
for a package of root-level `.loom` files. `refresh DIR` keeps IDs for exact
path/kind/name/key matches, allocates IDs for unambiguous new declarations and
reports removed IDs. When an old declaration disappears while a new one
appears, refresh refuses to guess whether it moved or was replaced. Use
`refresh DIR --move ID NEW_FILE NEW_NAME KEY` to preserve the ID, or
`refresh DIR --delete ID` to declare a replacement, even when the replacement
uses the same file, kind, and name. Each sidecar update is
published by rename from a private stage; refresh leaves the prior sidecar
there as a recovery copy. Source files are never rewritten. `init` accepts
dotted lowercase package names outside `std`. These commands maintain
identity metadata for parseable source; they do not prove a successful build.
`--move` can also record a private, nongeneric function rename. Merge composes
that explicit identity change with an independent move/body edit by normalizing
declaration and checked reference tokens, then comparing bindings in production
and test views. Comments and unrelated strings are retained. A local shadow
that captures a renamed call rejects even if it still type-checks. Conflicting
renames, overload/name collisions, public API renames, generic targets and
reference forms without complete evidence still need explicit resolution.
Keep `.loom-ids-stage-*` recovery directories out of source commits.
`scan DIR` lists current kind/name/file/key locators without writing metadata;
use its exact key to resolve a move or parameter-type change.

For example, this creates all three sidecars from ordinary source snapshots,
then merges a move on one side with a body edit on the other:

```sh
loom_trial_dir="$(realpath "$(mktemp -d)")"
mkdir -p "$loom_trial_dir/base" "$loom_trial_dir/left" "$loom_trial_dir/right"
cp tools/semantic_change_trial/fixtures/base/*.loom "$loom_trial_dir/base/"
target/loom run tools/semantic_change_trial -- init sample "$loom_trial_dir/base"
cp "$loom_trial_dir/base/.loom-ids" "$loom_trial_dir/left/.loom-ids"
cp "$loom_trial_dir/base/.loom-ids" "$loom_trial_dir/right/.loom-ids"
cp tools/semantic_change_trial/fixtures/left/*.loom "$loom_trial_dir/left/"
cp tools/semantic_change_trial/fixtures/right/*.loom "$loom_trial_dir/right/"
loom_amount_id="$(awk '$1 == "id" && $4 == "amount" { print $2 }' "$loom_trial_dir/base/.loom-ids")"
target/loom run tools/semantic_change_trial -- refresh "$loom_trial_dir/left" \
  --move "$loom_amount_id" app.loom amount -
target/loom run tools/semantic_change_trial -- refresh "$loom_trial_dir/right"
target/loom run tools/semantic_change_trial -- \
  "$loom_trial_dir/base" "$loom_trial_dir/left" "$loom_trial_dir/right" \
  > "$loom_trial_dir/preview.txt"
loom_review_token="$(sed -n 's/^review token: //p' "$loom_trial_dir/preview.txt")"
target/loom run tools/semantic_change_trial -- --apply "$loom_review_token" \
  "$loom_trial_dir/merged" "$loom_trial_dir/base" \
  "$loom_trial_dir/left" "$loom_trial_dir/right"
target/loom check "$loom_trial_dir/merged"
```

The preview includes the proposed `.loom-ids` and every merged source file.
It also prints a SHA-256 review token. After reviewing the result, apply that
exact proposal to a **new** directory (use the token printed by your preview):

```sh
target/loom run tools/semantic_change_trial -- --apply TOKEN OUTPUT_DIR \
  tools/semantic_change_trial/fixtures/base \
  tools/semantic_change_trial/fixtures/left \
  tools/semantic_change_trial/fixtures/right
```

For Git snapshots, use `--apply TOKEN OUTPUT_DIR --git GIT_EXE REPO
PACKAGE_DIR BASE_REV LEFT_REV RIGHT_REV`. Apply re-reads all three inputs and
rejects a changed file, sidecar, or resolved Git commit. It analyzes both the
production and test views, rejects directory-input and output-path symlinks
and path traversal, and exclusively creates `OUTPUT_DIR`; it never checks out, stages,
replaces, or commits an existing tree. The generated `.loom-ids` is written
last, so a failed partial write does not leave a complete package. On macOS,
use a real path such as `/private/tmp/output` rather than the `/tmp` symlink.

The current filesystem API cannot guarantee race-free no-follow writes against
another process modifying path ancestors between checks. Apply is therefore
for a trusted, locally controlled workspace, not a hostile shared directory.
The review-token format is prototype-only and has no compatibility promise.
The same trusted-local caveat applies to `init` and `refresh`: native rename
atomically publishes the staged sidecar but may replace a concurrently created
target. A failed update leaves its private stage for inspection or recovery.

The merge library accepts one side changing file/declaration layout while the
other edits declaration bodies. It preserves the layout side's surrounding
source text and the chosen declaration body verbatim, then parses and checks
the merged package. One side may also add uniquely named declarations with
fresh stable IDs while the other edits existing bodies. For a one-sided
addition, it scans the other side's retained declarations for the new name as
an identifier token, including in types, contracts, and `comptime` code.
Comments and string contents do not count. This conservative collision check
can reject harmless local names or field names; it is not a proof of unchanged
binding. Additions on both sides,
additions mixed with deletions, cross-side deletion plus body edits,
cross-side signature or contract changes plus body edits, retained identifier
collisions, concurrent layout changes, conflicting edits, changes
to the non-layout side's surrounding text, and unsupported declaration forms
are reported instead of guessed.

This is a deliberately narrow proof of the move-plus-edit workflow. It does
not yet manage Git/jj changes or commits, reconcile import changes or
multi-package source edits, accept `impl` or other non-identity top-level forms,
or merge edits within a single declaration. Adding a new overload can change
lookup, so cross-side addition of an existing name still requires review.

The [overload snapshots](fixtures/overloads/base/app.loom) move only the `Int`
version of `amount` while the other branch edits its body. The `Bool` version
keeps its own identity and both calls retain their checked targets:

```sh
target/loom run tools/semantic_change_trial -- \
  tools/semantic_change_trial/fixtures/overloads/base \
  tools/semantic_change_trial/fixtures/overloads/left \
  tools/semantic_change_trial/fixtures/overloads/right
```

Apply the printed token to a new directory using the same three inputs, then
`target/loom run OUTPUT_DIR`. Its assertions check the combined result, `43`.
