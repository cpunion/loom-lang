# GC runtime

Loom uses automatic memory management without ownership or borrow syntax. The
native runtime implements a precise moving collector. Movement and allocation
addresses are not observable in the language.

## Runtime ownership

`LoomRuntime` owns:

- the managed heap;
- the synchronous shadow-stack root chain;
- collector counters and threshold state;
- at most one attached async executor.

Synchronous code can use the runtime without constructing an executor, OS
poller, ready queue, or worker mailbox. A runtime can be active on only one
thread at a time; nested activation is valid only for the same runtime.
Destruction fails while the runtime is active, has roots, or still has an
attached executor.

## Collection policy

The heap begins with a 64 KiB collection threshold. Allocation charge grows
with managed allocations. After collection, the next threshold is the larger
of 64 KiB and twice the exact live footprint.

Collection occurs at compiler-known safepoints, before an allocation slow path,
or between coroutine resume calls. The production collector is
threshold-driven; tests can force a collection at every poll or before every
managed allocation.

## Precise roots

Synchronous generated frames link a shadow-stack record containing:

- a versioned immutable descriptor;
- pointers to existing universal value slots;
- one current state;
- a bitmap row selecting the slots live in that state.

The compiler publishes the state before a safepoint. Runtime helper operations
use temporary precise root scopes when they hold partially constructed values.

Async values live across suspension are stored in Task slots. Coroutine
descriptors provide the exact live bitmap for each state. Task results remain
roots only while their ownership and result state require it.

Derived list indexes and other non-owning acceleration structures are not
roots and are cleared before relocation.

## Moving collection

The collector traces live universal values, managed nodes and sequences, text
objects, Task frames, and witness instances. It builds replacement allocations,
updates every precise root and internal managed pointer, then releases dead old
storage.

Runtime clone and aggregate-building helpers use an explicit non-recursive work
stack. This avoids host stack overflow on deeply nested but valid managed
values and makes partially built graphs visible to the root protocol.

Compiler-emitted witness descriptors are static and do not move. Runtime-owned
witness instances use a separate non-moving arena because generated proof
arguments can hold their addresses across a safepoint; unreachable instances
are marked and swept.

## Source semantics

GC must not change:

- value equality or independent copy behavior;
- contract and refined-value checks;
- concept selection or dynamic dispatch;
- lexical `scoped` and `defer` cleanup ordering.

External resources such as files and sockets are not released by finalizers.
They use explicit close operations or lexical cleanup. The current core has no
source finalizer, weak-reference, stable-address, or general pinning API.

An out-of-memory condition is a process-level runtime fault rather than a
recoverable value. A future FFI boundary must copy data or define an explicit
bounded pin protocol; ordinary Loom values do not become immovable.

## Evidence

Runtime unit tests cover activation, attachment, root descriptor validation,
forced collection, relocation, nested managed values, witness mark/sweep, and
partial-construction helpers. LLVM integration tests exercise
synchronous shadow-stack maps, coroutine state liveness, structured values,
standard-library outputs, and collections at compiler-generated boundaries.

A complete Windows native job is configured with GC/runtime fixture coverage,
but it is not verified Windows runtime or release evidence until a real Windows
runner result is recorded.
