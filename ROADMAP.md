# Loom roadmap

This is the implementation route for the accepted
[project goals](docs/project/charter.md),
[language foundation](docs/rfcs/language-foundation.md), and
[change/deployment design](docs/rfcs/change-and-deployment.md). It is not a
release schedule or an assertion that these capabilities already work.
Current evidence stays in the
[implementation status](docs/project/implementation-status.md).

## Implementation approach

Start a small replacement compiler with a Rust seed and an existing Rust LLVM
binding. Keep one checked semantic model for native compilation and compile-time
evaluation. An AST, a checked typed program, and LLVM lowering are the initial
boundaries; add another representation only for a demonstrated consumer.

Compile ordinary scalar operations and calls directly. Carry GC, fault, or
scheduler context only where required by effects; do not make ordinary functions
participate in a runtime dependency executor. Library policies resolve to Loom
definitions, not public-name compiler dispatch tables.

Reuse audited LLVM/platform code when it fits. Do not port the current layer
structure or maintain an interpreter/native feature matrix as a goal. A bounded
compile-time evaluator uses the same checked rules; it is not a second public
runtime backend. Remove replaced paths instead of adding compatibility adapters.

The [native seed](compiler/README.md) now passes the N0 vertical-slice gate on
macOS: real check/build/test/run, typed data, shared lists, source file I/O,
constrained construction, and bounded required proofs. N1 is next; later
milestones below remain exit criteria, not completion claims.

## N0 — A native vertical slice

Build the smallest useful source-to-native path on macOS first:

- directory packages, explicit imports, `pub`, and isolated test roots;
- functions, scalar values, records/enums, basic generics and overloads,
  control flow, and the shared-data semantics needed by the slice;
- a small source `std`, including the text/collection and real file-I/O
  facilities needed to write a compiler;
- constrained construction and a small sound postcondition prover: unsupported
  required proofs are explicit errors, never unchecked assumptions or runtime
  postcondition fallbacks;
- ordinary `loom check`, `build`, `test`, and `run` on the same native program.

Keep one representative application with a same-directory `*_test.loom` file
and an embedded `test fn`. Show that library builds exclude both forms of test
code. Include one boundary failure and one required-proof rejection. Inspect
one scalar/record hot path for unnecessary allocation or scheduler machinery.
Use real I/O and the necessary memory/resource substrate, not a mock executor
or a host-language implementation masquerading as source `std`.

## N1 — Move the compiler into Loom

Use the N0 subset to implement compiler components in Loom, starting with source
handling, lexer/parser, and diagnostics, then binding, typing, and checked
program construction. Keep mutable drafts separate from validated program
revisions; share unchanged storage only where the promised facts remain valid.

The first self-hosting gate is concrete:

1. The Rust seed builds a native Loom-written compiler, stage 1.
2. Stage 1 builds the same compiler source, producing stage 2.
3. Stage 2 builds the source again and runs compiler and `std` package tests.
4. The stages agree on selected interfaces, diagnostics, and executable results;
   compare reproducible artifacts where their representation is controlled.

A small documented LLVM/platform bridge may remain in Rust. A Loom lexer
called by an otherwise Rust compiler is a useful intermediate step, not the
self-hosting gate. Do not wait for the entire metaprogramming, deployment, or
editor surface before making this transition. Bootstrap agreement is evidence,
not a proof of compiler correctness.

## N2 — Complete the language and source library

Extend the self-hosted path with the remaining accepted capabilities:

- concept/dynamic dispatch, associated types, and precise reachability;
- compile-time value/type/function parameters, variadics, selected-branch
  instantiation, typed macro generation, and structured reflection;
- tracked build inputs and reusable proof/effect summaries;
- broader contract reasoning and fine-grained, alias-safe constrained mutation;
- moving GC, complete lexical resource cleanup, stackless Tasks, real timer/I/O
  registration, and source-level tuple/list task composition;
- module resolution, multiversion normalization, scoped fork policies, lockfiles,
  and incremental reuse tied to actual inputs.

Each addition must work through the native CLI and its `std` tests. Required
proofs remain mandatory even while the supported prover fragment grows. Exact
overload ranking, macro spelling, solver choice, artifact encoding, and runtime
layout belong to focused implementation designs, not new feature checklists.

## N3 — Deliver semantic change and deployment workflows

Build the accepted tools on the compiler's identities, bindings, contracts,
effects, and immutable build basis. Reuse an existing version engine; do not
create a second authoritative source store.

Close the three stories in the change/deployment record: library evolution,
ordinary-source semantic changes and feedback, and deployed-state recovery.
Test move-plus-edit merging, changed overload bindings, incompatible tightening,
unknown deployment outcomes, and preservation/restoration of downgrade data.
Affected pure feedback updates must mark retained old results as stale and must
not replay external effects.
Reconciliation remains a library/system workflow with explicit effect safety,
not a replacement execution model for every function or Task.

## Delivery discipline

Use focused PRs and tests proportional to the changed boundary. Start with the
native macOS gate; expand Linux/Windows runtime and release evidence before
claiming support. Do not recreate a large dual-backend differential suite.

Measure compiler time/memory and representative native scalar, record, and
collection workloads. Investigate generated code before introducing another
optimization layer; no performance target permits weaker contracts or cleanup.

Alternate backends, a stable FFI/plugin ABI, and cross-cutting/AOP composition
are separate future work. They do not block self-hosting. The superseded
compiler remains in Git history as reference material, not in the active tree
as a compatibility target.
