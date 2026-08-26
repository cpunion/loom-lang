# Checkout composition study

Status: **Archived; non-normative**

This document describes a paused study design. Its proposed flow, slot,
contribution, capability, target, example, and scenario constructs are
pseudocode, not Loom syntax. The associated
[fixture contract](checkout-composition-fixture.md) is archived for the same
reason.

## Hypothesis

The study asked whether owner-controlled, typed contributions could reduce the
number of semantic edit points in a checkout system without reducing
correctness, explainability, or developer understanding.

Checkout was selected because it combines constrained domain values, pricing
order, authorization, risk checks, auditing, idempotency, and several observable
failure paths without requiring a distributed runtime.

## Fair baseline

The intended control was an idiomatic, version-pinned TypeScript application
using discriminated unions, modules, pure functions, typed pipelines or strategy
objects, dependency injection, a composition root, mature tests, and standard
editor support. An independent TypeScript reviewer would have been required to
reject a weak or artificial baseline.

Both implementations would expose equivalent typed extension points for
pricing, pre-authorization checks, and authorization-rejection handling. Editing
the composition root would count as a normal and permitted change. A separate
boundary task would require modifying the owner when no extension point existed;
the candidate would fail if it could inject behavior around an undeclared site.

## Candidate requirements

The experimental implementation would have needed to prove all of these
properties before any productivity comparison:

- domain invariants have one authoritative construction boundary;
- contributions target only owner-declared, typed slots;
- a build target explicitly selects every active contribution;
- imports and dependencies never activate behavior;
- active transforms form one explicit order independent of file layout;
- closed error and capability bounds cannot be widened by a contribution;
- duplicate keys, missing targets, missing bindings, unknown anchors, ambiguous
  order, and cycles fail with deterministic witnesses; and
- explanation output is derived from the same typed plan that executes.

The candidate could not use a hidden registry, name scanning, call interception,
or a hand-written central switch as a substitute for composition semantics.

## Study tasks

The protocol proposed one warm-up and four substantive tasks:

1. Add an EU VAT invariant at the single domain-construction boundary.
2. Add a VIP pricing transform after product promotions.
3. Add a high-value risk check before authorization and audit every business
   rejection without auditing validation or provider failures.
4. Diagnose and repair a duplicate-key or ordering-cycle failure.

A comprehension exercise would ask participants to identify all price sources,
explain why risk precedes authorization, distinguish audited and unaudited
failures, and predict the static impact of removing a contribution.

Two equivalent task variants and a counterbalanced crossover were intended to
reduce learning-order bias. Both implementations would have had to pass one
language-independent result, trace, ordering, and mutation oracle.

## Measurements and gates

Primary measures were correctness-adjusted completion time, task success, and
comprehension. Secondary measures included semantic edit points, files touched,
non-formatting diff size, duplicated constraints or ordering facts, diagnostic
latency, escaped mutations, and subjective workload.

Compiler soundness properties were absolute gates, not metrics that faster task
completion could offset. The study also required equal editor support before
measuring developer speed.

The protocol would have advanced only after a successful mechanism gate, a
small non-author pilot, and a preregistered crossover study. It would have
stopped if the candidate required invisible matching, could bypass owner
contracts, produced an execution trace inconsistent with its plan, depended on
file or link order, or merely renamed ordinary middleware without measurable
benefit.

## Why it remains archived

The experiment was designed before the current compiler core existed and would
have required a large speculative feature set. No executable fixture, reviewed
baseline, participant study, or result was produced. Current work focuses on a
conventional statically typed language, compiler, LLVM backend, runtime, and
toolchain. This study is retained only as a reproducible question for possible
future research.
