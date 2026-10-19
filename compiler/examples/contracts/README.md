# Reusable contract predicates

From the repository root:

```sh
target/loom check compiler/examples/contracts
target/loom test compiler/examples/contracts
target/loom run compiler/examples/contracts
```

`good` proves its postcondition by checking the body of `admitted`, including
that helper's own requirement. Remove `requires positive(value)` from `good`:
the compiler must reject it, even if every current caller passes a positive
constant. `bounded` reuses a predicate with multiple parameters, branch conditions,
and early returns. `lower` proves its result from both branches of `minimum`,
without requiring a separate contract on the helper.

`expressions.loom` uses `match`, `if/else`, nested blocks and local `let` bindings
directly in contracts and type predicates. Their guards and scoped bindings use
the same proof expansion as pure helpers. A proved explicit conversion removes
its repeated boundary check, not the distinct nominal type. Nonlocal returns,
caller-state assignments, cleanup and suspension are not clause expressions.

`locals.loom` uses ordinary `var` bindings in pure helpers: a minimum selection,
eager argument snapshots, conditional operand writes and short-circuit writes.
The same rules prove `local_widen` without a second construction check. These
are proof-time substitutions, not runtime storage or implicit copies.

`established_facts.loom` retains a supported positive entry fact even alongside
an unmodeled quotient. The quotient and its runtime faults are not removed.
Only established conjunction terms can be retained independently; a disjunction
does not establish either operand. Required goals still need truth and safety.
The same rule proves type weakening and target helper requirements without
requiring a model of every stronger-source constraint.

`forward` proves its own result contract from `good`'s verified contract. The
ordinary call still executes at runtime; proof analysis does not replace it.
Direct scalar calls without a postcondition can instead use the bounded pure
helper body. Argument expressions execute once, in source order, before the
callee's result is considered.

`requires` remains a runtime entry check. Change `good(7)` in `main` to `good(0)`
to see its failure. `ensures` must be proved during checking; helpers used only
for the proof, such as `admitted`, do not become runtime call targets.

This proof fragment supports direct scalar calls and synchronous `dyn` concept
calls with a declared scalar postcondition, not general function-body
verification. A `dyn` proof uses that declaration, never the concrete witness.
Calls without a usable result contract need bounded, nonrecursive scalar helper
bodies with local bindings/reassignment, `if/else` and early returns from body or
branch blocks. Reassignment cannot change earlier reads; expression blocks and
short-circuit operands merge only executed writes. Shared-storage mutation,
returns inside operands, cleanup and indirect calls remain unsupported
for helper expansion. Bounded unit-step scans use the same guarded rules;
broader loop shapes remain unproved. Branches retain their evaluation guards;
eager arguments, overwritten/discarded arithmetic and assertions still create proof
obligations. Expansion and conditional normalization have finite budgets.
Arithmetic that executed in a body may rely on its runtime overflow check having
passed. Arithmetic written only in `ensures` must still be proved safe.
Unsupported required proofs fail the build.
