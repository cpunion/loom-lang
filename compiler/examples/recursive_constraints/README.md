# Recursive constrained data

Run `target/loom run compiler/examples/recursive_constraints` or
`target/loom test compiler/examples/recursive_constraints` from the repository root.

Nonempty child Lists keep sharing through recursive enum values, forwarded
getters, replacement, append and a real asynchronous timer. The recursive
zero-initialized fold proves its positive normal-return result using callee
guarantees and loop induction, not a runtime postcondition check.

The tests include concurrent workers reading and growing the same constrained
children. Only the positive result is promised during interference, not an
unchanged length or an entry snapshot of mutable contents.

The tests also construct a typed cycle without traversing it. The invariant
does not imply acyclicity or termination; calling `size` on that cycle is not
promised to terminate. Raw or differently constrained backreferences do not
inherit the protected List's guarantee.
