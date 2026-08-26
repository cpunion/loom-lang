# Security Policy

Loom is an experimental compiler and runtime. Security reports are welcome,
especially where untrusted source, package metadata, artifacts, registry data,
or runtime input crosses a validation boundary.

## Supported versions

The project does not currently maintain a long-term support branch.

| Version | Security support |
| --- | --- |
| Latest `main` | Actively investigated |
| Latest tagged experimental release | Best effort until superseded |
| Older commits, artifacts, and releases | Not supported |

Compiler releases, the manifest language version, cached data, portable
artifacts, and internal runtime ABIs are versioned independently. A current
tool may intentionally reject an older artifact or runtime bundle.

## Report a vulnerability privately

Email security reports to [cpunion@gmail.com](mailto:cpunion@gmail.com). Do not
open a public issue, discussion, or pull request until the report has been
assessed and disclosure has been coordinated.

Include only the information needed to reproduce and understand the issue:

- the affected commit or release;
- host operating system and architecture;
- relevant Rust, LLVM, and linker versions;
- a minimal source file, manifest, artifact, or request when safe to share;
- observed impact and the expected trust boundary;
- any known workaround.

Do not send real registry tokens, private keys, production data, or other live
credentials. Replace them with synthetic values. If a reproducer contains
sensitive material, describe it first and arrange a safer transfer method.

The maintainers will coordinate acknowledgement, investigation, remediation,
and disclosure according to the report's risk and complexity. The project does
not promise a fixed response or remediation SLA.

## Relevant security boundaries

Examples of in-scope reports include:

- compiler or runtime memory-safety defects reachable from untrusted Loom
  source or checked artifacts;
- bypasses in checked-MIR or artifact validation;
- registry credential disclosure, insecure credential transport, or secret
  reflection in diagnostics;
- package-bundle, lockfile, cache, or runtime-bundle integrity bypasses;
- path traversal, symlink, archive, or output-path vulnerabilities;
- arbitrary code execution in a compiler process beyond the documented act of
  linking or running a user program;
- cross-package visibility or dependency-isolation bypasses;
- denial of service that defeats documented parser, artifact, or registry
  resource bounds.

Loom is **not a sandbox**. Compiling and running a Loom program intentionally
executes that program with the user's operating-system permissions. A program's
ordinary ability to read files, open sockets, consume CPU or memory, or invoke
documented host facilities is not by itself a vulnerability.

Ordinary compiler crashes on trusted input, performance regressions, language
design disagreements, and unsupported-platform failures should normally be
reported as regular bugs unless they cross a security boundary or provide a
reliable denial-of-service vector against untrusted input.

## Disclosure and fixes

Please allow the maintainers to validate the report and prepare tests, fixes,
and release guidance before public disclosure. A security fix should include a
deterministic regression test whenever publishing that test does not expose
users before a remedy is available.

The project may publish an advisory describing affected versions, impact,
mitigations, and credit. Reporters may request attribution or anonymity.
