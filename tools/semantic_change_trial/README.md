# Semantic change trial

This small tool previews a three-way merge of one Loom package. It reads three
snapshot directories and **does not write** any source or identity files:

```sh
target/loom run tools/semantic_change_trial -- \
  tools/semantic_change_trial/fixtures/base \
  tools/semantic_change_trial/fixtures/left \
  tools/semantic_change_trial/fixtures/right
```

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

Each directory contains ordinary root-level `.loom` files and a `.loom-ids`
sidecar. The sidecar requires exactly one `package NAME` line and exactly one
`next N` positive allocator line, followed by one
`id STABLE_ID KIND DECLARATION_NAME FILE.loom` line per declaration. `KIND` is
`fn`, `record`, `enum`, `type`, or `concept`. IDs must be explicitly carried
between revisions; names and source offsets are not durable identities.

You do not need to write the sidecar yourself. `init PACKAGE DIR` creates it
for a package of root-level `.loom` files. `refresh DIR` keeps IDs for exact
path/kind/name matches, allocates IDs for unambiguous new declarations and
reports removed IDs. When an old declaration disappears while a new one
appears, refresh refuses to guess whether it moved or was replaced. Use
`refresh DIR --move ID NEW_FILE NEW_NAME` to preserve the ID, or
`refresh DIR --delete ID` to declare a replacement, even when the replacement
uses the same file, kind, and name. Each sidecar update is
published by rename from a private stage; refresh leaves the prior sidecar
there as a recovery copy. Source files are never rewritten. `init` uses the
same lowercase package-name rule as `loom init`. These commands maintain
identity metadata for parseable source; they do not prove a successful build.
`--move` can record a rename, but this prototype's merge preview still rejects
any declaration-name change relative to its base revision.
Keep `.loom-ids-stage-*` recovery directories out of source commits.

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
  --move "$loom_amount_id" app.loom amount
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
fresh stable IDs while the other edits existing bodies. Additions on both sides,
additions mixed with deletions, names that could rebind existing calls,
concurrent layout changes, conflicting edits, changes to the non-layout side's
surrounding text, and unsupported declaration forms are reported instead of
guessed.

This is a deliberately narrow proof of the move-plus-edit workflow. It does
not yet manage Git/jj changes or commits, resolve imports or multi-package
projects, accept `impl` or other non-identity top-level forms, distinguish
overloads, or merge edits within a single declaration.
