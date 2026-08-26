# Design records

This directory contains design proposals that are not part of the Loom language
reference.

An accepted language feature is documented in the
[language reference](../reference/language/README.md) only after its parser,
semantic checks, diagnostics, tests, and native execution path agree. A design
record must never be used as evidence that a feature is available.

## Status model

- **Draft**: open for investigation; no compatibility promise.
- **Accepted**: approved for implementation, but not necessarily usable.
- **Implemented**: shipped with tests and normative reference documentation.
- **Deferred**: intentionally postponed without rejecting the underlying idea.
- **Rejected**: evaluated and not planned in its proposed form.
- **Superseded**: replaced by another record.
- **Archived**: retained as research history and outside the active roadmap.

## Active records

- [Typed code generation IR](typed-codegen-ir.md) — **Accepted;
  implementation in progress**

## Archived records

Earlier investigations that are not on the active roadmap are preserved as
[archived research](archived/README.md).

New proposals should begin with a tracking issue and state their user problem,
scope, alternatives, observable semantics, diagnostics, migration impact, test
plan, and acceptance criteria.
