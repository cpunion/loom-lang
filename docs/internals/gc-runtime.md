# GC runtime

Loom uses automatic memory management without ownership or borrow syntax. The
native runtime implements a precise moving collector. Movement and allocation
addresses are not observable in the language.

## Runtime ownership

`LoomRuntime` owns:

- the managed heap;
- independent universal-value and typed-pointer shadow-stack root chains;
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

Legacy synchronous generated frames link a shadow-stack record containing:

- a versioned immutable descriptor;
- pointers to existing universal value slots;
- one current state;
- a bitmap row selecting the slots live in that state.

The compiler publishes the state before a safepoint. Runtime helper operations
use temporary precise root scopes when they hold partially constructed values.

Typed synchronous frames use the same state/bitmap model on a separate chain.
Each live entry points to writable pointer-sized storage containing a direct
managed reference. It is never interpreted as a universal `ValueSlot`. A cell
may contain only null, the exact base of a typed managed allocation, or a
compiler-proven process-lifetime static/immortal pointer. Legacy moving object
pointers, interior pointers, and other unregistered finite-lifetime pointers
are forbidden. A legal static pointer is left unchanged without adding a
runtime tag or registration table.

The slot cells, slot-pointer array, descriptor, and frame have stable addresses
for the entire linked interval. In particular, slot cells cannot live inside
either moving heap. Generated code may update cell contents and the published
state, but collection must never invalidate the storage that the frame names.

Both descriptor forms are validated before a collection can increment its
counter, trace, sweep, or move any allocation. One descriptor is limited to
65,536 slots, 65,536 states, and 1,048,576 bitmap words in total across all
states. Each independent chain is limited to 65,536 linked frames. The limits
are shared ABI constants so compiler rejection and hostile-runtime-input
validation agree. The legacy LLVM emitter validates this pure root-map shape
before allocating a bitmap or emitting a descriptor.

Async values live across suspension are stored in Task slots. Coroutine
descriptors provide the exact live bitmap for each state. Task results remain
roots only while their ownership and result state require it.

Derived list indexes and other non-owning acceleration structures are not
roots and are cleared before relocation.

## Typed moving-object ABI

The typed heap is a side-by-side extension of the existing universal heap. A
typed allocation has no universal `Value` envelope and no required in-object
tag. `LoomGcObjectDescriptor` supplies a fixed pointer-bearing prefix size,
object alignment, and a strictly increasing list of exact pointer-cell byte
offsets. An allocation may append pointer-free trailing bytes. The allocator
copies the validated offsets into its private side table, zero-initializes the
complete allocation, and does not retain caller descriptor or offset pointers.
Every non-null fixed pointer cell follows the same exact-typed-base or
static/immortal restriction as a typed root.

The version-one symbols are:

- `loom_gc_typed_alloc_v1(descriptor, allocation_size, output)`;
- `loom_gc_typed_root_push_v1(frame)`;
- `loom_gc_typed_root_pop_v1(frame)`;
- the shared `loom_gc_safepoint_v1()`.

Allocation size is capped at 1 GiB, alignment at 4,096 bytes, and fixed pointer
cells at 4,096 per object. Pointer offsets must be naturally aligned, strictly
increasing, and wholly inside the fixed prefix. The allocation uses the exact
advertised alignment. Invalid metadata, ABI mismatch, and resource exhaustion
have distinct status codes; ordinary out-of-memory remains an uncatchable
process-level fault.

`output` is set to null before validation or an allocation safepoint. Its cell
address must remain stable throughout the complete call and any triggered
collection, and the cell cannot reside in either moving heap. After the runtime
owns the allocation and copied trace metadata, it publishes the zeroed base
address to `output`. A runtime helper may stage source data, allocate into a
stable private out-cell, initialize without another safepoint, and publish its
final language result last.

## Moving collection

The collector traces live universal values, managed nodes and sequences, text
objects, typed objects, Task frames, and witness instances. It builds
replacement allocations, updates every precise root and internal managed
pointer, then releases dead old storage. Typed tracing follows only the copied
fixed-pointer offsets. Parent-to-child graphs and cycles are marked without
recursion; relocation rewrites both typed root cells and typed object fields.

Runtime clone and aggregate-building helpers use an explicit non-recursive work
stack. This avoids host stack overflow on deeply nested but valid managed
values and makes partially built graphs visible to the root protocol.

Compiler-emitted witness descriptors are static and do not move. Runtime-owned
witness instances use a separate non-moving arena because generated proof
arguments can hold their addresses across a safepoint; unreachable instances
are marked and swept.

The typed LCIR literal-text slice is outside the managed heap. Its immutable
compiler-emitted `TextObject` globals live for the process lifetime, so their
one-pointer values need neither a shadow-stack entry nor relocation. This is
not a general exemption for `Text`. The runtime now has the typed allocation
and root foundation required by future moving LCIR values, but current LCIR
does not yet emit these frames or dynamically allocate typed `Text`. Those
source paths still select complete legacy lowering.

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

Runtime unit tests cover activation, attachment, both root descriptor forms,
shared resource bounds, copied typed layout metadata, advertised alignment,
forced collection, pointer-free trailing-byte preservation, typed
parent/child graphs, cycles, aliased and state-selective roots, legacy/typed
coexistence, relocation, nested managed values, witness mark/sweep, and
partial-construction helpers. The synchronous typed tests also prove that the
heap path constructs no executor. LLVM integration tests exercise
synchronous shadow-stack maps, coroutine state liveness, structured values,
standard-library outputs, and collections at compiler-generated boundaries.

A complete Windows native job is configured with GC/runtime fixture coverage,
but it is not verified Windows runtime or release evidence until a real Windows
runner result is recorded.
