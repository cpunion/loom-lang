# Value layout and native ABI

The native layout described here is compiler-private. It exists so generated
LLVM and the Rust runtime agree within one toolchain build. Source code cannot
inspect tags, pointers, allocation addresses, witness descriptors, or calling
conventions, and external code must not depend on them.

## Universal value envelope

The complete fallback representation is `ValueSlot`: six 64-bit words.
Current words carry a value tag, nominal metadata, auxiliary data, scalar data,
a managed-data pointer, and a witness pointer. Different value kinds use
different subsets and zero unused words.

The tag lets shared runtime helpers clone, compare, trace, format, and destroy
values whose static type has been erased by the current generic/universal
lowering. It is not language reflection or a permanent per-value type ID.

The current runtime ABI identity is versioned as a whole in
`loom-runtime-abi`. That identity is checked in runtime bundles and object
linking. It is not backward-compatible with earlier identities and is not a
public ABI.

## Primitive and aggregate representation

`Unit`, `Bool`, `Int`, and `Float` can use direct private LLVM values in
eligible internal calls. Monomorphic records with no invariant and only direct
primitive fields can use first-class LLVM aggregates:

- ordinary by-value parameters and readonly receivers are aggregate values;
- a mutable receiver uses a call-scoped in/out pointer;
- results are returned directly when the closed-world fault requirements allow
  it, or with an internal status when they do not.

Unsupported boundaries materialize the independent universal representation.
This preserves value-copy semantics and prevents a private stack address from
escaping.

Records, enums, refined values, tuples, and generic lists on the universal path
use GC-managed nodes. Logical copy is independent: mutating one value cannot
write through an earlier copy merely because the runtime shares an allocation
internally.

## `Text`

A universal `Text` slot contains its tag and one pointer to a `TextObject`.
The object has a versioned layout descriptor, allocation size, UTF-8 byte
length, Unicode scalar length, and trailing UTF-8 bytes. String literals use
the same object layout in immortal globals; dynamically created text is moved
by the GC.

The descriptor is runtime trace/layout metadata. It is not a source-visible
tag and does not make `Text` a dynamic type. A future direct typed-call layout
may bypass the universal envelope without changing `Text` semantics.

## Dynamic concept values

A `dyn C` value carries data plus an already selected conformance witness.
Witness descriptors contain only prerequisite and live method-slot tables
needed by generated code. Descriptor identity is not source RTTI.

The compiler may represent a known dynamic value with fewer machine values or
fold a witness into static code when that is unobservable. The semantic
requirement is selected conformance and value behavior, not a permanently
fixed two-word fat pointer.

Loom does not support runtime conversion from an untyped universal value to
`dyn C` by searching every conformance. This keeps witness reachability
closed-world.

## Managed layout and GC metadata

Managed allocations have static layout metadata sufficient for precise
tracing. Synchronous native frames publish pointers to live universal slots
through a versioned shadow-stack descriptor and per-state bitmaps. Coroutine
descriptors publish live Task-frame slots and captured witnesses.

Witness descriptors emitted by the compiler are immutable process-lifetime
constants. Dynamically assembled witness instances live in a non-moving proof
arena because generated hidden arguments can retain their address across a
safepoint; the arena is still marked and swept.

## Specialized local storage

The LLVM backend currently has a narrow non-escaping local `List[Int]` layout
using contiguous `i64` storage with length and capacity. It applies only when a
closed-world use scan proves no copy, escape, generic/witness boundary,
suspension, or cleanup hazard. All other list shapes use the complete generic
representation.

Similarly, checked integer and flat-record optimizations are private lowering
choices. They must never appear as requirements in the language reference.

## ABI change checklist

A native-layout change normally requires:

- a new shared ABI identity or component version;
- generated LLVM declaration and field-offset updates;
- runtime implementation and trace/clone/drop updates;
- runtime-bundle compatibility checks;
- native-object fingerprint invalidation;
- forced-moving-GC, malformed-descriptor, differential, and release tests.

Do not preserve an old layout merely to maintain accidental compatibility. If
external native interoperability is added, it needs a separately specified,
stable boundary with explicit conversion.
