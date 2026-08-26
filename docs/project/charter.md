# Project charter

Loom is an experimental, statically typed compiled language for ordinary
programs. The project is building one coherent path from source text to checked
portable IR and native executables, with contracts, concepts, automatic memory
management, lexical cleanup, and structured asynchronous tasks.

The project favors a small explicit language over a collection of overlapping
mechanisms.

## Current language direction

The implemented core includes:

- source modules, records, enums, generics, expressions, methods, and tests;
- checked integer arithmetic and explicit typed failure values;
- refined values, record invariants, preconditions, postconditions, and
  proof-based check elimination;
- named concepts, explicit conformances, associated types, static
  polymorphism, and `dyn C` dynamic values;
- automatic moving GC without ownership, borrow, or lifetime syntax;
- block-level `scoped` resources and block-level `defer` cleanup;
- stackless async functions, postfix `.await`, structured `Task` ownership, and
  tuple/list task joins;
- manifest packages, lockfiles, registries, portable artifacts, a persistent
  cache, an interpreter backend, and an LLVM native backend.

The Go-like `name Type` spelling is a surface choice. It does not imply
structural interface satisfaction, runtime interface discovery, function-level
`defer` semantics, or Go's runtime representation.

## Deliberate exclusions

The current scope does not include:

- Rust-style ownership, borrow, lifetime, or move-carrier syntax;
- runtime conversion from an untyped value to a concept by searching
  conformances;
- live programming or source/AST editing as a runtime model;
- AOP, implicit advice, operator runtimes, or desired-state reconciliation;
- runtime reflection, dynamic loading, plugins, or a stable native FFI ABI;
- inheritance hierarchies as a parallel abstraction mechanism;
- finalizers as resource management;
- a multithreaded shared-memory executor.

These exclusions are architecture boundaries, not hidden placeholders.
Introducing one requires a separate design proposal that explains observable
semantics, static checking, MIR, reachability/DCE, artifacts, runtime impact,
and test evidence.

## Engineering principles

1. Static meaning is decided before execution. Backends do not guess missing
   type, contract, or conformance facts.
2. One feature has one primary spelling and one semantic model.
3. Safety checks may be removed only by a proof whose failure falls back to the
   checked path.
4. Automatic memory management must preserve value semantics and lexical
   resource cleanup.
5. Native layout and optimization are private implementation choices.
6. Platform and performance claims name the exact evidence that supports them.
7. Corrupt external inputs fail closed; caches never become authorities.
8. New capability arrives as a vertical slice: syntax, semantics, MIR,
   validation, both applicable backends, tooling, and tests.

## Documentation authority

User-facing reference documents describe implemented, supported behavior.
Internals describe the current implementation. Contributing guides describe
repository process. RFCs and archived design studies are proposals or history,
not user reference.

The repository preserves selected earlier research as explicitly archived
design records under `docs/rfcs/archived`. It does not retain superseded
numbered reference documents as an alternate specification. Archived
declarative-composition, AOP-like, and desired-state studies do not define
implemented behavior.
