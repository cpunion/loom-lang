# Analysis with an explicit proof backend

Install Z3 on PATH, then run `target/loom run compiler/examples/analysis_solver`
or `target/loom test compiler/examples/analysis_solver` from the repository root.

This ordinary Loom program parses in-memory source and uses the public analysis
library. Default analysis performs no external solver I/O and leaves the nonlinear
square guarantee unproved. Explicitly supplying `std.loom.proof.z3.backend("z3")`
proves the nonnegative result while rejecting the false strictly-positive variant.
The guarantee concerns normal returns, not absence of arithmetic overflow faults.

The trusted solver runs in the analysis host only. It is not linked into analyzed
programs, and constructing the adapter does not start a process.
