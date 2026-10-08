# Inferred scalar loop contracts

```sh
target/loom check compiler/examples/loop_contracts
target/loom build compiler/examples/loop_contracts
target/loom test compiler/examples/loop_contracts
target/loom run compiler/examples/loop_contracts
```

The compiler proves the declared normal-return contracts by induction, not by
testing a finite number of iterations. It proposes scalar entry/range bounds and
weakened continuation bounds, then retains only predicates established on entry
and preserved by every symbolic backedge. Removing one candidate triggers a
recheck of the others. Zero-iteration exits are checked separately.

Written Int cursors also propose entry-difference relations with other written
Int locals, within the same candidate budget. The
[difference example](differences.loom) proves separately updated counters,
symbolic offsets, helper/cleanup updates and minimum sizes of structural packs.
These are mathematical candidates, not assumptions that two updates match;
conditional writes, early returns and every `continue` remain checked. Source
contract arithmetic still requires its own definedness proof.

Constant affine assignments also suggest weighted pair relations. The
[affine example](affine.loom) proves different-rate and opposite-direction
counters, including lexical cleanup and compile-time calls. These suggestions
reuse the same mathematical arithmetic and induction checks, not a second
executor or trusted update-pattern rule. General multi-local affine synthesis
and nonlinear invariants remain outside this candidate generator.

The current subset includes Int/Bool local assignments, branches, early returns
and nested loops. `break` preserves the state at its exit; `continue` must
preserve the invariant at its backedge. Both target the nearest loop, including
during each inference recheck. Scalar lexical cleanup executes in its checked
order before the jump; return values retain their pre-cleanup snapshots.
Direct body and guard calls reuse verified summaries or finite pure expansion.
Guard results are reevaluated at each tested head, not assumed stable by spelling.
Pure expansion can propose bounds for guards such as `below(value, limit)`;
those proposals pass the same entry/backedge checks as direct comparisons.
Unmodeled effects and resource cleanup reject in a required proof;
List extent/content proofs have their own supported fragment. Unknown proofs remain build errors;
there are no generated loop-invariant checks, termination promises or proofs
that arithmetic cannot fault. Ordinary loops without `ensures` are not restricted
by this proof subset.
