# Relational function contracts

```sh
target/loom check compiler/examples/relational_contracts
target/loom test compiler/examples/relational_contracts
target/loom run compiler/examples/relational_contracts
```

`related` derives a positive return value from two parameter bounds.
`from_record` applies the same reasoning to inline fields; `through_call`
uses a verified callee's lower-bound promise. `snapshot` keeps the saved value's
identity after local reassignment. `advance` proves that arithmetic written only
in a postcondition cannot overflow.

The shared bounded integer prover runs at compile time; it does not emit a
postcondition check or replace the native body. Actual entry checks and body
arithmetic still execute. These commands deliberately fail:

```sh
target/loom run compiler/examples/relational_contracts -- invalid-entry
target/loom run compiler/examples/relational_contracts -- body overflow
```

This is interval propagation over integer difference constraints, not a general
relational solver. At each boundary it scans at most 256 established facts and
uses at most 32 numeric identities. Unsupported or exhausted required proofs
reject compilation; optional refinement proofs retain their runtime checks.
Shared contents and required Float algebra remain unsupported.
