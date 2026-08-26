# Desired-state reconciliation

Status: **Archived; non-normative**

This record preserves an earlier investigation into durable desired state and
operator-style reconciliation. Loom does not implement or reserve an
`operator`, `reconcile`, or persistent workflow runtime.

This topic is independent of the archived
[static-composition proposal](static-composition.md). Static composition asked
how a build chooses behavior; reconciliation asked how a long-running system
recovers and converges after external state changes and failures.

## Research question

For a process that spans restarts and repeated observations, could a program
separate the desired outcome from a pure typed plan, then execute explicit,
idempotent actions with durable receipts?

The candidate protocol was:

```text
desired state
  + durable observations
  + pure plan(desired, current)
  + explicit external actions
  + idempotency keys and durable receipts
  -> repeated reconciliation
```

The proposal did not imply embedding Kubernetes or a deployment control plane in
the language. Possible domains included fulfillment, billing, replication,
access grants, and infrastructure, provided that current state was observable
and actions had explicit contracts.

## Candidate protocol

Each reconciliation iteration would have:

1. loaded desired state, program basis, observations, and receipts;
2. validated new observations at a typed boundary;
3. deterministically folded a current-state model;
4. run a pure `plan(desired, current)` function;
5. persisted a pending action, its idempotency key, and its program basis;
6. executed or replayed that exact action through an explicit provider;
7. persisted a success, failure, or unknown-outcome receipt; and
8. observed and planned again until converged, blocked, or escalated.

Persisting pending work before the external action was essential for crash
recovery. It would still provide at-least-once delivery, not exactly-once
execution. Safety would depend on the provider honoring the same logical
idempotency key on replay.

An action result alone would not prove convergence. A later observation, or a
durable receipt sufficient to reconstruct current state, would still have to
satisfy the next pure plan.

## State and ownership requirements

Pending work would have fixed the canonical action payload, idempotency key,
desired revision, program and rule basis, provider contract version, managed
resource domain, attempt history, and durable outcome. A deployment could not
silently reinterpret old pending actions with new code.

Controllers would have declared their writable resource domains. Overlapping
writers would require rejection, explicit partitioning, or an explainable
coordination protocol; last-writer-wins was not considered a safe default.

The runtime would also have needed structured evidence for no progress,
oscillation, permanently unavailable contracts, stale observations, and
unresolvable version changes. Safe retries do not prove eventual convergence.

## Why the work stopped

The difficult parts were distributed-systems concerns rather than syntax:
durable storage, observation consistency, unknown outcomes, idempotency,
provider contracts, crash recovery, upgrades, deletion, and competing
controllers. The repository had no fault-injected fixture proving these
properties, and no evidence that dedicated language constructs would outperform
a typed state machine, workflow engine, or operator SDK.

Combining this work with static composition would also have made failures
impossible to attribute. The protocol therefore remains an independent research
track outside Loom's current compiler, runtime, and standard library.

## Reopening criteria

A renewed experiment should start as a library or runtime prototype with one
fully observable resource and a provider that genuinely supports idempotency
keys. It must test normal convergence, explicit failure, unknown outcomes,
duplicate delivery, delayed observations, crashes immediately before and after
receipt persistence, program upgrades with old pending work, desired-state
changes during execution, and no-progress or oscillation escalation.

Only a protocol that survives restart and fault injection—and provides a clear
maintenance advantage over an established workflow baseline—would justify a
discussion of language syntax or a dedicated scheduler.
