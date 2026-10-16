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

The package/import graph stays fixed. Cross-package header/contract or deletion
changes can combine with edits only in completely disconnected import components.
The graph includes production and private-test imports, pinned consumers and
external module instances; common consumers or dependencies keep packages connected.
This is a conservative dependency separation rule, not declaration-level independence
or behavior equivalence. Changes to package scope, imports, conflicting
headers/contracts, connected deletion versus edits, unsupported declarations and references
without complete checked evidence require explicit resolution.
Explicit private nongeneric function renames compose in package batches too:
the shared token/binding normalizer checks each complete branch in production
and the renamed package's private tests before the combined merge. The original
baseline must match before normalization. Overloaded declarations are selected
by stable identity and signature locator, not spelling. A rename may split or
join an overload set only if checked calls and function references keep their
identified targets; ambiguous or changed bindings still require resolution.
Public/generic renames and moves
across package identities still require explicit resolution.
Private rename composition also rejects packages exporting a macro generator:
generated Text/AST can bind private helpers in the definition context, outside
the checked import closure. The shared invocation-signature query identifies
these providers; ordinary functions returning Text are not treated as macros.
This is not behavior-equivalence proof, an
arbitrary semantic VCS or an execution engine. Checking does not launch the
application or replay runtime effects; ordinary compile-time checking still runs.

Each merge API also accepts a final `ProofBackend` argument. Hosts can supply
`std.loom.proof.z3.backend("z3")` to discharge supported unresolved contracts.
The capability reaches baseline, branch, rename, binding and private-test checks;
counterexamples, unknown results and solver failures still reject the proposal.
Calls without that argument stay I/O-free. Neither form enables ambient file
or target reads. This is ordinary contract checking, not migration or
behavior-equivalence proof.

Run `target/loom test tools/semantic_change` from the repository root.
