# Z3 proof backend

`backend(tool)` constructs an explicit trusted host capability for
`std.loom.checking.with_proof_backend` and public compiler analysis. `tool` is an
executable path or a name on PATH; arguments and query input never pass through
a shell. Construction performs no I/O. Only unresolved supported obligations
invoke the solver, with the compiler's two-second timeout and exact `unsat` rule.
Errors, malformed output, `unknown` and timeout remain unproved.
Hosts with an existing context or cache can use `solve(tool, query)` directly;
the CLI does this without constructing and unpacking an intermediate adapter.

The adapter has no shared mutable cache. Compiler CLI caching remains private to
one build context; ordinary library analysis stays I/O-free unless its host
supplies a backend. Applications that do not import this adapter acquire no Z3
dependency. See the [source tool example](../../../../examples/analysis_solver).
