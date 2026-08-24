//! Compiler-private native ABI.
//!
//! Values use a uniform representation in the first native backend. Control
//! flow is compiled, while this representation keeps generic functions
//! shareable and lets the backend support nominal aggregates before committing
//! to a stable public ABI. Aggregate children and argument lists are linked
//! nodes so the safe Inkwell API is sufficient; Loom's Rust workspace forbids
//! handwritten unsafe code.

pub(crate) const VALUE_TAG_UNIT: u64 = 0;
pub(crate) const VALUE_TAG_BOOL: u64 = 1;
pub(crate) const VALUE_TAG_INT: u64 = 2;
pub(crate) const VALUE_TAG_FLOAT: u64 = 3;
pub(crate) const VALUE_TAG_TEXT: u64 = 4;
pub(crate) const VALUE_TAG_RECORD: u64 = 5;
pub(crate) const VALUE_TAG_ENUM: u64 = 6;
pub(crate) const VALUE_TAG_REFINED: u64 = 7;
pub(crate) const VALUE_TAG_VIOLATION: u64 = 8;
pub(crate) const VALUE_TAG_DYN: u64 = 9;
pub(crate) const DYN_FLAG_MUTABLE: u64 = 1;
pub(crate) const DYN_FLAG_WRITEBACK: u64 = 2;
pub(crate) const VALUE_TAG_TUPLE: u64 = 10;
pub(crate) const VALUE_TAG_TASK: u64 = 11;
pub(crate) const VALUE_TAG_LIST: u64 = 12;
pub(crate) const VALUE_TAG_TASK_OUTCOME: u64 = 13;

pub(crate) const TASK_VALUE_DIRECT: u64 = 0;

pub(crate) const JOIN_RESULT_SCALAR: u64 = 0;
pub(crate) const JOIN_RESULT_TUPLE: u64 = 1;
pub(crate) const JOIN_RESULT_LIST: u64 = 2;
pub(crate) const JOIN_RESULT_OUTCOME: u64 = 3;
pub(crate) const JOIN_RESULT_OUTCOME_TUPLE: u64 = 4;
pub(crate) const JOIN_RESULT_OUTCOME_LIST: u64 = 5;

pub(crate) const TASK_STEP_COMPLETED: u64 = 0;
pub(crate) const TASK_STEP_PENDING: u64 = 1;
pub(crate) const TASK_STEP_FAULTED: u64 = 2;
pub(crate) const TASK_STEP_CANCELLED: u64 = 3;

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
pub(crate) const WITNESS_NODE_FIELD_VALUE: u32 = 0;
pub(crate) const WITNESS_NODE_FIELD_NEXT: u32 = 1;

/// Witness field zero is the linked list of prerequisite proof arguments.
/// Requirement method slots follow in global `RequirementId` order.
pub(crate) const WITNESS_METHOD_FIELD_OFFSET: u32 = 1;

pub(crate) const WAIT_ABI_VERSION: u64 = 1;
pub(crate) const WAIT_SOURCE_KIND_TIMER: u64 = 1;
pub(crate) const WAIT_SOURCE_KIND_FD: u64 = 2;
pub(crate) const WAIT_SOURCE_KIND_COMPLETION: u64 = 3;
pub(crate) const WAIT_INTEREST_READABLE: u64 = 1 << 0;
pub(crate) const WAIT_INTEREST_WRITABLE: u64 = 1 << 1;
pub(crate) const READY_EVENT_TIMER: u64 = 1 << 2;
pub(crate) const READY_EVENT_COMPLETED: u64 = 1 << 3;

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
