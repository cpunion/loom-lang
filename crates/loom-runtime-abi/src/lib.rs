//! Shared versioned constants for the compiler-private native runtime ABI.
//!
//! Keep representation-specific LLVM field offsets in the code generator;
//! values crossing the runtime boundary are defined here once and consumed by
//! both generated-code declarations and the Rust runtime implementation.

pub const RUNTIME_ABI_VERSION: u32 = 40;
pub const TYPED_TASK_ABI_VERSION: u32 = 1;
pub const WAIT_ABI_VERSION: u32 = 1;
pub const STDLIB_ABI_VERSION: u32 = 10;
pub const LAYOUT_ABI_VERSION: u32 = 1;
pub const TYPED_GC_ABI_VERSION: u32 = 1;
pub const TYPED_GC_REPEATED_ABI_VERSION: u32 = 1;
pub const TYPED_SHADOW_STACK_ABI_VERSION: u32 = 1;
pub const TYPED_IO_ABI_VERSION: u32 = 1;
pub const TYPED_PROCESS_ABI_VERSION: u32 = 1;
pub const NATIVE_RUNTIME_ABI_IDENTITY: &str = "layout-v1/text-v4/wait-v1/task-v2/typed-task-v1/typed-task-adopt-v1/typed-task-winner-finalize-v1/typed-task-outcome-v1/typed-resource-ownership-v1/typed-timer-v1/typed-resource-v1/typed-io-v1/format-float-v1/typed-bytes-v2/typed-path-v1/typed-log-v1/stdout-v1/typed-process-v1/runtime-v34/gc-v9/typed-gc-v1/typed-repeated-v1/typed-shadow-stack-v1/stdlib-v10";

/// Initializes the process-wide immutable argument snapshot. Generated `main`
/// calls this only when the reachable program reads process arguments. Unix
/// copies `argv[1..argc]`; Windows preserves the ABI signature but ignores the
/// narrow pointers and reads the operating system's wide argument source.
/// Zero reports a newly installed snapshot; repeated or malformed Unix
/// initialization returns a nonzero ABI defect status.
pub const PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL: &str =
    "loom_runtime_process_arguments_initialize_typed_v1";
/// Returns the number of entries in the initialized argument snapshot, or a
/// negative value when no snapshot exists.
pub const PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL: &str =
    "loom_runtime_process_argument_count_typed_v1";
pub const PROCESS_ARGUMENT_COUNT_TYPED_INVALID: i64 = -1;
/// Allocates one snapshot entry as canonical managed Text. The boundary takes
/// `(index, output)` and returns zero only after publishing a complete value.
pub const PROCESS_ARGUMENT_AT_TYPED_SYMBOL: &str = "loom_runtime_process_argument_at_typed_v1";
/// Looks up one canonical Text name and publishes a canonical managed Text
/// value through `output` only for the found status.
pub const PROCESS_ENVIRONMENT_TYPED_SYMBOL: &str = "loom_runtime_process_environment_typed_v1";
pub const PROCESS_ARGUMENT_TYPED_OK: i32 = 0;
pub const PROCESS_ARGUMENT_TYPED_INVALID: i32 = 1;
pub const PROCESS_ENVIRONMENT_TYPED_INVALID: i32 = -1;
pub const PROCESS_ENVIRONMENT_TYPED_MISSING: i32 = 0;
pub const PROCESS_ENVIRONMENT_TYPED_FOUND: i32 = 1;

/// Writes exactly `length` bytes to the process standard-output stream.
///
/// The boundary never scans for NUL, adds a line ending, or applies the C
/// runtime's platform text-mode translation. A zero length accepts a null data
/// pointer and still flushes the process stream; every nonzero call requires a
/// readable range no larger than `isize::MAX`. `STDOUT_WRITE_OK` means the
/// complete range was accepted and flushed. `STDOUT_WRITE_FAILED` may have
/// emitted a prefix, so generated callers fail without retrying. Unix runtime
/// initialization for this boundary ignores `SIGPIPE`, making a closed pipe a
/// reported write failure instead of an uncatchable signal exit.
pub const STDOUT_WRITE_SYMBOL: &str = "loom_runtime_stdout_write_v1";
pub const STDOUT_WRITE_OK: i32 = 0;
pub const STDOUT_WRITE_INVALID_ARGUMENT: i32 = 1;
pub const STDOUT_WRITE_FAILED: i32 = 2;

/// Writes one canonical structured-log line from direct typed Text values.
///
/// The synchronous boundary takes `(level, message, fields, field_count)`.
/// `message` and every field pointer name complete canonical Text objects;
/// `fields` borrows a contiguous array of [`LoomTypedLogField`] for the call.
/// The runtime neither retains these pointers nor enters a Loom GC safepoint.
/// A failed write may have emitted a prefix, so generated callers must not
/// retry it.
pub const TYPED_LOG_WRITE_SYMBOL: &str = "loom_runtime_log_typed_v1";
pub const TYPED_LOG_OK: i32 = 0;
pub const TYPED_LOG_INVALID_ARGUMENT: i32 = 1;
pub const TYPED_LOG_WRITE_FAILED: i32 = 2;
pub const TYPED_LOG_FIELD_SIZE: u64 = 16;
pub const TYPED_LOG_FIELD_ALIGNMENT: u64 = 8;
pub const TYPED_LOG_FIELD_KEY_OFFSET: u64 = 0;
pub const TYPED_LOG_FIELD_VALUE_OFFSET: u64 = 8;

pub const GC_OK: i32 = 0;
pub const GC_INVALID_ARGUMENT: i32 = 1;
pub const GC_ABI_MISMATCH: i32 = 2;
pub const GC_FRAME_ORDER: i32 = 3;
pub const GC_ROOT_STACK_NOT_EMPTY: i32 = 4;
pub const GC_DESCRIPTOR_INVALID: i32 = 5;
pub const GC_RESOURCE_LIMIT: i32 = 6;

/// Hard limits for the typed synchronous root ABI.
///
/// The compiler must reject a function whose root map exceeds these bounds.
/// Runtime validation repeats the checks before linking an untrusted frame so
/// collection work and descriptor reads remain bounded.
pub const GC_MAX_ROOT_SLOTS: u64 = 65_536;
pub const GC_MAX_ROOT_STATES: u64 = 65_536;
pub const GC_MAX_ROOT_BITMAP_WORDS: u64 = 1_048_576;
pub const GC_MAX_ROOT_DEPTH: u64 = 65_536;

/// Hard limits for one typed managed allocation descriptor and allocation.
pub const GC_MAX_OBJECT_POINTERS: u64 = 4_096;
/// Maximum number of exact pointer cells traced in one repeated allocation.
pub const GC_MAX_REPEATED_POINTER_CELLS: u64 = 16_777_216;
pub const GC_MAX_OBJECT_BYTES: u64 = 1 << 30;
pub const GC_MAX_OBJECT_ALIGNMENT: u64 = 4_096;

/// Hard byte limit for each copied component of a typed Task fault.
pub const TYPED_TASK_MAX_FAULT_TEXT_BYTES: u64 = 64 * 1024;

/// Creates and publishes one zero-root typed `Task[Unit]` which becomes ready
/// at the supplied absolute monotonic deadline. The returned null pointer
/// reports allocation, context, or unpublished-construction failure.
pub const TYPED_TIMER_TASK_CREATE_SYMBOL: &str = "loom_typed_timer_task_create_v1";
/// Atomically transfers typed children from the active parent into one
/// initialized unpublished composite Task and publishes that composite.
pub const TYPED_TASK_PUBLISH_ADOPTING_SYMBOL: &str = "loom_typed_task_publish_adopting_v1";
/// Atomically consumes one terminal typed child into exact caller storage.
///
/// Completed results use the descriptor-checked move ABI. Faulted results
/// publish independent managed Text values for the primary code and message;
/// cancelled results have no payload. A successful call detaches and retires
/// the child and returns one of `TASK_COMPLETED`, `TASK_FAULTED`, or
/// `TASK_CANCELLED`. `TYPED_TASK_STATUS_INVALID` reports every invalid call.
pub const TYPED_TASK_TAKE_OUTCOME_SYMBOL: &str = "loom_typed_task_take_outcome_v1";

/// Creates one typed I/O leaf Task from a fully staged request.
///
/// The boundary takes `(executor, typed_task_descriptor, request)`. The
/// runtime copies every borrowed byte before retaining the operation and
/// duplicates every source File/Socket before returning. A null Task reports
/// an invalid compiler/runtime call or Task allocation failure; ordinary host
/// I/O failures are retained by the Task and later published as
/// [`TYPED_IO_OUTCOME_ERROR`].
pub const TYPED_IO_TASK_CREATE_SYMBOL: &str = "loom_typed_io_task_create_v1";
/// Advances the active typed I/O leaf from its generated resume callback.
///
/// The boundary takes `(task, executor, scratch_text_cell, outcome)`. The Text
/// cell must be an exact, currently-live non-result root in the Task frame and
/// must initially contain null. `outcome` must not overlap that frame. Pending
/// and faulted steps publish no source value. A completed Text or Error writes
/// one managed Text into the scratch cell; generated code then constructs its
/// exact target-layout Result without a safepoint and publishes the result.
pub const TYPED_IO_POLL_SYMBOL: &str = "loom_typed_io_poll_v1";
/// Non-suspending cancellation callback for typed I/O leaf descriptors.
pub const TYPED_IO_CANCEL_SYMBOL: &str = "loom_typed_io_cancel_v1";

pub const TYPED_IO_OPERATION_FILE_OPEN_READ: u32 = 1;
pub const TYPED_IO_OPERATION_FILE_CREATE: u32 = 2;
pub const TYPED_IO_OPERATION_FILE_READ_TEXT: u32 = 3;
pub const TYPED_IO_OPERATION_FILE_WRITE_TEXT: u32 = 4;
pub const TYPED_IO_OPERATION_SOCKET_CONNECT: u32 = 5;
pub const TYPED_IO_OPERATION_SOCKET_READ_TEXT: u32 = 6;
pub const TYPED_IO_OPERATION_SOCKET_WRITE_TEXT: u32 = 7;

/// Canonical all-ones value for an operation with no source resource and for
/// the closed state of a direct File/Socket record.
pub const TYPED_IO_INVALID_RESOURCE_TOKEN: u64 = u64::MAX;

pub const TYPED_IO_OUTCOME_UNIT: u32 = 1;
pub const TYPED_IO_OUTCOME_TEXT: u32 = 2;
pub const TYPED_IO_OUTCOME_RESOURCE: u32 = 3;
pub const TYPED_IO_OUTCOME_ERROR: u32 = 4;

/// Default fault class for operation-specific File/Socket failures.
pub const TYPED_IO_FAULT_CLASS_OPERATION: u64 = 0;
/// Fault class for a Socket connect port outside `0..=65535`.
pub const TYPED_IO_FAULT_CLASS_INVALID_PORT: u64 = 1;
/// Fault class for Socket host resolution failure or an empty address set.
pub const TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE: u64 = 2;

/// Zeroed typed allocator taking `(descriptor, allocation_size, output)`.
///
/// `output` must name writable pointer-sized storage whose address remains
/// stable for the complete call, including any collection triggered by the
/// allocator. The output cell must not reside in the moving heap.
pub const TYPED_GC_ALLOC_SYMBOL: &str = "loom_gc_typed_alloc_v1";
/// Zeroed repeated-element allocator taking `(descriptor, capacity, output)`.
pub const TYPED_GC_REPEATED_ALLOC_SYMBOL: &str = "loom_gc_typed_repeated_alloc_v1";
pub const TYPED_GC_ROOT_PUSH_SYMBOL: &str = "loom_gc_typed_root_push_v1";
pub const TYPED_GC_ROOT_POP_SYMBOL: &str = "loom_gc_typed_root_pop_v1";
/// Stages two complete Text payloads before its typed allocation safepoint and
/// publishes the initialized managed leaf through its output cell.
pub const TEXT_CONCAT_TYPED_SYMBOL: &str = "loom_runtime_text_concat_typed_v1";
/// Canonical binary64 formatter publishing one direct managed Text pointer.
pub const FORMAT_FLOAT_TYPED_SYMBOL: &str = "loom_runtime_format_float_typed_v1";
/// Reads one Unicode scalar and returns a freshly allocated direct Text.
pub const TEXT_GET_TYPED_SYMBOL: &str = "loom_runtime_text_get_typed_v1";
pub const TEXT_GET_TYPED_INVALID: i32 = -1;
pub const TEXT_GET_TYPED_MISSING: i32 = 0;
pub const TEXT_GET_TYPED_FOUND: i32 = 1;
/// Stages two complete immutable byte sequences, then publishes one freshly
/// allocated direct Bytes pointer. Success and defects use the shared `GC_*`
/// status domain.
pub const BYTES_APPEND_TYPED_SYMBOL: &str = "loom_runtime_bytes_append_typed_v1";
/// Copies one immutable byte sequence, appends one checked byte unit, and
/// publishes a direct Bytes pointer. The signed unit is checked defensively by
/// the runtime even though checked LCIR proves `0..=255` before this boundary.
pub const BYTES_PUSH_TYPED_SYMBOL: &str = "loom_runtime_bytes_push_typed_v1";
/// Appends one checked byte unit to a compiler-certified unique Bytes value.
/// Only a distinct `ByteObject` with spare capacity may be updated in place;
/// Text-backed byte views always fall back to a fresh `ByteObject`.
pub const BYTES_PUSH_UNIQUE_TYPED_SYMBOL: &str = "loom_runtime_bytes_push_unique_typed_v1";
/// Validates one immutable byte sequence as UTF-8, then publishes a direct
/// Text pointer on success. Canonical Text-backed input is reused; a distinct
/// `ByteObject` is relabelled in place after validation because immutable Bytes
/// aliases accept either canonical leaf descriptor. `GC_OK` is success,
/// positive `GC_*` values are ABI/runtime defects, and this negative status is
/// the sole ordinary invalid-UTF-8 outcome.
pub const BYTES_DECODE_UTF8_TYPED_SYMBOL: &str = "loom_runtime_bytes_decode_utf8_typed_v1";
pub const BYTES_DECODE_UTF8_TYPED_INVALID_UTF8: i32 = -1;
/// Lexically joins two canonical direct Text values and publishes one direct
/// Text result. A leading `/` in the child is the sole ordinary failure and
/// does not allocate. Success uses `GC_OK`; positive `GC_*` values report ABI
/// or runtime defects.
pub const PATH_JOIN_TYPED_SYMBOL: &str = "loom_runtime_path_join_typed_v1";
pub const PATH_JOIN_TYPED_ABSOLUTE: i32 = -1;
/// Scalar Float parsing shared by both native backends. Naming the symbol and
/// closed status domain here prevents either emitter from inventing an ABI
/// spelling. Integer parsing is ordinary standard-library Loom source and has
/// no runtime boundary.
pub const PARSE_FLOAT_SYMBOL: &str = "loom_runtime_parse_float";
pub const PARSE_FLOAT_STATUS_OK: i32 = 0;
pub const PARSE_FLOAT_STATUS_INVALID_SYNTAX: i32 = 1;
pub const PARSE_FLOAT_STATUS_OUT_OF_RANGE: i32 = 2;
/// Direct File/Socket cleanup taking `(executor, kind, inout token)`.
///
/// This compiler-private boundary never constructs a universal `Value`,
/// schedules a Task, or enters the executor loop. Status zero writes the
/// closed-token sentinel back to the exact source record field. Close is a
/// final RAII release and has no ordinary failure status. The token must have
/// one exact owner in the executor resource ledger. Every nonzero status is an
/// ABI or runtime defect.
pub const TYPED_RESOURCE_CLOSE_SYMBOL: &str = "loom_typed_resource_close_v1";
pub const TYPED_RESOURCE_KIND_FILE: u32 = 1;
pub const TYPED_RESOURCE_KIND_SOCKET: u32 = 2;
pub const TYPED_RESOURCE_CLOSE_OK: i32 = 0;
pub const TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT: i32 = 1;

/// Runtime-owned state bit in [`LoomGcTypedRootFrame::flags`].
///
/// Compiler-generated code must initialize `flags` to zero and must not
/// inspect or modify the field between a successful push and pop.
pub const GC_ROOT_FRAME_LINKED: u32 = 1;

/// Description of precise direct-pointer roots in one typed native frame.
///
/// `live_bitmaps` contains `state_count` rows of exactly `live_bitmap_words`
/// words. Bit `n` identifies entry `n` in the frame's slot-pointer array. Bits
/// beyond `slot_count` in the final word must be zero. Descriptors are
/// runtime-private immutable metadata and must outlive every linked frame
/// which references them. Each root slot is a pointer to a pointer-sized
/// managed-reference cell, so the collector never guesses a representation.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomGcTypedRootDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub slot_count: u64,
    pub state_count: u64,
    pub live_bitmap_words: u64,
    pub live_bitmaps: *const u64,
}

/// Intrusive shadow-stack header for typed direct managed pointers.
///
/// Each entry in `slots` points to writable pointer-sized storage containing
/// only null, the exact base of a runtime-managed typed allocation, or a
/// compiler-proven process-lifetime static/immortal pointer. An interior
/// pointer and any other unregistered finite-lifetime pointer are invalid.
/// Every slot cell address must remain stable from push through pop and must
/// not itself reside in the moving heap. The runtime
/// rewrites managed entries after a moving collection. The pointer array is
/// immutable while linked; `previous` and `flags` are runtime-owned fields.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomGcTypedRootFrame {
    pub abi_version: u32,
    pub flags: u32,
    pub state: u64,
    pub descriptor: *const LoomGcTypedRootDescriptor,
    pub slots: *const *mut core::ffi::c_void,
    pub previous: *mut LoomGcTypedRootFrame,
}

/// Immutable precise trace metadata for one typed managed object shape.
///
/// `fixed_size` is the required pointer-bearing prefix. An allocation may be
/// larger to hold pointer-free trailing storage. `pointer_offsets` is a
/// strictly increasing array of `pointer_count` byte offsets from the object
/// base to aligned pointer-sized managed-reference cells. Each such cell obeys
/// the same null/exact-typed-base/static-immortal target restriction as a typed
/// root. In particular, typed metadata cannot hide an interior or other
/// unregistered finite-lifetime reference. Descriptor identity is
/// compiler/runtime metadata and is not a source-visible type tag.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomGcObjectDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub fixed_size: u64,
    pub object_align: u64,
    pub pointer_count: u64,
    pub pointer_offsets: *const u64,
}

/// Immutable precise trace metadata for a fixed header followed by elements.
///
/// The allocation size is derived from `fixed_size + capacity *
/// element_stride`; it is never trusted from an object field. Fixed pointer
/// offsets are relative to the object base. Element pointer offsets are
/// relative to each element base and are repeated for the allocation capacity.
/// Uninitialized capacity is zero-filled, so tracing it observes only null
/// cells. Both offset tables are copied before the allocation can become
/// visible, and the runtime retains neither caller pointer.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomGcRepeatedObjectDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub fixed_size: u64,
    pub object_align: u64,
    pub fixed_pointer_count: u64,
    pub fixed_pointer_offsets: *const u64,
    pub element_stride: u64,
    pub element_pointer_count: u64,
    pub element_pointer_offsets: *const u64,
}

/// Scheduler-private callback used by a typed stackless coroutine.
///
/// The first two opaque pointers identify the Task and its Executor. The last
/// pointer is the Task's stable, compiler-shaped frame. Callbacks return one
/// of the `TASK_*` step constants. `resume` may return `TASK_PENDING`;
/// `cancel` and `dispose_result` are non-suspending cleanup callbacks and must
/// return a terminal step. The runtime invokes `cancel` exactly once when an
/// initialized frame is retired before normal completion, including an
/// initialized-but-unpublished frame. `dispose_result` runs exactly once only
/// for a published, initialized result that was not transferred to its owner.
/// Neither callback is a GC finalizer. While either cleanup callback is
/// active, task creation/publication, joins, scheduler re-entry, and wait
/// registration/suspension are invalid; fault reporting, precise root
/// operations, GC, root-state publication, and cancellation queries remain
/// available. A well-formed cleanup fault returns `TASK_FAULTED` after
/// recording its fault. Such a fault is suppressed when cancellation already
/// owns the primary outcome, but an invalid step or topology violation is a
/// runtime defect and is never converted to cancellation.
pub type LoomTypedTaskCallback = unsafe extern "C" fn(
    *mut core::ffi::c_void,
    *mut core::ffi::c_void,
    *mut core::ffi::c_void,
) -> i32;

/// Exact physical contract for one compiler-shaped coroutine frame.
///
/// `root_offsets` names pointer-sized managed-reference cells within the
/// frame. `live_bitmaps` has `root_state_count` rows of exactly
/// `root_bitmap_words` words. The runtime copies and validates both arrays at
/// Task creation, so their source storage only needs to remain live for that
/// call. Every live bit in `completed_root_state` must identify a cell wholly
/// inside the result range; other frame cells cease to be roots at completion.
/// The frame address is stable but is exposed only while unpublished and as a
/// callback argument after publication. Results cross the ABI by an exact
/// size/alignment checked move, never through a universal value envelope.
/// Descriptor identity is not language RTTI and is never exposed to Loom
/// source.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomTypedCoroutineDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub resume: Option<LoomTypedTaskCallback>,
    pub cancel: Option<LoomTypedTaskCallback>,
    pub dispose_result: Option<LoomTypedTaskCallback>,
    pub frame_size: u64,
    pub frame_align: u64,
    pub result_offset: u64,
    pub result_size: u64,
    pub result_align: u64,
    pub root_slot_count: u64,
    pub root_state_count: u64,
    pub root_bitmap_words: u64,
    pub root_offsets: *const u64,
    pub live_bitmaps: *const u64,
    pub completed_root_state: u64,
}

/// Borrowed UTF-8 bytes owned by a live typed Task.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomByteView {
    pub data: *const u8,
    pub length: u64,
}

/// One copied request for a typed I/O leaf Task.
///
/// `resource_token` is [`TYPED_IO_INVALID_RESOURCE_TOKEN`] for File open/create
/// and Socket connect. File/Socket read and write carry the exact direct
/// capability token previously published by this executor. The token is never
/// an OS descriptor or handle and is valid only while the current running Task
/// owns its unique runtime ledger entry. `argument` is path, contents, or host
/// according to `operation`; operations without Text require the canonical
/// null/zero view. `auxiliary` is the Socket connect port and is zero for every
/// other operation. The runtime copies this structure and every argument byte
/// during Task creation and retains no caller pointer.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomTypedIoRequest {
    pub abi_version: u32,
    pub operation: u32,
    pub resource_token: u64,
    pub argument: LoomByteView,
    pub auxiliary: i64,
}

/// Primitive completion wire written by [`TYPED_IO_POLL_SYMBOL`].
///
/// This is deliberately not the physical layout of `Result[T, IoError]`.
/// `detail` is the closed `IoErrorKind` index only for Error. `payload` is the
/// capability token for Resource, the closed fault-class value for Error, and
/// zero for Text or Unit. Text and Error publish their sole managed Text
/// through the separate rooted scratch cell.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LoomTypedIoOutcome {
    pub kind: u32,
    pub detail: u32,
    pub payload: u64,
}

/// One borrowed canonical `TextMap[Text]` entry for typed structured logging.
///
/// Both pointers name complete direct Text objects. Generated code passes a
/// view over the map's immutable canonical-order entry storage, not the map
/// object itself, so this wire does not expose or require a universal value.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LoomTypedLogField {
    pub key: *const core::ffi::c_void,
    pub value: *const core::ffi::c_void,
}

impl Default for LoomByteView {
    fn default() -> Self {
        Self {
            data: core::ptr::null(),
            length: 0,
        }
    }
}

/// Borrowed view of the primary fault retained by a typed Task.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct LoomTypedTaskFaultView {
    pub code: LoomByteView,
    pub message: LoomByteView,
    pub detail: LoomByteView,
}

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
/// Runtime layout descriptor referenced by compiler-emitted literal and
/// runtime-allocated managed Text objects.
pub const TEXT_LAYOUT_SYMBOL: &str = "loom_layout_text_v1";
/// Runtime layout descriptor referenced by managed arbitrary Bytes objects.
pub const BYTES_LAYOUT_SYMBOL: &str = "loom_layout_bytes_v1";
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

/// Status domain for typed Task management operations. Coroutine callbacks
/// use the independent `TASK_*` step domain above.
pub const TYPED_TASK_OK: i32 = 0;
pub const TYPED_TASK_INVALID_ARGUMENT: i32 = 1;
pub const TYPED_TASK_NO_MEMORY: i32 = 2;
pub const TYPED_TASK_CLEANUP_FAULTED: i32 = 3;
pub const TYPED_TASK_STATUS_INVALID: i32 = -1;

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
        BYTES_APPEND_TYPED_SYMBOL, BYTES_DECODE_UTF8_TYPED_INVALID_UTF8,
        BYTES_DECODE_UTF8_TYPED_SYMBOL, BYTES_LAYOUT_SYMBOL, BYTES_PUSH_TYPED_SYMBOL,
        BYTES_PUSH_UNIQUE_TYPED_SYMBOL, FORMAT_FLOAT_TYPED_SYMBOL, GC_DESCRIPTOR_INVALID,
        GC_MAX_OBJECT_ALIGNMENT, GC_MAX_OBJECT_BYTES, GC_MAX_OBJECT_POINTERS,
        GC_MAX_REPEATED_POINTER_CELLS, GC_MAX_ROOT_BITMAP_WORDS, GC_MAX_ROOT_DEPTH,
        GC_MAX_ROOT_SLOTS, GC_MAX_ROOT_STATES, GC_RESOURCE_LIMIT, LAYOUT_ABI_VERSION, LoomByteView,
        LoomGcObjectDescriptor, LoomGcRepeatedObjectDescriptor, LoomGcTypedRootDescriptor,
        LoomGcTypedRootFrame, LoomTypedCoroutineDescriptor, LoomTypedIoOutcome, LoomTypedIoRequest,
        LoomTypedLogField, LoomTypedTaskFaultView, NATIVE_RUNTIME_ABI_IDENTITY,
        PARSE_FLOAT_STATUS_INVALID_SYNTAX, PARSE_FLOAT_STATUS_OK, PARSE_FLOAT_STATUS_OUT_OF_RANGE,
        PARSE_FLOAT_SYMBOL, PATH_JOIN_TYPED_ABSOLUTE, PATH_JOIN_TYPED_SYMBOL,
        PROCESS_ARGUMENT_AT_TYPED_SYMBOL, PROCESS_ARGUMENT_COUNT_TYPED_INVALID,
        PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL, PROCESS_ARGUMENT_TYPED_INVALID,
        PROCESS_ARGUMENT_TYPED_OK, PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL,
        PROCESS_ENVIRONMENT_TYPED_FOUND, PROCESS_ENVIRONMENT_TYPED_INVALID,
        PROCESS_ENVIRONMENT_TYPED_MISSING, PROCESS_ENVIRONMENT_TYPED_SYMBOL, RUNTIME_ABI_VERSION,
        STDLIB_ABI_VERSION, STDOUT_WRITE_FAILED, STDOUT_WRITE_INVALID_ARGUMENT, STDOUT_WRITE_OK,
        STDOUT_WRITE_SYMBOL, TEXT_CONTAINS_SYMBOL, TEXT_GET_TYPED_FOUND, TEXT_GET_TYPED_INVALID,
        TEXT_GET_TYPED_MISSING, TEXT_GET_TYPED_SYMBOL, TEXT_LAYOUT_SYMBOL, TEXT_OBJECT_ALIGNMENT,
        TEXT_OBJECT_FIELD_ALLOCATION_SIZE, TEXT_OBJECT_FIELD_BYTE_LENGTH, TEXT_OBJECT_FIELD_BYTES,
        TEXT_OBJECT_FIELD_LAYOUT, TEXT_OBJECT_FIELD_SCALAR_LENGTH, TEXT_OBJECT_HEADER_SIZE,
        TYPED_GC_ABI_VERSION, TYPED_GC_ALLOC_SYMBOL, TYPED_GC_REPEATED_ABI_VERSION,
        TYPED_GC_REPEATED_ALLOC_SYMBOL, TYPED_GC_ROOT_POP_SYMBOL, TYPED_GC_ROOT_PUSH_SYMBOL,
        TYPED_IO_ABI_VERSION, TYPED_IO_CANCEL_SYMBOL, TYPED_IO_FAULT_CLASS_INVALID_PORT,
        TYPED_IO_FAULT_CLASS_OPERATION, TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE,
        TYPED_IO_INVALID_RESOURCE_TOKEN, TYPED_IO_OPERATION_FILE_CREATE,
        TYPED_IO_OPERATION_FILE_OPEN_READ, TYPED_IO_OPERATION_FILE_READ_TEXT,
        TYPED_IO_OPERATION_FILE_WRITE_TEXT, TYPED_IO_OPERATION_SOCKET_CONNECT,
        TYPED_IO_OPERATION_SOCKET_READ_TEXT, TYPED_IO_OPERATION_SOCKET_WRITE_TEXT,
        TYPED_IO_OUTCOME_ERROR, TYPED_IO_OUTCOME_RESOURCE, TYPED_IO_OUTCOME_TEXT,
        TYPED_IO_OUTCOME_UNIT, TYPED_IO_POLL_SYMBOL, TYPED_IO_TASK_CREATE_SYMBOL,
        TYPED_LOG_FIELD_ALIGNMENT, TYPED_LOG_FIELD_KEY_OFFSET, TYPED_LOG_FIELD_SIZE,
        TYPED_LOG_FIELD_VALUE_OFFSET, TYPED_LOG_INVALID_ARGUMENT, TYPED_LOG_OK,
        TYPED_LOG_WRITE_FAILED, TYPED_LOG_WRITE_SYMBOL, TYPED_PROCESS_ABI_VERSION,
        TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT, TYPED_RESOURCE_CLOSE_OK,
        TYPED_RESOURCE_CLOSE_SYMBOL, TYPED_RESOURCE_KIND_FILE, TYPED_RESOURCE_KIND_SOCKET,
        TYPED_SHADOW_STACK_ABI_VERSION, TYPED_TASK_ABI_VERSION, TYPED_TASK_CLEANUP_FAULTED,
        TYPED_TASK_INVALID_ARGUMENT, TYPED_TASK_MAX_FAULT_TEXT_BYTES, TYPED_TASK_NO_MEMORY,
        TYPED_TASK_OK, TYPED_TASK_PUBLISH_ADOPTING_SYMBOL, TYPED_TASK_STATUS_INVALID,
        TYPED_TASK_TAKE_OUTCOME_SYMBOL, TYPED_TIMER_TASK_CREATE_SYMBOL,
    };

    #[test]
    fn native_runtime_identity_is_pinned() {
        assert_eq!(RUNTIME_ABI_VERSION, 40);
        assert_eq!(TYPED_TASK_ABI_VERSION, 1);
        assert_eq!(LAYOUT_ABI_VERSION, 1);
        assert_eq!(TYPED_GC_ABI_VERSION, 1);
        assert_eq!(TYPED_GC_REPEATED_ABI_VERSION, 1);
        assert_eq!(TYPED_SHADOW_STACK_ABI_VERSION, 1);
        assert_eq!(TYPED_GC_ALLOC_SYMBOL, "loom_gc_typed_alloc_v1");
        assert_eq!(
            TYPED_GC_REPEATED_ALLOC_SYMBOL,
            "loom_gc_typed_repeated_alloc_v1"
        );
        assert_eq!(TYPED_GC_ROOT_PUSH_SYMBOL, "loom_gc_typed_root_push_v1");
        assert_eq!(TYPED_GC_ROOT_POP_SYMBOL, "loom_gc_typed_root_pop_v1");
        assert_eq!(
            TYPED_TASK_PUBLISH_ADOPTING_SYMBOL,
            "loom_typed_task_publish_adopting_v1"
        );
        assert_eq!(
            TYPED_TASK_TAKE_OUTCOME_SYMBOL,
            "loom_typed_task_take_outcome_v1"
        );
        assert_eq!(TEXT_GET_TYPED_SYMBOL, "loom_runtime_text_get_typed_v1");
        assert_eq!(
            FORMAT_FLOAT_TYPED_SYMBOL,
            "loom_runtime_format_float_typed_v1"
        );
        assert_eq!(TEXT_GET_TYPED_INVALID, -1);
        assert_eq!(TEXT_GET_TYPED_MISSING, 0);
        assert_eq!(TEXT_GET_TYPED_FOUND, 1);
        assert_eq!(
            BYTES_APPEND_TYPED_SYMBOL,
            "loom_runtime_bytes_append_typed_v1"
        );
        assert_eq!(BYTES_PUSH_TYPED_SYMBOL, "loom_runtime_bytes_push_typed_v1");
        assert_eq!(
            BYTES_PUSH_UNIQUE_TYPED_SYMBOL,
            "loom_runtime_bytes_push_unique_typed_v1"
        );
        assert_eq!(
            BYTES_DECODE_UTF8_TYPED_SYMBOL,
            "loom_runtime_bytes_decode_utf8_typed_v1"
        );
        assert_eq!(BYTES_DECODE_UTF8_TYPED_INVALID_UTF8, -1);
        assert_eq!(PATH_JOIN_TYPED_SYMBOL, "loom_runtime_path_join_typed_v1");
        assert_eq!(PATH_JOIN_TYPED_ABSOLUTE, -1);
        assert_eq!(PARSE_FLOAT_SYMBOL, "loom_runtime_parse_float");
        assert_eq!(PARSE_FLOAT_STATUS_OK, 0);
        assert_eq!(PARSE_FLOAT_STATUS_INVALID_SYNTAX, 1);
        assert_eq!(PARSE_FLOAT_STATUS_OUT_OF_RANGE, 2);
        assert_eq!(STDOUT_WRITE_SYMBOL, "loom_runtime_stdout_write_v1");
        assert_eq!(STDOUT_WRITE_OK, 0);
        assert_eq!(STDOUT_WRITE_INVALID_ARGUMENT, 1);
        assert_eq!(STDOUT_WRITE_FAILED, 2);
        assert_eq!(TYPED_LOG_WRITE_SYMBOL, "loom_runtime_log_typed_v1");
        assert_eq!(TYPED_LOG_OK, 0);
        assert_eq!(TYPED_LOG_INVALID_ARGUMENT, 1);
        assert_eq!(TYPED_LOG_WRITE_FAILED, 2);
        assert_eq!(TYPED_LOG_FIELD_SIZE, 16);
        assert_eq!(TYPED_LOG_FIELD_ALIGNMENT, 8);
        assert_eq!(TYPED_LOG_FIELD_KEY_OFFSET, 0);
        assert_eq!(TYPED_LOG_FIELD_VALUE_OFFSET, 8);
        assert_eq!(TYPED_RESOURCE_CLOSE_SYMBOL, "loom_typed_resource_close_v1");
        assert_eq!(TYPED_RESOURCE_KIND_FILE, 1);
        assert_eq!(TYPED_RESOURCE_KIND_SOCKET, 2);
        assert_eq!(TYPED_RESOURCE_CLOSE_OK, 0);
        assert_eq!(TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT, 1);
        assert_eq!(
            TYPED_TIMER_TASK_CREATE_SYMBOL,
            "loom_typed_timer_task_create_v1"
        );
        assert_eq!(STDLIB_ABI_VERSION, 10);
        assert_eq!(
            NATIVE_RUNTIME_ABI_IDENTITY,
            "layout-v1/text-v4/wait-v1/task-v2/typed-task-v1/typed-task-adopt-v1/typed-task-winner-finalize-v1/typed-task-outcome-v1/typed-resource-ownership-v1/typed-timer-v1/typed-resource-v1/typed-io-v1/format-float-v1/typed-bytes-v2/typed-path-v1/typed-log-v1/stdout-v1/typed-process-v1/runtime-v34/gc-v9/typed-gc-v1/typed-repeated-v1/typed-shadow-stack-v1/stdlib-v10",
        );
    }

    #[test]
    fn typed_process_abi_is_pinned() {
        assert_eq!(TYPED_PROCESS_ABI_VERSION, 1);
        assert_eq!(
            PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL,
            "loom_runtime_process_arguments_initialize_typed_v1"
        );
        assert_eq!(
            PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL,
            "loom_runtime_process_argument_count_typed_v1"
        );
        assert_eq!(PROCESS_ARGUMENT_COUNT_TYPED_INVALID, -1);
        assert_eq!(
            PROCESS_ARGUMENT_AT_TYPED_SYMBOL,
            "loom_runtime_process_argument_at_typed_v1"
        );
        assert_eq!(
            PROCESS_ENVIRONMENT_TYPED_SYMBOL,
            "loom_runtime_process_environment_typed_v1"
        );
        assert_eq!(PROCESS_ARGUMENT_TYPED_OK, 0);
        assert_eq!(PROCESS_ARGUMENT_TYPED_INVALID, 1);
        assert_eq!(PROCESS_ENVIRONMENT_TYPED_INVALID, -1);
        assert_eq!(PROCESS_ENVIRONMENT_TYPED_MISSING, 0);
        assert_eq!(PROCESS_ENVIRONMENT_TYPED_FOUND, 1);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn typed_io_abi_is_pinned() {
        assert_eq!(TYPED_IO_ABI_VERSION, 1);
        assert_eq!(TYPED_IO_TASK_CREATE_SYMBOL, "loom_typed_io_task_create_v1");
        assert_eq!(TYPED_IO_POLL_SYMBOL, "loom_typed_io_poll_v1");
        assert_eq!(TYPED_IO_CANCEL_SYMBOL, "loom_typed_io_cancel_v1");
        assert_eq!(TYPED_IO_OPERATION_FILE_OPEN_READ, 1);
        assert_eq!(TYPED_IO_OPERATION_FILE_CREATE, 2);
        assert_eq!(TYPED_IO_OPERATION_FILE_READ_TEXT, 3);
        assert_eq!(TYPED_IO_OPERATION_FILE_WRITE_TEXT, 4);
        assert_eq!(TYPED_IO_OPERATION_SOCKET_CONNECT, 5);
        assert_eq!(TYPED_IO_OPERATION_SOCKET_READ_TEXT, 6);
        assert_eq!(TYPED_IO_OPERATION_SOCKET_WRITE_TEXT, 7);
        assert_eq!(TYPED_IO_INVALID_RESOURCE_TOKEN, u64::MAX);
        assert_eq!(TYPED_IO_OUTCOME_UNIT, 1);
        assert_eq!(TYPED_IO_OUTCOME_TEXT, 2);
        assert_eq!(TYPED_IO_OUTCOME_RESOURCE, 3);
        assert_eq!(TYPED_IO_OUTCOME_ERROR, 4);
        assert_eq!(TYPED_IO_FAULT_CLASS_OPERATION, 0);
        assert_eq!(TYPED_IO_FAULT_CLASS_INVALID_PORT, 1);
        assert_eq!(TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE, 2);
        assert_eq!(size_of::<LoomTypedIoRequest>(), 40);
        assert_eq!(align_of::<LoomTypedIoRequest>(), 8);
        assert_eq!(offset_of!(LoomTypedIoRequest, abi_version), 0);
        assert_eq!(offset_of!(LoomTypedIoRequest, operation), 4);
        assert_eq!(offset_of!(LoomTypedIoRequest, resource_token), 8);
        assert_eq!(offset_of!(LoomTypedIoRequest, argument), 16);
        assert_eq!(offset_of!(LoomTypedIoRequest, auxiliary), 32);
        assert_eq!(size_of::<LoomTypedIoOutcome>(), 16);
        assert_eq!(align_of::<LoomTypedIoOutcome>(), 8);
        assert_eq!(offset_of!(LoomTypedIoOutcome, kind), 0);
        assert_eq!(offset_of!(LoomTypedIoOutcome, detail), 4);
        assert_eq!(offset_of!(LoomTypedIoOutcome, payload), 8);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn typed_task_layout_is_pinned_for_the_native_64_bit_abi() {
        assert_eq!(TYPED_TASK_OK, 0);
        assert_eq!(TYPED_TASK_INVALID_ARGUMENT, 1);
        assert_eq!(TYPED_TASK_NO_MEMORY, 2);
        assert_eq!(TYPED_TASK_CLEANUP_FAULTED, 3);
        assert_eq!(TYPED_TASK_STATUS_INVALID, -1);
        assert_eq!(TYPED_TASK_MAX_FAULT_TEXT_BYTES, 64 * 1024);
        assert_eq!(size_of::<LoomTypedCoroutineDescriptor>(), 120);
        assert_eq!(align_of::<LoomTypedCoroutineDescriptor>(), 8);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, abi_version), 0);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, flags), 4);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, resume), 8);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, cancel), 16);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, dispose_result), 24);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, frame_size), 32);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, root_offsets), 96);
        assert_eq!(offset_of!(LoomTypedCoroutineDescriptor, live_bitmaps), 104);
        assert_eq!(
            offset_of!(LoomTypedCoroutineDescriptor, completed_root_state),
            112
        );
        assert_eq!(size_of::<LoomByteView>(), 16);
        assert_eq!(align_of::<LoomByteView>(), 8);
        assert_eq!(size_of::<LoomTypedLogField>() as u64, TYPED_LOG_FIELD_SIZE);
        assert_eq!(
            align_of::<LoomTypedLogField>() as u64,
            TYPED_LOG_FIELD_ALIGNMENT
        );
        assert_eq!(
            offset_of!(LoomTypedLogField, key) as u64,
            TYPED_LOG_FIELD_KEY_OFFSET
        );
        assert_eq!(
            offset_of!(LoomTypedLogField, value) as u64,
            TYPED_LOG_FIELD_VALUE_OFFSET
        );
        assert_eq!(size_of::<LoomTypedTaskFaultView>(), 48);
        assert_eq!(align_of::<LoomTypedTaskFaultView>(), 8);
    }

    #[test]
    fn typed_gc_statuses_symbols_and_limits_are_pinned() {
        assert_eq!(GC_DESCRIPTOR_INVALID, 5);
        assert_eq!(GC_RESOURCE_LIMIT, 6);
        assert_eq!(GC_MAX_ROOT_SLOTS, 65_536);
        assert_eq!(GC_MAX_ROOT_STATES, 65_536);
        assert_eq!(GC_MAX_ROOT_BITMAP_WORDS, 1_048_576);
        assert_eq!(GC_MAX_ROOT_DEPTH, 65_536);
        assert_eq!(GC_MAX_OBJECT_POINTERS, 4_096);
        assert_eq!(GC_MAX_REPEATED_POINTER_CELLS, 16_777_216);
        assert_eq!(GC_MAX_OBJECT_BYTES, 1 << 30);
        assert_eq!(GC_MAX_OBJECT_ALIGNMENT, 4_096);
        assert_eq!(TYPED_GC_ALLOC_SYMBOL, "loom_gc_typed_alloc_v1");
        assert_eq!(
            TYPED_GC_REPEATED_ALLOC_SYMBOL,
            "loom_gc_typed_repeated_alloc_v1"
        );
        assert_eq!(TYPED_GC_ROOT_PUSH_SYMBOL, "loom_gc_typed_root_push_v1");
        assert_eq!(TYPED_GC_ROOT_POP_SYMBOL, "loom_gc_typed_root_pop_v1");
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
        assert_eq!(BYTES_LAYOUT_SYMBOL, "loom_layout_bytes_v1");
        assert_eq!(TEXT_CONTAINS_SYMBOL, "loom_runtime_text_contains");
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn typed_gc_layout_is_pinned_for_the_native_64_bit_abi() {
        assert_eq!(size_of::<LoomGcTypedRootDescriptor>(), 40);
        assert_eq!(align_of::<LoomGcTypedRootDescriptor>(), 8);
        assert_eq!(offset_of!(LoomGcTypedRootDescriptor, abi_version), 0);
        assert_eq!(offset_of!(LoomGcTypedRootDescriptor, flags), 4);
        assert_eq!(offset_of!(LoomGcTypedRootDescriptor, slot_count), 8);
        assert_eq!(offset_of!(LoomGcTypedRootDescriptor, state_count), 16);
        assert_eq!(offset_of!(LoomGcTypedRootDescriptor, live_bitmap_words), 24,);
        assert_eq!(offset_of!(LoomGcTypedRootDescriptor, live_bitmaps), 32);

        assert_eq!(size_of::<LoomGcTypedRootFrame>(), 40);
        assert_eq!(align_of::<LoomGcTypedRootFrame>(), 8);
        assert_eq!(offset_of!(LoomGcTypedRootFrame, abi_version), 0);
        assert_eq!(offset_of!(LoomGcTypedRootFrame, flags), 4);
        assert_eq!(offset_of!(LoomGcTypedRootFrame, state), 8);
        assert_eq!(offset_of!(LoomGcTypedRootFrame, descriptor), 16);
        assert_eq!(offset_of!(LoomGcTypedRootFrame, slots), 24);
        assert_eq!(offset_of!(LoomGcTypedRootFrame, previous), 32);

        assert_eq!(size_of::<LoomGcObjectDescriptor>(), 40);
        assert_eq!(align_of::<LoomGcObjectDescriptor>(), 8);
        assert_eq!(offset_of!(LoomGcObjectDescriptor, abi_version), 0);
        assert_eq!(offset_of!(LoomGcObjectDescriptor, flags), 4);
        assert_eq!(offset_of!(LoomGcObjectDescriptor, fixed_size), 8);
        assert_eq!(offset_of!(LoomGcObjectDescriptor, object_align), 16);
        assert_eq!(offset_of!(LoomGcObjectDescriptor, pointer_count), 24);
        assert_eq!(offset_of!(LoomGcObjectDescriptor, pointer_offsets), 32);

        assert_eq!(size_of::<LoomGcRepeatedObjectDescriptor>(), 64);
        assert_eq!(align_of::<LoomGcRepeatedObjectDescriptor>(), 8);
        assert_eq!(offset_of!(LoomGcRepeatedObjectDescriptor, abi_version), 0);
        assert_eq!(offset_of!(LoomGcRepeatedObjectDescriptor, flags), 4);
        assert_eq!(offset_of!(LoomGcRepeatedObjectDescriptor, fixed_size), 8);
        assert_eq!(offset_of!(LoomGcRepeatedObjectDescriptor, object_align), 16);
        assert_eq!(
            offset_of!(LoomGcRepeatedObjectDescriptor, fixed_pointer_count),
            24
        );
        assert_eq!(
            offset_of!(LoomGcRepeatedObjectDescriptor, fixed_pointer_offsets),
            32
        );
        assert_eq!(
            offset_of!(LoomGcRepeatedObjectDescriptor, element_stride),
            40
        );
        assert_eq!(
            offset_of!(LoomGcRepeatedObjectDescriptor, element_pointer_count),
            48
        );
        assert_eq!(
            offset_of!(LoomGcRepeatedObjectDescriptor, element_pointer_offsets),
            56
        );
    }
}
