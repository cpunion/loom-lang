# Checked semantic merges

This source library merges explicit declaration identities, not similar names
or source spans. `merge` handles import-free packages; `merge_with_context` checks
one package against pinned production/test projects. The
[CLI trial](../semantic_change_trial/README.md) supplies directory/Git snapshots,
identity maintenance and reviewed application to a new tree.

`merge_packages(base, left, right, production, tests)` accepts lists of
`PackageRevision { directory, revision }` from one module, with a nonempty baseline.
Branches may add self-contained packages or omit whole packages. New packages
must have fresh declaration IDs and the directory identity given by their module
and package name; new explicit/implicit dependencies require expanded context and
explicit resolution. Identical additions or deletions combine; competing additions
and deletion versus edit do not. `directory` is the package's path in the pinned
project, not its branch snapshot location. Every baseline must match exactly;
declaration IDs must be unique across the batch and remain in their package.

The library proposes every tree before checking the combined program. It checks
production once and tests with each owned package as a separate root. Imported
tests never become another package's private scope. Retained references must
resolve to the same identified declaration and relative location, or the same
pinned dependency location. Unselected module consumers are checked too: unchanged
text can silently select another overload after an API edit.
Match guards/bodies and destructuring use the compiler's source-reference roles:
pattern names introduce locals, while nominal paths, variants and explicit fields
require checked definition evidence. A same-typed callback bound by a pattern
cannot silently capture a renamed function.
Constrained aliases compare their pure helper calls and field owners too. They
still require a pinned project context for the implicit std Result/Error closure;
syntax coverage does not waive purity, invariants or required proofs. Concepts
with associated declarations and checked `dyn` bindings use the same reference
comparison. Anonymous impls use explicit IDs, a `-` locator label and a key for
their concept, target and generic declarations. Moves compose with method-body
edits. Member bindings normalize within the identified owner and checked member
signature rather than sibling-body byte offsets; this does not synthesize durable
member IDs or merge concurrent container-body edits. Interface/default-presence
changes and ambiguous member slots still require resolution.

Retained-package imports and external module instances stay pinned. Cross-package header/contract or deletion
changes can combine with edits only in completely disconnected import components.
The graph includes production and private-test imports, pinned consumers and
external module instances; common consumers or dependencies keep packages connected.
This is a conservative dependency separation rule, not declaration-level independence
or behavior equivalence. Changes to package scope, imports, conflicting
headers/contracts, connected deletion versus edits, unsupported declarations and references
without complete checked evidence require explicit resolution.
Explicit private named-declaration renames compose in package batches too:
the shared token/binding normalizer checks each complete branch in production
and the renamed package's private tests before the combined merge. The original
baseline must match before normalization. Overloaded declarations are selected
by stable identity and signature locator, not spelling. A rename may split or
join an overload set only if checked calls and function references keep their
identified targets; ambiguous or changed bindings still require resolution.
Generic bodies require checked abstract bindings as well as agreement with any
observed concrete instances; every retained reference still needs one checked
definition. Unselected static branches, unsupported generic forms and missing
evidence require resolution.
The compiler's public reference obligations supply the source forms and unknown
ranges; this tool does not keep a second syntax whitelist. Explicit concept
bounds and lexical cleanup blocks participate in binding comparison. This is
explicit-name coverage, not a proof of implicit effects or behavior compatibility.
Default-method bodies participate through their implicit generic `Self` scope.
The same declaration-wide checks apply to explicit generics and compile-time
parameters; missing or conflicting evidence still requires resolution.
Public function, record, enum, refined-type and concept renames also compose in a complete selected-module batch,
including checked import members, calls and each package's private tests. Every
known package in that module must participate; known consumers in other module
instances require separate resolution. Visibility and existing dependency edges
stay fixed. Public overload imports split into the names required by
their identified declarations; calls and contextual function values follow only
their own selected target. Existing import lines and trailing comments stay in
place, and additional imports retain the file's line ending. Normalized member
sets must agree in both production and effective private-test scope; unrelated
import changes still require resolution. Type renames refresh affected function
and impl locators while retaining their explicit IDs and checked member scopes.
Single-package public renames, member renames and moves across package identities
remain unsupported.
This does not update unknown external consumers or prove API compatibility.
Rename composition also rejects packages exporting a macro generator:
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
