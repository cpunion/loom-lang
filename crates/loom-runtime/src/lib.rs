//! Native runtime linked into LLVM-produced Loom executables.
//!
//! The scheduler, structured Task tree, joins, cancellation and managed frame
//! relocation are ordinary Rust. Unsafe code is confined to the private C ABI
//! and platform handle conversions whose lifetimes are represented by runtime
//! registration or scoped-resource entries.

#![deny(unsafe_op_in_unsafe_fn)]
// This crate is a compiler-private C ABI. Its exported unsafe entry points are
// documented at the ABI level, and the boxed vectors intentionally provide
// stable addresses for pointers held by generated code.
#![allow(
    clippy::missing_safety_doc,
    clippy::must_use_candidate,
    clippy::struct_excessive_bools,
    clippy::vec_box
)]

mod float;
mod gc;
mod int;
mod int_list;
mod platform;
mod process;
mod reactor;
mod runtime;
mod scheduler;
mod standard;
mod text;
mod value;
mod witness;

pub use gc::{
    activate_runtime_v1, build_value_nodes_v1, clone_value_v1, clone_witness_v1,
    deactivate_runtime_v1, root_pop_v1, root_push_v1, safepoint_v1, typed_alloc_v1,
    typed_root_pop_v1, typed_root_push_v1,
};
pub use int_list::{LoomIntListStorage, int_list_drop, int_list_reserve};
pub use value::value_summary;

pub use reactor::{
    LoomReadyNotification, LoomRegistration, LoomWaitSource, WaitEvent, WaitSet, WaitToken,
    WaitableSource, executor_cancel, executor_create_for_runtime_v1, executor_destroy,
    executor_last_os_error, executor_notify_completion, executor_pop_ready, executor_register,
    executor_wait, wait_now_ns,
};
pub use runtime::{LoomRuntime, runtime_create_v1, runtime_destroy_v1};
pub use scheduler::{
    LoomCoroutineDescriptor, LoomJoinSpec, LoomTask, LoomTaskCancel, LoomTaskResume, LoomTaskTrace,
    LoomTraceVisitor, context_raise_fault_v1, context_raise_fault_with_span_v1,
    executor_gc_collections, executor_gc_live_objects, executor_gc_reclaimed,
    executor_gc_relocations, executor_live_tasks, executor_run, executor_tasks_reclaimed,
    file_try_create, file_try_open_read, file_try_read_text, file_try_write_text, join_add_list,
    join_add_task, join_create, join_task, socket_try_connect, socket_try_read_text,
    socket_try_write_text, task_add_join_child, task_cancel, task_capture_witnesses_v1,
    task_from_wait_source, task_is_cancelled, task_join_count, task_join_result,
    task_join_result_step, task_join_step, task_join_winner, task_prepare_join, task_report_fault,
    task_result, task_set_fault, task_set_state, task_slot, task_spawn, task_spawn_descriptor,
    task_suspend_join, task_suspend_value, task_suspend_wait, task_trace_live_slots,
    task_witness_v1, task_write_join_result, typed_task_abort_unpublished_v1, typed_task_create_v1,
    typed_task_fault_view_v1, typed_task_frame_v1, typed_task_initialize_v1,
    typed_task_is_cancel_requested_v1, typed_task_publish_result_v1, typed_task_publish_v1,
    typed_task_record_fault_v1, typed_task_request_cancel_v1, typed_task_set_root_state_v1,
    typed_task_status_v1, typed_task_take_result_v1,
};
pub use standard::{
    JSON_DEPTH_LIMIT, JsonFailure, JsonFailureKind, JsonNode, bytes_append, bytes_decode_utf8,
    bytes_get, bytes_is_utf8, escape_json_text, format_json, json_format, json_parse, log_write,
    parse_json, path_contains_nul, path_join, text_concat, text_contains, text_get, text_length,
    text_map_get, text_map_insert, text_map_remove,
};
pub use text::concat_typed_v1;

pub use loom_runtime_abi::{
    COROUTINE_ABI_VERSION, DYN_FLAG_MUTABLE, FAULT_FORMAT_ENV, FAULT_FORMAT_JSON,
    FAULT_JSON_PREFIX, FAULT_SCHEMA_VERSION, GC_ABI_MISMATCH, GC_DESCRIPTOR_INVALID,
    GC_FRAME_ORDER, GC_INVALID_ARGUMENT, GC_MAX_OBJECT_ALIGNMENT, GC_MAX_OBJECT_BYTES,
    GC_MAX_OBJECT_POINTERS, GC_MAX_ROOT_BITMAP_WORDS, GC_MAX_ROOT_DEPTH, GC_MAX_ROOT_SLOTS,
    GC_MAX_ROOT_STATES, GC_OK, GC_RESOURCE_LIMIT, GC_ROOT_FRAME_LINKED, GC_ROOT_STACK_NOT_EMPTY,
    LAYOUT_ABI_VERSION, LAYOUT_FLAG_LEAF, LAYOUT_FLAG_MANAGED_POINTER, LAYOUT_FLAG_TRAILING_BYTES,
    LAYOUT_KIND_BYTES, LAYOUT_KIND_TEXT, LoomByteView, LoomGcObjectDescriptor,
    LoomGcRootDescriptor, LoomGcRootFrame, LoomGcTypedRootDescriptor, LoomGcTypedRootFrame,
    LoomLayoutDescriptor, LoomTypedCoroutineDescriptor, LoomTypedTaskCallback,
    LoomTypedTaskFaultView, LoomWitnessDescriptor, LoomWitnessInstance,
    NATIVE_RUNTIME_ABI_IDENTITY, READY_CLOSED, READY_COMPLETED, READY_ERROR, READY_READABLE,
    READY_TIMER, READY_WRITABLE, RUNTIME_ABI_VERSION, SHADOW_STACK_ABI_VERSION,
    STANDARD_LIBRARY_ABI_VERSION, TASK_CANCELLED, TASK_COMPLETED, TASK_FAULTED, TASK_JOIN_ALL,
    TASK_JOIN_ANY, TASK_JOIN_RACE, TASK_JOIN_SETTLED, TASK_PENDING, TYPED_GC_ABI_VERSION,
    TYPED_GC_ALLOC_SYMBOL, TYPED_GC_ROOT_POP_SYMBOL, TYPED_GC_ROOT_PUSH_SYMBOL,
    TYPED_SHADOW_STACK_ABI_VERSION, TYPED_TASK_ABI_VERSION, TYPED_TASK_CLEANUP_FAULTED,
    TYPED_TASK_INVALID_ARGUMENT, TYPED_TASK_MAX_FAULT_TEXT_BYTES, TYPED_TASK_NO_MEMORY,
    TYPED_TASK_OK, TYPED_TASK_STATUS_INVALID, WAIT_ABI_VERSION, WAIT_DUPLICATE_SOURCE,
    WAIT_INVALID_ARGUMENT, WAIT_NO_MEMORY, WAIT_OK, WAIT_READABLE, WAIT_SOURCE_COMPLETION,
    WAIT_SOURCE_IO, WAIT_SOURCE_TIMER, WAIT_STALE_REGISTRATION, WAIT_SYSTEM_ERROR,
    WAIT_UNSUPPORTED, WAIT_WRITABLE, WITNESS_ABI_VERSION,
};

pub const WAIT_INFINITE: u64 = u64::MAX;

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::io::Write;
    use std::mem::{align_of, offset_of, size_of};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::reactor::LoomExecutor;

    fn source(kind: u32) -> LoomWaitSource {
        LoomWaitSource {
            abi_version: WAIT_ABI_VERSION,
            kind,
            handle: -1,
            ..LoomWaitSource::default()
        }
    }

    static VALUE_RELOCATED: AtomicBool = AtomicBool::new(false);
    static DESCRIPTOR_CANCELLED: AtomicBool = AtomicBool::new(false);
    const TASK_BATCH_SIZE: usize = 512;

    fn socket_pair() -> std::io::Result<(TcpStream, TcpStream)> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let client = TcpStream::connect(address)?;
        let (server, _) = listener.accept()?;
        Ok((client, server))
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn coroutine_descriptor_layout_includes_independent_witness_slots() {
        assert_eq!(size_of::<LoomCoroutineDescriptor>(), 80);
        assert_eq!(align_of::<LoomCoroutineDescriptor>(), 8);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, abi_version), 0);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, flags), 4);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, resume), 8);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, cancel), 16);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, trace), 24);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, slot_count), 32);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, witness_count), 40);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, result_slot), 48);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, state_count), 56);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, live_bitmap_words), 64);
        assert_eq!(offset_of!(LoomCoroutineDescriptor, live_bitmaps), 72);
    }

    unsafe extern "C" fn gc_fixture_resume(
        task: *mut LoomTask,
        executor: *mut LoomExecutor,
    ) -> i32 {
        let slot = unsafe { task_slot(task, 0).cast::<scheduler::ValueSlot>() };
        match unsafe { scheduler::task_state(task) } {
            0 => {
                let _unreachable = gc::allocate_value();
                let live = gc::allocate_value().cast::<scheduler::ValueSlot>();
                unsafe {
                    (*slot).words[0] = 7;
                    (*slot).words[3] = live as u64;
                    (*slot).words[4] = live as u64;
                }
                let wait = LoomWaitSource {
                    abi_version: WAIT_ABI_VERSION,
                    kind: WAIT_SOURCE_TIMER,
                    handle: -1,
                    deadline_ns: wait_now_ns().saturating_add(1_000_000),
                    ..LoomWaitSource::default()
                };
                if unsafe { task_suspend_wait(executor, task, &raw const wait) } != WAIT_OK {
                    return TASK_FAULTED;
                }
                unsafe { task_set_state(task, 1) };
                TASK_PENDING
            }
            1 => {
                let old = unsafe { (*slot).words[3] };
                let current = unsafe { (*slot).words[4] };
                VALUE_RELOCATED.store(old != current, Ordering::SeqCst);
                unsafe { slot.write(scheduler::ValueSlot::default()) };
                TASK_COMPLETED
            }
            _ => TASK_FAULTED,
        }
    }

    unsafe extern "C" fn completed_child_resume(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_COMPLETED
    }

    unsafe extern "C" fn trace_managed_sequence_slots(
        task: *mut LoomTask,
        visitor: Option<LoomTraceVisitor>,
        context: *mut c_void,
    ) {
        if let Some(visitor) = visitor {
            unsafe { visitor(task_slot(task, 1), context) };
            unsafe { visitor(task_slot(task, 2), context) };
        }
    }

    unsafe extern "C" fn descriptor_cancel(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        DESCRIPTOR_CANCELLED.store(true, Ordering::SeqCst);
        TASK_CANCELLED
    }

    unsafe extern "C" fn task_batch_resume(
        task: *mut LoomTask,
        executor: *mut LoomExecutor,
    ) -> i32 {
        match unsafe { scheduler::task_state(task) } {
            0 => {
                if unsafe { task_prepare_join(executor, task, TASK_JOIN_ALL) } != WAIT_OK {
                    return TASK_FAULTED;
                }
                for _ in 0..TASK_BATCH_SIZE {
                    let child = unsafe { task_spawn(executor, Some(completed_child_resume), 1, 0) };
                    if child.is_null()
                        || unsafe { task_add_join_child(executor, task, child) } != WAIT_OK
                    {
                        return TASK_FAULTED;
                    }
                }
                unsafe { task_set_state(task, 1) };
                if unsafe { task_suspend_join(executor, task) } == 1 {
                    TASK_PENDING
                } else {
                    TASK_FAULTED
                }
            }
            1 => {
                let destination = unsafe { task_slot(task, 1) };
                if unsafe { task_write_join_result(task, destination, 2) } == WAIT_OK {
                    TASK_COMPLETED
                } else {
                    TASK_FAULTED
                }
            }
            _ => TASK_FAULTED,
        }
    }

    unsafe extern "C" fn blocking_race_resume(
        task: *mut LoomTask,
        executor: *mut LoomExecutor,
    ) -> i32 {
        match unsafe { scheduler::task_state(task) } {
            0 => {
                let slow = unsafe { scheduler::spawn_slow_blocking_fixture(executor) };
                let timer_source = LoomWaitSource {
                    abi_version: WAIT_ABI_VERSION,
                    kind: WAIT_SOURCE_TIMER,
                    handle: -1,
                    deadline_ns: wait_now_ns().saturating_add(2_000_000),
                    ..LoomWaitSource::default()
                };
                let timer = unsafe { task_from_wait_source(executor, &raw const timer_source) };
                if slow.is_null()
                    || timer.is_null()
                    || unsafe { task_prepare_join(executor, task, TASK_JOIN_RACE) } != WAIT_OK
                    || unsafe { task_add_join_child(executor, task, slow) } != WAIT_OK
                    || unsafe { task_add_join_child(executor, task, timer) } != WAIT_OK
                {
                    return TASK_FAULTED;
                }
                unsafe { task_set_state(task, 1) };
                if unsafe { task_suspend_join(executor, task) } == 1 {
                    TASK_PENDING
                } else {
                    TASK_FAULTED
                }
            }
            1 => {
                if unsafe { task_join_winner(task) } != 1 {
                    return TASK_FAULTED;
                }
                let destination = unsafe { task_slot(task, 1) };
                if unsafe { task_write_join_result(task, destination, 3) } == WAIT_OK {
                    TASK_COMPLETED
                } else {
                    TASK_FAULTED
                }
            }
            _ => TASK_FAULTED,
        }
    }

    #[test]
    fn real_completion_timer_and_io_are_one_shot() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let mut frame = 1_i32;
        let frame_pointer = (&raw mut frame).cast::<c_void>();

        let completion = source(WAIT_SOURCE_COMPLETION);
        let mut registration = LoomRegistration::default();
        // SAFETY: all stack values outlive their one-shot registrations.
        unsafe {
            assert_eq!(
                executor_register(
                    executor,
                    &raw const completion,
                    frame_pointer,
                    &raw mut registration,
                ),
                WAIT_OK
            );
            assert_eq!(
                executor_notify_completion(executor, &raw const registration, 0, 0),
                WAIT_OK
            );
            let mut ready_count = 0;
            assert_eq!(executor_wait(executor, 0, &raw mut ready_count), WAIT_OK);
            assert_eq!(ready_count, 1);
            let mut ready = LoomReadyNotification::default();
            assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
            assert_eq!(ready.frame, frame_pointer);
            assert_ne!(ready.events & READY_COMPLETED, 0);
            assert_eq!(
                executor_notify_completion(executor, &raw const registration, 0, 0),
                WAIT_STALE_REGISTRATION
            );

            let mut timer = source(WAIT_SOURCE_TIMER);
            timer.deadline_ns = wait_now_ns() + 1_000_000;
            assert_eq!(
                executor_register(
                    executor,
                    &raw const timer,
                    frame_pointer,
                    &raw mut registration,
                ),
                WAIT_OK
            );
            assert_eq!(
                executor_wait(executor, 2_000_000_000, &raw mut ready_count),
                WAIT_OK
            );
            assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
            assert_ne!(ready.events & READY_TIMER, 0);

            let (left, mut right) = socket_pair().expect("create socket pair");
            left.set_nonblocking(true).expect("make socket nonblocking");
            let mut readable = source(WAIT_SOURCE_IO);
            readable.handle = crate::platform::socket_handle_bits(&left);
            readable.interests = WAIT_READABLE;
            assert_eq!(
                executor_register(
                    executor,
                    &raw const readable,
                    frame_pointer,
                    &raw mut registration,
                ),
                WAIT_OK
            );
            right.write_all(b"x").expect("make socket readable");
            assert_eq!(
                executor_wait(executor, 2_000_000_000, &raw mut ready_count),
                WAIT_OK
            );
            assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
            assert_ne!(ready.events & READY_READABLE, 0);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn moving_heap_rewrites_roots_and_reclaims_unreachable_values() {
        VALUE_RELOCATED.store(false, Ordering::SeqCst);
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe { (*runtime).heap.collect_on_every_poll = true };
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let task = unsafe { task_spawn(executor, Some(gc_fixture_resume), 1, 0) };
        assert!(!task.is_null());
        assert_eq!(unsafe { executor_run(executor, task) }, TASK_COMPLETED);
        assert!(VALUE_RELOCATED.load(Ordering::SeqCst));
        assert!(unsafe { executor_gc_relocations(executor) } >= 1);
        assert!(unsafe { executor_gc_reclaimed(executor) } >= 1);
        // The completed callback cleared its last object root. A final
        // scheduler safepoint proves the survivor can be reclaimed too.
        unsafe { gc::collect(&mut *executor) };
        assert_eq!(unsafe { executor_gc_live_objects(executor) }, 0);
        unsafe {
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn public_gc_live_object_count_includes_owned_witness_proofs() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let descriptor = LoomWitnessDescriptor {
            prerequisite_count: 0,
            method_count: 0,
            methods: std::ptr::null(),
        };
        let source = LoomWitnessInstance {
            descriptor: &raw const descriptor,
            prerequisites: std::ptr::null(),
        };
        unsafe {
            let task = task_spawn(executor, Some(completed_child_resume), 1, 0);
            assert!(!task.is_null());
            gc::enter_executor(executor);
            let data = gc::allocate_value().cast::<scheduler::ValueSlot>();
            let witness = clone_witness_v1(&raw const source);
            gc::leave_executor();
            assert!(!data.is_null() && !witness.is_null());
            let root = task_slot(task, 0).cast::<scheduler::ValueSlot>();
            (*root).words[loom_runtime_abi::VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_DYN;
            (*root).words[loom_runtime_abi::VALUE_WORD_DATA] = data as u64;
            (*root).words[loom_runtime_abi::VALUE_WORD_WITNESS] = witness as u64;

            gc::collect(&mut *executor);
            assert_eq!(executor_gc_live_objects(executor), 2);
            root.write(scheduler::ValueSlot::default());
            gc::collect(&mut *executor);
            assert_eq!(executor_gc_live_objects(executor), 0);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn borrowed_executor_collects_into_its_runtime_heap_and_reports_stats() {
        VALUE_RELOCATED.store(false, Ordering::SeqCst);
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe { (*runtime).heap.collect_on_every_poll = true };
        let heap = unsafe { &raw mut (*runtime).heap };
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let task = unsafe { task_spawn(executor, Some(gc_fixture_resume), 1, 0) };
        assert!(!task.is_null());
        assert_eq!(unsafe { executor_run(executor, task) }, TASK_COMPLETED);
        assert!(VALUE_RELOCATED.load(Ordering::SeqCst));
        assert!(unsafe { executor_gc_collections(executor) } >= 1);
        assert!(unsafe { executor_gc_relocations(executor) } >= 1);
        assert!(unsafe { executor_gc_reclaimed(executor) } >= 1);
        unsafe {
            gc::collect(&mut *executor);
            assert_eq!(executor_gc_live_objects(executor), 0);
            assert_eq!(&raw mut (*runtime).heap, heap);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn moving_heap_rewrites_managed_text_and_bytes_as_whole_objects() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let descriptor = LoomCoroutineDescriptor {
            abi_version: COROUTINE_ABI_VERSION,
            flags: 0,
            resume: Some(completed_child_resume),
            cancel: None,
            trace: Some(trace_managed_sequence_slots),
            slot_count: 3,
            witness_count: 0,
            result_slot: 0,
            state_count: 0,
            live_bitmap_words: 0,
            live_bitmaps: std::ptr::null(),
        };
        unsafe {
            let task = task_spawn_descriptor(executor, &raw const descriptor);
            assert!(!task.is_null());
            gc::enter_executor(executor);
            let dead = gc::text_value(b"dead").unwrap();
            let value = gc::text_value("moving 界🙂".as_bytes()).unwrap();
            let byte_value = gc::byte_value(&[0xff, 0]).unwrap();
            gc::leave_executor();
            task_slot(task, 0)
                .cast::<scheduler::ValueSlot>()
                .write(dead);
            let slot = task_slot(task, 1).cast::<scheduler::ValueSlot>();
            slot.write(value);
            let byte_slot = task_slot(task, 2).cast::<scheduler::ValueSlot>();
            byte_slot.write(byte_value);
            let old_object = (*slot).words[loom_runtime_abi::VALUE_WORD_DATA];
            let old_byte_object = (*byte_slot).words[loom_runtime_abi::VALUE_WORD_DATA];

            gc::collect(&mut *executor);

            assert_ne!((*slot).words[loom_runtime_abi::VALUE_WORD_DATA], old_object);
            assert_eq!(
                text::text_value_bytes(&*slot),
                Some("moving 界🙂".as_bytes()),
            );
            assert_ne!(
                (*byte_slot).words[loom_runtime_abi::VALUE_WORD_DATA],
                old_byte_object,
            );
            assert_eq!(text::byte_value_bytes(&*byte_slot), Some(&[0xff, 0][..]));
            for index in [1, 2, 3, 5] {
                assert_eq!((*slot).words[index], 0);
            }
            assert_eq!(executor_gc_live_objects(executor), 2);
            assert!(executor_gc_reclaimed(executor) >= 1);

            slot.write(scheduler::ValueSlot::default());
            byte_slot.write(scheduler::ValueSlot::default());
            gc::collect(&mut *executor);
            assert_eq!(executor_gc_live_objects(executor), 0);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn longer_aggregate_view_marks_the_tail_of_an_already_seen_head() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let bitmap = [0b11_u64];
        let descriptor = LoomCoroutineDescriptor {
            abi_version: COROUTINE_ABI_VERSION,
            flags: 0,
            resume: Some(completed_child_resume),
            cancel: None,
            trace: None,
            slot_count: 2,
            witness_count: 0,
            result_slot: 0,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        unsafe {
            let task = task_spawn_descriptor(executor, &raw const descriptor);
            assert!(!task.is_null());
            gc::enter_executor(executor);
            let head = gc::allocate_value_node().cast::<scheduler::ValueNode>();
            let tail = gc::allocate_value_node().cast::<scheduler::ValueNode>();
            gc::leave_executor();
            (*head).next = tail;
            (*tail).next = std::ptr::null_mut();

            let first = task_slot(task, 0).cast::<scheduler::ValueSlot>();
            (*first).words[loom_runtime_abi::VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_RECORD;
            (*first).words[loom_runtime_abi::VALUE_WORD_AUX] = 1;
            (*first).words[loom_runtime_abi::VALUE_WORD_DATA] = head as u64;
            let longer = task_slot(task, 1).cast::<scheduler::ValueSlot>();
            *longer = *first;
            (*longer).words[loom_runtime_abi::VALUE_WORD_AUX] = 2;

            gc::collect(&mut *executor);

            let moved_head =
                (*longer).words[loom_runtime_abi::VALUE_WORD_DATA] as *mut scheduler::ValueNode;
            assert_ne!(moved_head, head);
            assert_eq!(
                (*first).words[loom_runtime_abi::VALUE_WORD_DATA],
                moved_head as u64,
            );
            assert!(!(*moved_head).next.is_null());
            assert_ne!((*moved_head).next, tail);
            assert_eq!(executor_gc_live_objects(executor), 2);

            first.write(scheduler::ValueSlot::default());
            longer.write(scheduler::ValueSlot::default());
            gc::collect(&mut *executor);
            assert_eq!(executor_gc_live_objects(executor), 0);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn list_indexes_are_head_local_and_rebuild_after_heap_relocation() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let bitmap = [0b11_u64];
        let descriptor = LoomCoroutineDescriptor {
            abi_version: COROUTINE_ABI_VERSION,
            flags: 0,
            resume: Some(completed_child_resume),
            cancel: None,
            trace: None,
            slot_count: 2,
            witness_count: 0,
            result_slot: 0,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        unsafe {
            let task = task_spawn_descriptor(executor, &raw const descriptor);
            assert!(!task.is_null());
            let source = task_slot(task, 0).cast::<scheduler::ValueSlot>();
            let copied = task_slot(task, 1).cast::<scheduler::ValueSlot>();
            (*source).words[loom_runtime_abi::VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_LIST;
            (*copied).words[loom_runtime_abi::VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_LIST;

            gc::enter_executor(executor);
            for number in 0..128_i64 {
                let mut value = scheduler::ValueSlot::default();
                value.words[loom_runtime_abi::VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_INT;
                value.words[loom_runtime_abi::VALUE_WORD_SCALAR] = number.cast_unsigned();
                assert_eq!(gc::list_add(source, &raw const value), 0);
                assert_eq!(gc::list_add(copied, &raw const value), 0);
            }
            assert_ne!(
                (*source).words[loom_runtime_abi::VALUE_WORD_DATA],
                (*copied).words[loom_runtime_abi::VALUE_WORD_DATA],
            );
            assert_eq!((*executor).heap().list_node_indexes.len(), 2);
            assert!(
                (*executor)
                    .heap()
                    .list_node_indexes
                    .values()
                    .all(|entry| entry.length == 128 && entry.nodes.is_none())
            );
            for index in (0..128_i64).rev() {
                let mut source_value = scheduler::ValueSlot::default();
                let mut copied_value = scheduler::ValueSlot::default();
                assert_eq!(gc::list_get(source, index, &raw mut source_value), 1);
                assert_eq!(gc::list_get(copied, index, &raw mut copied_value), 1);
                assert_eq!(
                    source_value.words[loom_runtime_abi::VALUE_WORD_SCALAR],
                    index.cast_unsigned(),
                );
                assert_eq!(source_value.words, copied_value.words);
            }
            assert!(
                (*executor)
                    .heap()
                    .list_node_indexes
                    .values()
                    .all(|entry| { entry.nodes.as_ref().is_some_and(|nodes| nodes.len() == 128) })
            );
            gc::leave_executor();

            let old_source_head = (*source).words[loom_runtime_abi::VALUE_WORD_DATA];
            let old_copied_head = (*copied).words[loom_runtime_abi::VALUE_WORD_DATA];
            gc::collect(&mut *executor);
            assert!((*executor).heap().list_node_indexes.is_empty());
            assert_ne!(
                (*source).words[loom_runtime_abi::VALUE_WORD_DATA],
                old_source_head,
            );
            assert_ne!(
                (*copied).words[loom_runtime_abi::VALUE_WORD_DATA],
                old_copied_head,
            );

            gc::enter_executor(executor);
            let mut last_source = scheduler::ValueSlot::default();
            let mut first_copied = scheduler::ValueSlot::default();
            assert_eq!(gc::list_get(source, 127, &raw mut last_source), 1);
            assert_eq!(gc::list_get(copied, 0, &raw mut first_copied), 1);
            assert_eq!(last_source.words[loom_runtime_abi::VALUE_WORD_SCALAR], 127,);
            assert_eq!(first_copied.words[loom_runtime_abi::VALUE_WORD_SCALAR], 0,);
            assert_eq!((*executor).heap().list_node_indexes.len(), 2);
            let mut extra = scheduler::ValueSlot::default();
            extra.words[loom_runtime_abi::VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_INT;
            extra.words[loom_runtime_abi::VALUE_WORD_SCALAR] = 999;
            assert_eq!(gc::list_add(copied, &raw const extra), 0);
            assert_eq!((*source).words[loom_runtime_abi::VALUE_WORD_AUX], 128);
            assert_eq!((*copied).words[loom_runtime_abi::VALUE_WORD_AUX], 129);
            let mut appended = scheduler::ValueSlot::default();
            assert_eq!(gc::list_get(copied, 128, &raw mut appended), 1);
            assert_eq!(appended.words[loom_runtime_abi::VALUE_WORD_SCALAR], 999,);
            assert_eq!(
                (*executor)
                    .heap()
                    .list_node_indexes
                    .get(
                        &usize::try_from((*copied).words[loom_runtime_abi::VALUE_WORD_DATA],)
                            .unwrap(),
                    )
                    .and_then(|entry| entry.nodes.as_ref())
                    .map(Vec::len),
                Some(129),
            );
            let mut missing = scheduler::ValueSlot::default();
            assert_eq!(gc::list_get(source, 128, &raw mut missing), 0);
            gc::leave_executor();

            source.write(scheduler::ValueSlot::default());
            copied.write(scheduler::ValueSlot::default());
            gc::collect(&mut *executor);
            assert!((*executor).heap().list_node_indexes.is_empty());
            assert_eq!(executor_gc_live_objects(executor), 0);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn task_capture_owns_compact_conditional_proof_dag() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let leaf_descriptor = LoomWitnessDescriptor {
            prerequisite_count: 0,
            method_count: 0,
            methods: std::ptr::null(),
        };
        let applied_descriptor = LoomWitnessDescriptor {
            prerequisite_count: 1,
            method_count: 0,
            methods: std::ptr::null(),
        };
        let descriptor = LoomCoroutineDescriptor {
            abi_version: COROUTINE_ABI_VERSION,
            flags: 0,
            resume: Some(completed_child_resume),
            cancel: None,
            trace: None,
            slot_count: 1,
            witness_count: 1,
            result_slot: 0,
            state_count: 0,
            live_bitmap_words: 0,
            live_bitmaps: std::ptr::null(),
        };
        unsafe {
            let task = task_spawn_descriptor(executor, &raw const descriptor);
            assert!(!task.is_null());
            let invalid = LoomWitnessInstance {
                descriptor: &raw const applied_descriptor,
                prerequisites: std::ptr::null(),
            };
            let invalid_roots = [&raw const invalid];
            assert_eq!(
                task_capture_witnesses_v1(task, invalid_roots.as_ptr(), 1),
                WAIT_INVALID_ARGUMENT,
            );
            assert!(task_witness_v1(task, 0).is_null());
            let source_root = {
                let mut leaf = LoomWitnessInstance {
                    descriptor: &raw const leaf_descriptor,
                    prerequisites: std::ptr::null(),
                };
                let prerequisites = [&raw const leaf];
                let mut applied = LoomWitnessInstance {
                    descriptor: &raw const applied_descriptor,
                    prerequisites: prerequisites.as_ptr(),
                };
                let roots = [&raw const applied];
                assert_eq!(task_capture_witnesses_v1(task, roots.as_ptr(), 1), WAIT_OK,);
                let captured = task_witness_v1(task, 0);
                assert!(!captured.is_null());
                assert_ne!(captured, &raw const applied);
                applied.descriptor = std::ptr::null();
                leaf.descriptor = std::ptr::null();
                assert!(applied.descriptor.is_null());
                assert!(leaf.descriptor.is_null());
                captured
            };

            assert_eq!((*source_root).descriptor, &raw const applied_descriptor);
            let prerequisite = *(*source_root).prerequisites;
            assert!(!prerequisite.is_null());
            assert_eq!((*prerequisite).descriptor, &raw const leaf_descriptor);
            assert!(task_witness_v1(task, 1).is_null());
            assert_eq!(
                task_capture_witnesses_v1(task, std::ptr::null(), 0),
                WAIT_INVALID_ARGUMENT,
            );
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn coroutine_descriptor_drives_cancel_and_precise_gc_roots() {
        DESCRIPTOR_CANCELLED.store(false, Ordering::SeqCst);
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let bitmap = [1_u64];
        let descriptor = LoomCoroutineDescriptor {
            abi_version: COROUTINE_ABI_VERSION,
            flags: 0,
            resume: Some(completed_child_resume),
            cancel: Some(descriptor_cancel),
            trace: None,
            slot_count: 3,
            witness_count: 0,
            result_slot: 2,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        unsafe {
            let task = task_spawn_descriptor(executor, &raw const descriptor);
            assert!(!task.is_null());
            gc::enter_executor(executor);
            let live = gc::allocate_value().cast::<scheduler::ValueSlot>();
            let dead = gc::allocate_value().cast::<scheduler::ValueSlot>();
            gc::leave_executor();
            let live_slot = task_slot(task, 0).cast::<scheduler::ValueSlot>();
            let dead_slot = task_slot(task, 1).cast::<scheduler::ValueSlot>();
            (*live_slot).words[0] = 7;
            (*live_slot).words[4] = live as u64;
            (*dead_slot).words[0] = 7;
            (*dead_slot).words[4] = dead as u64;
            gc::collect(&mut *executor);
            assert_eq!(executor_gc_live_objects(executor), 1);
            assert_ne!((*live_slot).words[4], live as u64);

            assert_eq!(task_cancel(executor, task), WAIT_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert!(DESCRIPTOR_CANCELLED.load(Ordering::SeqCst));
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }

        let incompatible = LoomCoroutineDescriptor {
            abi_version: COROUTINE_ABI_VERSION + 1,
            ..descriptor
        };
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(unsafe { task_spawn_descriptor(executor, &raw const incompatible) }.is_null());
        unsafe {
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn consumed_task_batches_are_reclaimed_at_the_next_safepoint() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        unsafe {
            let batch = task_spawn(executor, Some(task_batch_resume), 2, 0);
            assert!(!batch.is_null());
            assert_eq!(executor_run(executor, batch), TASK_COMPLETED);

            // Starting another root gives the executor a safepoint after the
            // batch result has detached its consumed children.
            let next = task_spawn(executor, Some(completed_child_resume), 1, 0);
            assert!(!next.is_null());
            assert_eq!(executor_run(executor, next), TASK_COMPLETED);
            assert!(executor_tasks_reclaimed(executor) >= TASK_BATCH_SIZE as u64);
            assert!(executor_live_tasks(executor) <= 2);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn blocking_worker_does_not_delay_timer_or_cancellation() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let started = std::time::Instant::now();
        unsafe {
            let root = task_spawn(executor, Some(blocking_race_resume), 2, 0);
            assert!(!root.is_null());
            assert_eq!(executor_run(executor, root), TASK_COMPLETED);
            assert!(started.elapsed() < std::time::Duration::from_millis(100));
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn many_one_shot_completion_registrations_drain_exactly_once() {
        const COUNT: usize = 512;
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let completion = source(WAIT_SOURCE_COMPLETION);
        let mut frames = (0..COUNT)
            .map(|index| Box::new(i32::try_from(index).expect("frame index fits i32")))
            .collect::<Vec<_>>();
        let mut registrations = vec![LoomRegistration::default(); COUNT];
        unsafe {
            for (frame, registration) in frames.iter_mut().zip(&mut registrations) {
                assert_eq!(
                    executor_register(
                        executor,
                        &raw const completion,
                        (&raw mut **frame).cast::<c_void>(),
                        registration,
                    ),
                    WAIT_OK
                );
            }
            for registration in &registrations {
                assert_eq!(
                    executor_notify_completion(executor, registration, 0, 0),
                    WAIT_OK
                );
                assert_eq!(
                    executor_notify_completion(executor, registration, 0, 0),
                    WAIT_STALE_REGISTRATION
                );
            }
            let mut ready_count = 0;
            assert_eq!(executor_wait(executor, 0, &raw mut ready_count), WAIT_OK);
            assert_eq!(
                usize::try_from(ready_count).expect("ready count fits usize"),
                COUNT
            );
            let mut seen = std::collections::BTreeSet::new();
            for _ in 0..COUNT {
                let mut ready = LoomReadyNotification::default();
                assert_eq!(executor_pop_ready(executor, &raw mut ready), 1);
                assert_ne!(ready.events & READY_COMPLETED, 0);
                assert!(seen.insert(ready.frame as usize));
            }
            assert_eq!(seen.len(), COUNT);
            let mut empty = LoomReadyNotification::default();
            assert_eq!(executor_pop_ready(executor, &raw mut empty), 0);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }
}
