# Governance

Loom is currently maintained by the repository owner, [Li Jie](https://github.com/cpunion).
This document records the authority model as it exists today; it does not invent
a committee or voting process.

## Responsibilities

The maintainer is responsible for:

- accepting or rejecting language and toolchain changes;
- reviewing and merging pull requests;
- defining release readiness and publishing releases;
- maintaining the security response process; and
- enforcing the [Code of Conduct](CODE_OF_CONDUCT.md).

Contributors own the accuracy of their changes, tests, migration notes, and
review responses. Review or discussion does not transfer that responsibility.

## Decision process

Every repository change, including a maintainer-authored change, is made through
a pull request. Small fixes may be decided in that pull request. A change to
observable language behavior should begin with an issue that describes the
problem, alternatives, precise semantics, diagnostics, compatibility impact,
and test plan.

The maintainer decides after considering technical evidence and contributor
feedback. Decisions favor the [project charter](docs/project/charter.md), a
small coherent language, deterministic compilation, actionable diagnostics, and
test-backed behavior. Rejected or deferred proposals may be preserved as design
records when their analysis remains useful.

## Releases and compatibility

Only the maintainer may publish an official Loom release. A release must satisfy
the repository's documented release gates and the process in the
[release guide](docs/contributing/releases.md). Versioning policy is documented
separately in [Versioning](docs/project/versioning.md).

## Changes to governance

Governance changes use the same public pull-request process. The model should be
revisited if the project gains additional sustained maintainers; roles and
authority must be documented before they are exercised.
