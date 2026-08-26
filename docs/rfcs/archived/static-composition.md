# Owner-controlled static composition

Status: **Archived; non-normative**

This record preserves an earlier investigation into declarative, AOP-like
composition. Loom does not reserve or implement the proposed `flow`, `slot`,
`contribution`, `transform`, `allows`, or `uses` constructs.

## Research question

Could a declaration owner expose a small number of typed extension points so
that other modules could contribute named behavior, while the compiler produced
one deterministic and explainable composition plan?

The proposal was deliberately narrower than traditional aspect-oriented
programming:

- an owner would have to declare every extension point;
- a contribution would name exactly one qualified target;
- imports and dependencies would never activate behavior;
- ordering would come from typed data dependencies or explicit edges;
- the build would reject missing targets, duplicate keys, ambiguous order, and
  cycles;
- runtime scanning, name-pattern matching, call-stack interception, and monkey
  patching would remain forbidden.

The intended description was therefore *owner-controlled static composition*,
not general-purpose AOP.

## Candidate model

A slot contract would have specified its owner, input and output types, closed
failure type, composition algebra, allowed external capabilities, and diagnostic
rules. Each contribution would have carried a qualified identity, source
location, exact target, typed transform, dependencies, and explicit ordering
edges.

The most developed candidate was an ordered pipeline:

```text
empty pipeline      = Ok(input)
one transform       = S -> Result[S, E]
several transforms  = one explicit total order; first Err stops the pipeline
```

Transform keys would be unique within a slot. An ordering edge would affect a
target only when both endpoints were active, but every anchor would still have
to resolve to a declared transform. File order, directory traversal, import
order, and registration timing would have had no semantic effect.

Other possible algebras—keyed members and provably commutative rule folds—were
only sketches. Field extension, open methods, implicit provider selection,
user-defined composition algebras, and pattern dispatch were explicitly outside
the experiment.

## Activation and explanation

A build target, rather than an import, would have selected each contribution.
Every active behavior would therefore have had a traceable path:

```text
target -> contribution -> owner-declared slot
```

The executable plan and the explanation output were intended to share one typed
representation. Tools would have had to report the source of every active step,
why it applied, why it occupied its position, which target activated it, and
which declarations would be affected by its removal.

## Why the work stopped

The experiment coupled too many unproven mechanisms: new surface syntax, typed
outcome lanes, composition algebras, build-target activation, capability
closure, provider binding, and an explanation schema. Ordinary functions,
concepts, explicit `dyn` values, typed lists, middleware, and composition roots
already form a strong baseline. No evidence showed that a language mechanism
would improve correctness or maintenance enough to justify the additional
semantic surface.

The design also faced an unavoidable API tension: exposing many slots would
inflate owner contracts, while exposing few slots would fail to localize
unanticipated changes.

## Questions that remain open

Any renewed proposal would need to answer at least the following:

1. Which real cross-owner change cannot be handled cleanly by explicit typed
   composition?
2. Is a slot public API, and how are its type, error set, and removal versioned?
3. Who controls activation: source, package manifest, or build target?
4. How do optional contributions retain stable ordering anchors?
5. Can contributions add dependencies or effects without making owner contracts
   open-ended?
6. Which explanation data is stable enough for tools and review?
7. Which parts belong in language semantics rather than a library or build tool?

## Reopening criteria

A future experiment should contain one owner, one ordered slot, and two
independent contributions. It must compare against an idiomatic explicit
composition root and cover empty, A-only, B-only, and A-plus-B configurations.
It must also provide stable failures for duplicate keys, unknown anchors,
ambiguous order, cycles, and missing targets, plus an owner-without-a-slot test.

The work should proceed only if that minimal experiment demonstrates a clear,
measurable advantage without adding a provider runtime, effect system, package
bundle mechanism, dynamic plugin search, live programming, AST editing, or an
operator runtime.
