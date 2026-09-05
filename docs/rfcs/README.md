# Design records

This directory contains accepted directions and proposals that are not yet
part of the implemented Loom language reference. Acceptance fixes a target;
it does not certify current compiler support.

An accepted language feature is documented in the
[language reference](../reference/language/README.md) only after its parser,
semantic checks, diagnostics, tests, and native execution path agree. A design
record must never be used as evidence that a feature is available.

## Status model

- **Draft**: open for investigation; no compatibility promise.
- **Accepted**: approved for implementation, but not necessarily usable.
- **Implemented**: shipped with tests and normative reference documentation.
- **Deferred**: intentionally postponed without rejecting the underlying idea.
- **Rejected**: evaluated and awaiting removal after any current constraints are
  recorded in the authoritative documentation.
- **Superseded**: temporarily retained while its replacement is being completed.

## Active records

- [Language foundation](language-foundation.md) — **Accepted direction;
  replacement implementation planned**
- [Change and deployment](change-and-deployment.md) — **Accepted direction;
  implementation planned**
- [Typed code generation IR](typed-codegen-ir.md) — **Accepted;
  implementation in progress**

The first two records and the [roadmap](../../ROADMAP.md) govern the renewed
project direction. The typed-codegen record explains work in the existing
compiler; its architecture is not a requirement for the replacement.

New proposals should begin with a tracking issue and state their user problem,
scope, alternatives, observable semantics, diagnostics, migration impact, test
plan, and acceptance criteria.
