# Value layout and native ABI

The native layout described here is compiler-private. It exists so generated
LLVM and the Rust runtime agree within one toolchain build. Source code cannot
inspect tags, pointers, allocation addresses, witness descriptors, or calling
conventions, and external code must not depend on them.

Production native compilation selects one representation boundary for an
entire reachable artifact. A completely supported direct artifact uses typed
LCIR for primitive values, structural tuples, and closed POD records. Any
reachable feature outside current LCIR coverage selects the complete legacy
layout below; the two callable ABIs are never mixed in one object.

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

## Legacy primitive and aggregate specialization

Within a complete legacy object, `Unit`, `Bool`, `Int`, and `Float` can use
direct private LLVM values in eligible internal calls. Monomorphic records with
no invariant and only direct primitive fields can use a separate closed-world
specialization:

- ordinary by-value parameters and readonly receivers are aggregate values;
- a mutable receiver uses a call-scoped in/out pointer;
- results are returned directly when the closed-world fault requirements allow
  it, or with an internal status when they do not.

Unsupported specialization boundaries materialize the independent universal
representation. This preserves value-copy semantics and prevents a private
stack address from escaping. These layouts are legacy emitter decisions and
are not reused by typed LCIR.

Records, enums, refined values, tuples, and generic lists on the universal path
use GC-managed nodes. Logical copy is independent: mutating one value cannot
write through an earlier copy merely because the runtime shares an allocation
internally.

## Typed LCIR representations

The independent `loom-codegen-ir` foundation catalogs `Unit` as `Zst`, `Bool`
as `I1`, `Int` as `I64`, `Float` as `F64`, and each supported structural tuple
or closed record as an immutable `Product` of canonical element or field value
types. Tuples and records may contain one another as long as the representation
graph is acyclic. An explicit registration table chooses the canonical value
representation for a semantic type; other representation alternatives do not
compete merely because they have the same semantic type.

The checked-artifact LLVM API maps a product to a literal LLVM struct and emits
construction, projection, and functional field replacement as `insertvalue`
and `extractvalue`. Product parameters, returns, block phis, and loop-carried
values remain direct SSA. Ordinary product copy or move copies the SSA value;
mutation reconstructs the changed path and therefore cannot write through an
earlier copy. No LCIR product requires a `ValueSlot`, record allocation, GC
trace metadata, executor, or source-function `alloca`.

An infallible function with no inout parameters returns its source result `T`
directly. With ordered functional writebacks `W...`, it returns `{ T, W... }`.
A faulting function returns `{ i32 status, T, W... }` and receives one hidden
fault-context pointer. Normal and fault exits both return the latest inout
values; the source result is zero-filled on a fault. This is a
compiler-private object ABI, not a native library ABI.

The production automatic route uses this typed ABI for eligible build, run,
and test artifacts. Tuple construction and `let` destructuring are direct SSA
construction and extraction; they do not allocate tuple nodes. Generic
records, invariants, refined or managed aggregate elements, runtime-checked
construction, projected inout arguments, concepts, contracts, cleanup, and
async operations still select the complete universal route. Typed LCIR does
not change the legacy runtime ABI or make either object ABI public.

See [Code generation IR](codegen-ir.md) for the implemented foundation and the
[typed code generation IR RFC](../rfcs/typed-codegen-ir.md) for the accepted
representation and migration design.

## `Text`

A universal `Text` slot contains its tag and one pointer to a `TextObject`.
The object has a versioned layout descriptor, allocation size, UTF-8 byte
length, Unicode scalar length, and trailing UTF-8 bytes. String literals use
the same object layout in immortal globals; dynamically created text is moved
by the GC.

The descriptor is runtime trace/layout metadata. It is not a source-visible
tag and does not make `Text` a dynamic type. No direct typed-call `Text` layout
is implemented.

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

Element access on this proved layout retains the allocation's pointer
provenance through typed LLVM GEP instructions. Exact range scans can therefore
be vectorized without weakening the source bounds rules. The append lowering
also carries its range induction value and non-observable length in SSA so the
private allocation cannot make LLVM conservatively reload or update the header
on every iteration. Length is synchronized at allocation-growth boundaries and
normal loop exit; an element expression that references its receiver keeps
eager length updates.

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
