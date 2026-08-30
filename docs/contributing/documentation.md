# Documentation

Documentation is part of the implementation. A behavior change is incomplete
until the correct audience can discover its syntax, guarantees, limitations,
and version boundary.

## Audience and location

Write each fact once at the right layer:

| Location | Audience and content |
| --- | --- |
| root README | first visit, installation, smallest verified example, support summary, links |
| `docs/getting-started` | task-oriented first project and common workflow |
| `docs/reference` | implemented user-facing syntax, semantics, standard library, and toolchain behavior |
| `docs/internals` | current compiler/runtime architecture and private representations |
| `docs/contributing` | repository setup, tests, benchmarks, fuzzing, releases, and writing policy |
| `docs/project` | charter, status, quality, terminology, and versioning |
| `docs/rfcs` | active proposals that may change the current design |

Do not copy an internal layout into the language reference. Do not put an
unimplemented roadmap item into a command or standard-library reference.

## Language and style

- Write repository documentation in English.
- Lead with observable behavior, then constraints and examples.
- Use short paragraphs, descriptive headings, and tables only when they improve
  comparison.
- Use Loom's canonical spelling: `value Type`, `concept`, `dyn C`, `Text`,
  `Unit`, `Task[T]`, postfix `.await`, `scoped`, and `defer`.
- Distinguish “must” (required behavior), “may” (permitted variation), and
  “currently” (implementation fact).
- Qualify platform support as CI-tested compiler layers, native runtime, cross
  target, or release archive.
- Prefer links to one authoritative page over duplicated version tables.

Avoid marketing claims, vague “production ready” language, and comparisons
unsupported by reproducible evidence.

## Implemented, experimental, and proposed

User reference may state a feature is implemented only when source and tests
exercise the described behavior. Include current limits beside the feature,
not in a distant disclaimer.

Internals can document an optimization or private ABI, but must label it as an
implementation choice and avoid implying source code can observe it.

Unimplemented designs belong in an RFC. An RFC should have an explicit status,
motivation, semantics, rejected alternatives, compiler/runtime/artifact impact,
and test plan. Once a pre-release proposal is abandoned, remove it after moving
any still-relevant constraint into the current reference or an active RFC. The
repository is not a compatibility archive for unpublished designs.

In particular, do not merge these topics into current reference pages without
a separately accepted and implemented change:

- live or AST-editing execution;
- AOP/advice or declarative composition experiments;
- desired-state/operator runtimes;
- ownership/borrow/lifetime syntax;
- runtime conformance discovery from an untyped `any` value;
- stable native FFI/plugin layouts;
- future multithreaded or distributed execution.

## Examples

Every code block should either:

- compile in a checked fixture;
- be a deliberately incomplete synopsis labelled as such; or
- be an invalid example followed by the expected diagnostic behavior.

Prefer adapting an existing tested fixture over inventing syntax from memory.
Tool commands should match `loom --help` and specify the backend where the
artifact kind depends on it.

## Status and compatibility

When a page mentions a format or ABI, state whether it is:

- source language;
- stable user-facing file format;
- versioned but compiler-private artifact;
- cache-only internal data;
- compiler-private native ABI.

Update [Versioning](../project/versioning.md) and
[Implementation status](../project/implementation-status.md) when those tables
change. Do not represent a future workflow matrix as current CI.

## Review checklist

- Is the page in the correct audience section?
- Does every “implemented” claim have a repository test or workflow?
- Do CLI options and defaults match the parser?
- Do manifest examples match the deserialized schema and validation rules?
- Are backend and platform limits explicit?
- Are future ideas separated from reference behavior?
- Are relative links valid?
- Are code blocks tagged with the right language?
- Is terminology consistent with the project glossary?
- Does the change remove stale duplication rather than creating another source
  of truth?

For a broad rewrite, search for non-English text, stale version references, and
superseded pre-release documents. Preserve only facts that still describe the
current implementation or an active proposal.
