# Checked semantic merges

This source library merges explicit declaration identities, not similar names
or source spans. `merge` handles import-free packages; `merge_with_context` checks
one package against pinned production/test projects. The
[CLI trial](../semantic_change_trial/README.md) supplies directory/Git snapshots,
identity maintenance and reviewed application to a new tree.

`merge_packages(base, left, right, production, tests)` accepts lists of
`PackageRevision { directory, revision }`. Each branch selects the same nonempty
set of packages from one module. `directory` is the package's path in the pinned
project, not its branch snapshot location. Every baseline must match exactly;
declaration IDs must be unique across the batch and remain in their package.

The library proposes every tree before checking the combined program. It checks
production once and tests with each owned package as a separate root. Imported
tests never become another package's private scope. Retained references must
resolve to the same identified declaration and relative location, or the same
pinned dependency location. Unselected module consumers are checked too: unchanged
text can silently select another overload after an API edit.

The package/import graph stays fixed. Changes to package scope, imports, conflicting
headers/contracts, deletion versus edits, unsupported declarations and references
without complete checked evidence require explicit resolution. Batch rename
composition is not implemented; the single-package entry points retain their
private nongeneric rename support. This is not behavior-equivalence proof, an
arbitrary semantic VCS or an execution engine. Checking never runs application
bodies or replays their effects.

Run `target/loom test tools/semantic_change` from the repository root.
