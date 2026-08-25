//! Shared versioned constants for the compiler-private native runtime ABI.
//!
//! Keep representation-specific LLVM field offsets in the code generator;
//! values crossing the runtime boundary are defined here once and consumed by
//! both generated-code declarations and the Rust runtime implementation.

pub const RUNTIME_ABI_VERSION: u32 = 3;
pub const COROUTINE_ABI_VERSION: u32 = 1;
pub const WAIT_ABI_VERSION: u32 = 1;
pub const STANDARD_LIBRARY_ABI_VERSION: u32 = 3;
pub const LAYOUT_ABI_VERSION: u32 = 1;
pub const NATIVE_RUNTIME_ABI_IDENTITY: &str =
    "loom-value-v2/layout-v1/text-v1/wait-v1/task-v1/runtime-v1/gc-v3/int-list-v1/stdlib-v3";

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
pub const WAIT_SOURCE_FD: u32 = 2;
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
    use super::{
        LAYOUT_ABI_VERSION, NATIVE_RUNTIME_ABI_IDENTITY, RUNTIME_ABI_VERSION,
        STANDARD_LIBRARY_ABI_VERSION,
    };

    #[test]
    fn native_runtime_identity_is_pinned() {
        assert_eq!(RUNTIME_ABI_VERSION, 3);
        assert_eq!(LAYOUT_ABI_VERSION, 1);
        assert_eq!(STANDARD_LIBRARY_ABI_VERSION, 3);
        assert_eq!(
            NATIVE_RUNTIME_ABI_IDENTITY,
            "loom-value-v2/layout-v1/text-v1/wait-v1/task-v1/runtime-v1/gc-v3/int-list-v1/stdlib-v3",
        );
    }
}
