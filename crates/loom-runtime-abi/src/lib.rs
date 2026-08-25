//! Shared versioned constants for the compiler-private native runtime ABI.
//!
//! Keep representation-specific LLVM field offsets in the code generator;
//! values crossing the runtime boundary are defined here once and consumed by
//! both generated-code declarations and the Rust runtime implementation.

pub const RUNTIME_ABI_VERSION: u32 = 1;
pub const COROUTINE_ABI_VERSION: u32 = 1;
pub const WAIT_ABI_VERSION: u32 = 1;
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
