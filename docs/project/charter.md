# Project charter

Loom aims to make programs easier to write, understand, verify, and evolve.
Developers keep ordinary source files and familiar editors; types, contracts,
dependencies, and declaration identities let tools reason about changes beyond
textual lines.

The compiler is the foundation of this experience, not the whole product. A
small language, a source-written standard library, and tools for change and
deployment have separate responsibilities.

## Goals

1. **Readable programs.** Keep related logic together in the organization the
   developer chooses. Declarative meaning must not require fragmented handlers,
   an AST editor, or an execution graph in every runtime call.
2. **Trustworthy guarantees.** Establish constrained values at boundaries,
   preserve invariants through permitted mutation, and prove declared function
   postconditions. Distinguish proof, runtime checks, tests, and assumptions.
3. **Useful feedback and collaboration.** Explain affected definitions, stale
   results, and changed bindings. Merge moves and edits semantically without
   guessing declaration identity or replaying external effects.
4. **Safe evolution of deployed systems.** Check the actual deployed basis
   against the candidate artifact. Require complete migration and recovery
   plans when persisted state is incompatible; retain data across downgrade
   and account for it on a later upgrade.
5. **A small implementation that can self-host early.** Produce straightforward
   typed native code, implement policy in Loom libraries, and move compiler
   components into Loom as soon as the native subset can support them. Fast
   startup and check/build/test feedback, with controlled memory growth, are
   part of the programming experience, not only compiler benchmarks.

## Responsibility boundaries

| Layer | Responsibility |
| --- | --- |
| Language and compiler | Source syntax, static types, concepts, contracts, effect reasoning, compile-time programming, and typed native compilation. |
| Minimal runtime | Irreducible allocation/GC, coroutine scheduling, wait registration, and platform boundaries. |
| Loom libraries | Collections, text processing, formats such as JSON, I/O APIs, testing helpers, task composition, and application policy. |
| Development and deployment tools | Semantic change management, affected feedback, deployment records, compatibility analysis, and migration/recovery orchestration. |

Reconciliation belongs in libraries and systems built on these mechanisms, not
in a mandatory language-level operator runtime. The project retains the goal
of continuous, readable desired-state workflows. Cross-cutting composition and
AOP remain in the backlog until their benefit and integration model are clear.

No ownership/borrow/lifetime syntax, GC finalizers, runtime conformance search
from `any`, or compulsory source database is part of this direction. Physical
addresses and runtime layouts are not source contracts. FFI and additional
backends require their own narrow boundaries, not a universal-value fallback.

## Goals are not implementation status

The accepted [language foundation](../rfcs/language-foundation.md) and
[change and deployment design](../rfcs/change-and-deployment.md) record the
target decisions. They supersede narrower project-scope statements, not the
observable behavior of the existing compiler.

The [implementation status](implementation-status.md), compiler guide,
and executable fixtures describe what currently works. Earlier prototypes are
available in Git history, not a requirement to preserve their architecture.
There is no compatibility obligation to an
unpublished prototype; actual deployed state still creates explicit obligations.

The [roadmap](../../ROADMAP.md) defines the replacement compiler's vertical
slices and self-hosting gates. Do not turn a temporary subset into a reduced
goal, maintain a second runtime interpreter as a product requirement, or label
a proposed capability as implemented.
