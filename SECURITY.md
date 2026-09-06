# Security policy

Loom is an experimental compiler. Reports against the current development
revision are welcome; there is no supported release or compatibility branch.

Report vulnerabilities privately to [cpunion@gmail.com](mailto:cpunion@gmail.com).
Include the commit, host, toolchain, minimal reproducer, and affected trust
boundary. Do not include live credentials or private data. Coordinate public
disclosure with the maintainers; no fixed response time is promised.

Relevant boundaries include compiler memory safety, required-proof soundness,
package visibility, source/output path handling, and unintended execution in
the compiler process. Unsupported features are not security guarantees.

Loom is not a sandbox. Building invokes the host toolchain; running a program
executes it with the user's operating-system permissions. Ordinary bugs and
unsupported-platform failures belong in the public issue tracker unless they
cross a security boundary.
