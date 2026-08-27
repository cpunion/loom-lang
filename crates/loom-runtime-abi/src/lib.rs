//! Shared versioned constants for the compiler-private native runtime ABI.
//!
//! Keep representation-specific LLVM field offsets in the code generator;
//! values crossing the runtime boundary are defined here once and consumed by
//! both generated-code declarations and the Rust runtime implementation.

pub const RUNTIME_ABI_VERSION: u32 = 8;
pub const COROUTINE_ABI_VERSION: u32 = 2;
pub const WAIT_ABI_VERSION: u32 = 1;
pub const STANDARD_LIBRARY_ABI_VERSION: u32 = 4;
pub const LAYOUT_ABI_VERSION: u32 = 1;
pub const SHADOW_STACK_ABI_VERSION: u32 = 1;
pub const WITNESS_ABI_VERSION: u32 = 1;
pub const NATIVE_RUNTIME_ABI_IDENTITY: &str = "loom-value-v2/layout-v1/text-v1/wait-v1/task-v2/runtime-v2/gc-v7/shadow-stack-v1/witness-v1/int-list-v1/stdlib-v4";

pub const GC_OK: i32 = 0;
pub const GC_INVALID_ARGUMENT: i32 = 1;
pub const GC_ABI_MISMATCH: i32 = 2;
pub const GC_FRAME_ORDER: i32 = 3;
pub const GC_ROOT_STACK_NOT_EMPTY: i32 = 4;

/// Runtime-owned state bit in [`LoomGcRootFrame::flags`].
///
/// Compiler-generated code must initialize `flags` to zero and must not
/// inspect or modify the field between a successful push and pop.
pub const GC_ROOT_FRAME_LINKED: u32 = 1;

/// Number of machine words in the universal `Value` envelope.
///
/// The current native ABI is deliberately restricted to 64-bit targets, so a
/// runtime word and an LLVM pointer have the same width. Concrete `Text`
/// payloads use only the tag and data words; all other words are zero.
pub const VALUE_SLOT_WORDS: usize = 6;
pub const VALUE_WORD_TAG: usize = 0;
pub const VALUE_WORD_NOMINAL: usize = 1;
pub const VALUE_WORD_AUX: usize = 2;
pub const VALUE_WORD_SCALAR: usize = 3;
pub const VALUE_WORD_DATA: usize = 4;
pub const VALUE_WORD_WITNESS: usize = 5;

/// The owned copy of a mutable dynamic view remains mutable but never keeps a
/// call-scoped writeback carrier.
pub const DYN_FLAG_MUTABLE: u64 = 1;

/// Immutable compiler-emitted dispatch metadata for one conformance.
///
/// `methods` is a dense concept-local table selected by the closed-world
/// reachability plan. Descriptors and their method arrays are process-lifetime
/// constants. They intentionally contain no runtime type or concept identity.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomWitnessDescriptor {
    pub prerequisite_count: u64,
    pub method_count: u64,
    pub methods: *const *const core::ffi::c_void,
}

/// One immutable conformance proof and its recursively supplied prerequisites.
///
/// Instances may be compiler globals, synchronous stack temporaries, Task-owned
/// captures, or allocations in the non-moving GC proof arena. A nonzero
/// descriptor prerequisite count requires a contiguous, non-null
/// `prerequisites` array of exactly that length.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomWitnessInstance {
    pub descriptor: *const LoomWitnessDescriptor,
    pub prerequisites: *const *const LoomWitnessInstance,
}

/// Description of the precise universal-value roots within one native stack
/// frame.
///
/// `live_bitmaps` contains `state_count` rows of exactly `live_bitmap_words`
/// words. Bit `n` identifies entry `n` in the frame's slot-pointer array. Bits
/// beyond `slot_count` in the final word must be zero. Descriptors are
/// runtime-private immutable metadata: generated frames normally reference
/// compiler globals, while runtime helper scopes may own an address-stable
/// descriptor dynamically. In both cases the descriptor and its bitmap must
/// outlive every linked frame which references them.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomGcRootDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub slot_count: u64,
    pub state_count: u64,
    pub live_bitmap_words: u64,
    pub live_bitmaps: *const u64,
}

/// Intrusive shadow-stack header embedded at the start of a generated native
/// stack frame.
///
/// `slots` points to a caller-owned array of `slot_count` pointers to existing
/// universal `Value` allocas. This keeps LLVM storage independent and lets
/// SROA optimize the underlying allocas. Every entry must be non-null and the
/// pointer array is immutable while linked. The compiler updates only `state`
/// before a safepoint. `previous` and `flags` are maintained only by the
/// runtime. Empty descriptors and frames are invalid and should be omitted.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomGcRootFrame {
    pub abi_version: u32,
    pub flags: u32,
    pub state: u64,
    pub descriptor: *const LoomGcRootDescriptor,
    pub slots: *const *mut core::ffi::c_void,
    pub previous: *mut LoomGcRootFrame,
}

pub const VALUE_TAG_UNIT: u64 = 0;
pub const VALUE_TAG_BOOL: u64 = 1;
pub const VALUE_TAG_INT: u64 = 2;
pub const VALUE_TAG_FLOAT: u64 = 3;
pub const VALUE_TAG_TEXT: u64 = 4;
pub const VALUE_TAG_RECORD: u64 = 5;
pub const VALUE_TAG_ENUM: u64 = 6;
pub const VALUE_TAG_REFINED: u64 = 7;
pub const VALUE_TAG_CONSTRAINT_ERROR: u64 = 8;
pub const VALUE_TAG_DYN: u64 = 9;
pub const VALUE_TAG_TUPLE: u64 = 10;
pub const VALUE_TAG_TASK: u64 = 11;
pub const VALUE_TAG_LIST: u64 = 12;
pub const VALUE_TAG_TASK_OUTCOME: u64 = 13;

pub const LAYOUT_KIND_TEXT: u32 = 1;
pub const LAYOUT_KIND_BYTES: u32 = 2;
pub const LAYOUT_FLAG_MANAGED_POINTER: u32 = 1;
pub const LAYOUT_FLAG_LEAF: u32 = 1 << 1;
pub const LAYOUT_FLAG_TRAILING_BYTES: u32 = 1 << 2;

/// Compiler/runtime-private description of one native value layout.
///
/// Descriptor identity is never exposed as language RTTI. In this phase the
/// descriptor is used by managed `Text` allocations and provides the stable
/// prefix which later typed value/container layouts can extend.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoomLayoutDescriptor {
    pub abi_version: u32,
    pub kind: u32,
    pub value_size: u64,
    pub value_align: u64,
    pub object_header_size: u64,
    pub object_align: u64,
    pub flags: u32,
    pub reserved: u32,
}

pub const TEXT_OBJECT_FIELD_LAYOUT: u32 = 0;
pub const TEXT_OBJECT_FIELD_ALLOCATION_SIZE: u32 = 1;
pub const TEXT_OBJECT_FIELD_BYTE_LENGTH: u32 = 2;
pub const TEXT_OBJECT_FIELD_SCALAR_LENGTH: u32 = 3;
pub const TEXT_OBJECT_FIELD_BYTES: u32 = 4;
pub const TEXT_OBJECT_HEADER_SIZE: u64 = 32;
pub const TEXT_OBJECT_ALIGNMENT: u64 = 8;
/// Runtime descriptor referenced by compiler-emitted immortal Text objects.
pub const TEXT_LAYOUT_SYMBOL: &str = "loom_layout_text_v1";
/// Allocation-free UTF-8 byte-subsequence helper used by typed LCIR Text.
pub const TEXT_CONTAINS_SYMBOL: &str = "loom_runtime_text_contains";
pub const FAULT_SCHEMA_VERSION: u32 = 1;
pub const FAULT_FORMAT_ENV: &str = "LOOM_FAULT_FORMAT";
pub const FAULT_FORMAT_JSON: &str = "json";
pub const FAULT_JSON_PREFIX: &str = "LOOM_FAULT_JSON_V1:";

pub const TASK_COMPLETED: i32 = 0;
pub const TASK_PENDING: i32 = 1;
pub const TASK_FAULTED: i32 = 2;
pub const TASK_CANCELLED: i32 = 3;

pub const TASK_JOIN_ALL: u32 = 0;
pub const TASK_JOIN_SETTLED: u32 = 1;
pub const TASK_JOIN_ANY: u32 = 2;
pub const TASK_JOIN_RACE: u32 = 3;

pub const WAIT_OK: i32 = 0;
pub const WAIT_INVALID_ARGUMENT: i32 = 1;
pub const WAIT_UNSUPPORTED: i32 = 2;
pub const WAIT_SYSTEM_ERROR: i32 = 3;
pub const WAIT_DUPLICATE_SOURCE: i32 = 4;
pub const WAIT_STALE_REGISTRATION: i32 = 5;
pub const WAIT_NO_MEMORY: i32 = 6;

pub const WAIT_SOURCE_TIMER: u32 = 1;
pub const WAIT_SOURCE_IO: u32 = 2;
pub const WAIT_SOURCE_COMPLETION: u32 = 3;

pub const WAIT_READABLE: u32 = 1 << 0;
pub const WAIT_WRITABLE: u32 = 1 << 1;

pub const READY_READABLE: u32 = 1 << 0;
pub const READY_WRITABLE: u32 = 1 << 1;
pub const READY_TIMER: u32 = 1 << 2;
pub const READY_COMPLETED: u32 = 1 << 3;
pub const READY_CLOSED: u32 = 1 << 4;
pub const READY_ERROR: u32 = 1 << 5;

#[cfg(test)]
mod tests {
    use std::mem::{align_of, offset_of, size_of};

    use super::{
        COROUTINE_ABI_VERSION, LAYOUT_ABI_VERSION, LoomGcRootDescriptor, LoomGcRootFrame,
        LoomWitnessDescriptor, LoomWitnessInstance, NATIVE_RUNTIME_ABI_IDENTITY,
        RUNTIME_ABI_VERSION, SHADOW_STACK_ABI_VERSION, STANDARD_LIBRARY_ABI_VERSION,
        TEXT_CONTAINS_SYMBOL, TEXT_LAYOUT_SYMBOL, TEXT_OBJECT_ALIGNMENT,
        TEXT_OBJECT_FIELD_ALLOCATION_SIZE, TEXT_OBJECT_FIELD_BYTE_LENGTH, TEXT_OBJECT_FIELD_BYTES,
        TEXT_OBJECT_FIELD_LAYOUT, TEXT_OBJECT_FIELD_SCALAR_LENGTH, TEXT_OBJECT_HEADER_SIZE,
        WITNESS_ABI_VERSION,
    };

    #[test]
    fn native_runtime_identity_is_pinned() {
        assert_eq!(RUNTIME_ABI_VERSION, 8);
        assert_eq!(COROUTINE_ABI_VERSION, 2);
        assert_eq!(LAYOUT_ABI_VERSION, 1);
        assert_eq!(SHADOW_STACK_ABI_VERSION, 1);
        assert_eq!(WITNESS_ABI_VERSION, 1);
        assert_eq!(STANDARD_LIBRARY_ABI_VERSION, 4);
        assert_eq!(
            NATIVE_RUNTIME_ABI_IDENTITY,
            "loom-value-v2/layout-v1/text-v1/wait-v1/task-v2/runtime-v2/gc-v7/shadow-stack-v1/witness-v1/int-list-v1/stdlib-v4",
        );
    }

    #[test]
    fn compiler_visible_text_layout_declarations_are_pinned() {
        assert_eq!(TEXT_OBJECT_FIELD_LAYOUT, 0);
        assert_eq!(TEXT_OBJECT_FIELD_ALLOCATION_SIZE, 1);
        assert_eq!(TEXT_OBJECT_FIELD_BYTE_LENGTH, 2);
        assert_eq!(TEXT_OBJECT_FIELD_SCALAR_LENGTH, 3);
        assert_eq!(TEXT_OBJECT_FIELD_BYTES, 4);
        assert_eq!(TEXT_OBJECT_HEADER_SIZE, 32);
        assert_eq!(TEXT_OBJECT_ALIGNMENT, 8);
        assert_eq!(TEXT_LAYOUT_SYMBOL, "loom_layout_text_v1");
        assert_eq!(TEXT_CONTAINS_SYMBOL, "loom_runtime_text_contains");
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn shadow_stack_layout_is_pinned_for_the_native_64_bit_abi() {
        assert_eq!(size_of::<LoomGcRootDescriptor>(), 40);
        assert_eq!(align_of::<LoomGcRootDescriptor>(), 8);
        assert_eq!(offset_of!(LoomGcRootDescriptor, abi_version), 0);
        assert_eq!(offset_of!(LoomGcRootDescriptor, flags), 4);
        assert_eq!(offset_of!(LoomGcRootDescriptor, slot_count), 8);
        assert_eq!(offset_of!(LoomGcRootDescriptor, state_count), 16);
        assert_eq!(offset_of!(LoomGcRootDescriptor, live_bitmap_words), 24);
        assert_eq!(offset_of!(LoomGcRootDescriptor, live_bitmaps), 32);

        assert_eq!(size_of::<LoomGcRootFrame>(), 40);
        assert_eq!(align_of::<LoomGcRootFrame>(), 8);
        assert_eq!(offset_of!(LoomGcRootFrame, abi_version), 0);
        assert_eq!(offset_of!(LoomGcRootFrame, flags), 4);
        assert_eq!(offset_of!(LoomGcRootFrame, state), 8);
        assert_eq!(offset_of!(LoomGcRootFrame, descriptor), 16);
        assert_eq!(offset_of!(LoomGcRootFrame, slots), 24);
        assert_eq!(offset_of!(LoomGcRootFrame, previous), 32);

        assert_eq!(size_of::<LoomWitnessDescriptor>(), 24);
        assert_eq!(align_of::<LoomWitnessDescriptor>(), 8);
        assert_eq!(offset_of!(LoomWitnessDescriptor, prerequisite_count), 0);
        assert_eq!(offset_of!(LoomWitnessDescriptor, method_count), 8);
        assert_eq!(offset_of!(LoomWitnessDescriptor, methods), 16);

        assert_eq!(size_of::<LoomWitnessInstance>(), 16);
        assert_eq!(align_of::<LoomWitnessInstance>(), 8);
        assert_eq!(offset_of!(LoomWitnessInstance, descriptor), 0);
        assert_eq!(offset_of!(LoomWitnessInstance, prerequisites), 8);
    }
}
