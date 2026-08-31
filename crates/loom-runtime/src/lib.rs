//! Native runtime linked into LLVM-produced Loom executables.
//!
//! The runtime owns typed moving-GC objects, stackless Tasks, platform waits,
//! and the small process boundary used by the compiler and standard library.

#![deny(unsafe_op_in_unsafe_fn)]
// This crate is a compiler-private C ABI. Exported unsafe entry points are
// documented by loom-runtime-abi, and boxed vectors intentionally provide
// stable scheduler addresses held by generated code.
#![allow(
    clippy::missing_safety_doc,
    clippy::must_use_candidate,
    clippy::struct_excessive_bools,
    clippy::vec_box
)]

mod float;
mod gc;
mod json;
mod logging;
mod output;
mod platform;
mod process;
mod reactor;
mod runtime;
mod scheduler;
mod text;
mod typed_json;

pub use float::format_float_typed_v1;
pub use gc::{
    activate_runtime_v1, deactivate_runtime_v1, safepoint_v1, typed_alloc_v1,
    typed_repeated_alloc_v1, typed_root_pop_v1, typed_root_push_v1,
};
pub use json::{JSON_DEPTH_LIMIT, JsonFormatFailure, JsonNode, escape_json_text, format_json};
pub use logging::log_typed_v1;
pub use output::{stdout_write_v1, write_process_stderr, write_process_stdout};
pub use process::{
    argument_at_typed_v1, argument_count_typed_v1, arguments_initialize_typed_v1,
    environment_typed_v1,
};
pub use reactor::{
    LoomReadyNotification, LoomRegistration, LoomWaitSource, WaitEvent, WaitSet, WaitToken,
    WaitableSource, executor_cancel, executor_create_for_runtime_v1, executor_destroy,
    executor_last_os_error, executor_notify_completion, executor_pop_ready, executor_register,
    executor_wait, wait_now_ns,
};
pub use runtime::{LoomRuntime, runtime_create_v1, runtime_destroy_v1};
pub use scheduler::{
    LoomTask, context_raise_fault_v1, context_raise_fault_with_span_v1, executor_gc_collections,
    executor_gc_live_objects, executor_gc_reclaimed, executor_gc_relocations, executor_live_tasks,
    executor_run, executor_tasks_reclaimed, task_add_join_child, task_join_step, task_join_winner,
    task_prepare_join, task_report_fault, task_suspend_join, typed_io_cancel_v1, typed_io_poll_v1,
    typed_io_task_create_v1, typed_resource_close_v1, typed_task_abort_unpublished_v1,
    typed_task_create_v1, typed_task_fault_view_v1, typed_task_frame_v1, typed_task_initialize_v1,
    typed_task_is_cancel_requested_v1, typed_task_publish_adopting_v1,
    typed_task_publish_result_v1, typed_task_publish_v1, typed_task_record_fault_v1,
    typed_task_request_cancel_v1, typed_task_set_root_state_v1, typed_task_status_v1,
    typed_task_take_outcome_v1, typed_task_take_result_v1, typed_timer_task_create_v1,
};
pub use text::{
    bytes_append_typed_v1, bytes_decode_utf8_typed_v1, concat_typed_v1, from_utf8_units_typed_v1,
    get_typed_v1, path_join_typed_v1, text_contains,
};
pub use typed_json::json_format_typed_v1;

pub use loom_runtime_abi::*;

pub const WAIT_INFINITE: u64 = u64::MAX;
