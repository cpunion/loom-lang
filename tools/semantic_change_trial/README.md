# Semantic merge dry run

This small tool previews a three-way merge of one Loom package. It reads three
snapshot directories and **does not write** any source or identity files:

```sh
target/loom run tools/semantic_change_trial -- \
  tools/semantic_change_trial/fixtures/base \
  tools/semantic_change_trial/fixtures/left \
  tools/semantic_change_trial/fixtures/right
```

Each directory contains ordinary root-level `.loom` files and a `.loom-ids`
sidecar. The sidecar has one `package NAME` line and one
`id STABLE_ID KIND DECLARATION_NAME FILE.loom` line per declaration. `KIND` is
`fn`, `record`, `enum`, `type`, or `concept`. IDs must be explicitly carried
between revisions; names and source offsets are not durable identities.

The preview includes the proposed `.loom-ids` and every merged source file.
The merge library accepts one side changing file/declaration layout while the
other edits declaration bodies. It preserves the layout side's surrounding
source text and the chosen declaration body verbatim, then parses and checks
the merged package. Ambiguous identities, concurrent layout changes, new
declarations, conflicting edits, changes to the non-layout side's surrounding
text, and unsupported declaration forms are reported instead of guessed.

This is a deliberately narrow proof of the move-plus-edit workflow. It does
not yet integrate with Git/jj, resolve imports or multi-package projects,
merge edits within a single declaration, or apply changes to disk.
