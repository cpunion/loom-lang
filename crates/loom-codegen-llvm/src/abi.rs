//! Compiler-private native ABI.
//!
//! Values use a uniform representation in the first native backend. Control
//! flow is compiled, while this representation keeps generic functions
//! shareable and lets the backend support nominal aggregates before committing
//! to a stable public ABI. Aggregate children and argument lists are linked
//! nodes so the safe Inkwell API is sufficient; Loom's Rust workspace forbids
//! handwritten unsafe code.

pub(crate) use loom_runtime_abi::{
    DYN_FLAG_MUTABLE, VALUE_TAG_BOOL, VALUE_TAG_CONSTRAINT_ERROR, VALUE_TAG_DYN, VALUE_TAG_ENUM,
    VALUE_TAG_FLOAT, VALUE_TAG_INT, VALUE_TAG_LIST, VALUE_TAG_RECORD, VALUE_TAG_REFINED,
    VALUE_TAG_TASK, VALUE_TAG_TASK_OUTCOME, VALUE_TAG_TEXT, VALUE_TAG_TUPLE, VALUE_TAG_UNIT,
};

pub(crate) const TASK_VALUE_DIRECT: u64 = 0;

pub(crate) const JOIN_RESULT_SCALAR: u64 = 0;
pub(crate) const JOIN_RESULT_TUPLE: u64 = 1;
pub(crate) const JOIN_RESULT_LIST: u64 = 2;
pub(crate) const JOIN_RESULT_OUTCOME: u64 = 3;
pub(crate) const JOIN_RESULT_OUTCOME_TUPLE: u64 = 4;
pub(crate) const JOIN_RESULT_OUTCOME_LIST: u64 = 5;

pub(crate) const TASK_STEP_COMPLETED: u64 = loom_runtime_abi::TASK_COMPLETED as u64;
pub(crate) const TASK_STEP_PENDING: u64 = loom_runtime_abi::TASK_PENDING as u64;
pub(crate) const TASK_STEP_FAULTED: u64 = loom_runtime_abi::TASK_FAULTED as u64;
pub(crate) const TASK_STEP_CANCELLED: u64 = loom_runtime_abi::TASK_CANCELLED as u64;

pub(crate) const VALUE_FIELD_TAG: u32 = 0;
pub(crate) const VALUE_FIELD_NOMINAL: u32 = 1;
pub(crate) const VALUE_FIELD_AUX: u32 = 2;
pub(crate) const VALUE_FIELD_SCALAR: u32 = 3;
pub(crate) const VALUE_FIELD_DATA: u32 = 4;
pub(crate) const VALUE_FIELD_WITNESS: u32 = 5;

pub(crate) const VALUE_NODE_FIELD_VALUE: u32 = 0;
pub(crate) const VALUE_NODE_FIELD_NEXT: u32 = 1;
pub(crate) const ARG_NODE_FIELD_VALUE: u32 = 0;
pub(crate) const ARG_NODE_FIELD_NEXT: u32 = 1;
pub(crate) const WITNESS_DESCRIPTOR_FIELD_METHODS: u32 = 2;
pub(crate) const WITNESS_INSTANCE_FIELD_DESCRIPTOR: u32 = 0;
pub(crate) const WITNESS_INSTANCE_FIELD_PREREQUISITES: u32 = 1;

pub(crate) const WAIT_ABI_VERSION: u64 = loom_runtime_abi::WAIT_ABI_VERSION as u64;
pub(crate) const COROUTINE_ABI_VERSION: u64 = loom_runtime_abi::COROUTINE_ABI_VERSION as u64;
pub(crate) const WAIT_SOURCE_KIND_TIMER: u64 = loom_runtime_abi::WAIT_SOURCE_TIMER as u64;
pub(crate) const WAIT_SOURCE_KIND_IO: u64 = loom_runtime_abi::WAIT_SOURCE_IO as u64;
pub(crate) const WAIT_SOURCE_KIND_COMPLETION: u64 = loom_runtime_abi::WAIT_SOURCE_COMPLETION as u64;
pub(crate) const WAIT_INTEREST_READABLE: u64 = loom_runtime_abi::WAIT_READABLE as u64;
pub(crate) const WAIT_INTEREST_WRITABLE: u64 = loom_runtime_abi::WAIT_WRITABLE as u64;
pub(crate) const READY_EVENT_TIMER: u64 = loom_runtime_abi::READY_TIMER as u64;
pub(crate) const READY_EVENT_COMPLETED: u64 = loom_runtime_abi::READY_COMPLETED as u64;

pub(crate) const WAIT_SOURCE_FIELD_ABI_VERSION: u32 = 0;
pub(crate) const WAIT_SOURCE_FIELD_KIND: u32 = 1;
pub(crate) const WAIT_SOURCE_FIELD_HANDLE: u32 = 2;
pub(crate) const WAIT_SOURCE_FIELD_INTERESTS: u32 = 3;
pub(crate) const WAIT_SOURCE_FIELD_RESERVED: u32 = 4;
pub(crate) const WAIT_SOURCE_FIELD_DEADLINE: u32 = 5;

pub(crate) const READY_NOTIFICATION_FIELD_FRAME: u32 = 1;
pub(crate) const READY_NOTIFICATION_FIELD_EVENTS: u32 = 2;

pub(crate) const COROUTINE_FRAME_FIELD_STATE: u32 = 0;
pub(crate) const COROUTINE_FRAME_FIELD_RESULT: u32 = 1;

pub(crate) const GC_ROOT_FRAME_FIELD_ABI_VERSION: u32 = 0;
pub(crate) const GC_ROOT_FRAME_FIELD_FLAGS: u32 = 1;
pub(crate) const GC_ROOT_FRAME_FIELD_STATE: u32 = 2;
pub(crate) const GC_ROOT_FRAME_FIELD_DESCRIPTOR: u32 = 3;
pub(crate) const GC_ROOT_FRAME_FIELD_SLOTS: u32 = 4;
pub(crate) const GC_ROOT_FRAME_FIELD_PREVIOUS: u32 = 5;
