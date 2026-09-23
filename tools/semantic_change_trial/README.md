# Semantic merge preview and apply

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
sidecar. The sidecar has one `package NAME` line and one
`id STABLE_ID KIND DECLARATION_NAME FILE.loom` line per declaration. `KIND` is
`fn`, `record`, `enum`, `type`, or `concept`. IDs must be explicitly carried
between revisions; names and source offsets are not durable identities.

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

The merge library accepts one side changing file/declaration layout while the
other edits declaration bodies. It preserves the layout side's surrounding
source text and the chosen declaration body verbatim, then parses and checks
the merged package. Ambiguous identities, concurrent layout changes, new
declarations, conflicting edits, changes to the non-layout side's surrounding
text, and unsupported declaration forms are reported instead of guessed.

This is a deliberately narrow proof of the move-plus-edit workflow. It does
not yet manage Git/jj changes or commits, resolve imports or multi-package
projects, or merge edits within a single declaration.
