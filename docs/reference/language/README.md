# Loom language reference

> Normative for Loom language version 0.3.

This reference defines the behavior of Loom source programs. It describes the
language as observed by a program: syntax, static typing, contracts, resources,
tasks, and failures. It stops at source-observable semantics.

Loom is a statically typed, compiled language with nominal data types,
definition-site checked generics, explicit concept conformance, executable
contracts, automatic memory management, lexical resource cleanup, and
structured asynchronous tasks. It does not expose ownership, borrowing,
lifetime, pointer, or runtime type-registry syntax.

## Reading paths

For a first pass through the language, read these pages in order:

1. [Lexical structure](lexical-structure.md)
2. [Packages and declarations](packages-and-declarations.md)
3. [Types and values](types-and-values.md)
4. [Expressions and control flow](expressions-and-control-flow.md)
5. [Functions and methods](functions-and-methods.md)
6. [Records, enums, and patterns](records-enums-and-patterns.md)

For abstraction and correctness features, continue with:

- [Generics, concepts, and `dyn`](generics-concepts-and-dyn.md)
- [Constrained types and contracts](constrained-types-and-contracts.md)
- [Memory and resources](memory-and-resources.md)
- [Async functions and tasks](async-and-tasks.md)
- [Diagnostics and failures](diagnostics.md)

Library types and imported operations are cataloged in the
[standard library reference](../std/README.md).

## Reference conventions

Grammar fragments use `name Type` for parameters and fields. A colon is used
only for bounds, such as `T: Display`, and not as a general type separator.

Code examples use significant newlines. Loom has no semicolon token. A final
expression in a block is that block's value; a block without a final expression
has value `Unit`.

Short fragments may omit required imports and nearby declarations when those
details are not the rule being illustrated.

The words *must*, *must not*, and *requires* state static or runtime rules. A
program that violates a static rule is rejected before execution. Runtime
contract and runtime faults terminate ordinary program control flow and are not
implicitly converted to `Result` values.

## Language boundary

Version 0.3 has no source forms for inheritance, exceptions, `null`, operator
overloading, reflection, downcasting, a universal `any`, raw pointers,
finalizers, weak references, detached tasks, generators, or user-defined async
destructors. Names such as `view`, `box`, and `shared` are ordinary identifiers,
not interface-carrier syntax.
