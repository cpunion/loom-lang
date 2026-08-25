//! Precise moving heap for compiler-generated Loom values.
//!
//! Collection only runs between coroutine resume calls. Generated code has no
//! native stack roots at that point: every value live across `.await` is in a
//! compiler-described Task slot, so the runtime can relocate objects without
//! pinning or exposing addresses to source programs.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::ptr;

use crate::reactor::LoomExecutor;
use crate::scheduler::{ValueNode, ValueSlot, trace_task_roots};

const VALUE_TAG_RECORD: u64 = 5;
const VALUE_TAG_TEXT: u64 = 4;
const VALUE_TAG_ENUM: u64 = 6;
const VALUE_TAG_REFINED: u64 = 7;
const VALUE_TAG_CONSTRAINT_ERROR: u64 = 8;
const VALUE_TAG_DYN: u64 = 9;
const VALUE_TAG_TUPLE: u64 = 10;
const VALUE_TAG_LIST: u64 = 12;
const VALUE_TAG_TASK_OUTCOME: u64 = 13;
const TASK_COMPLETED: u64 = 0;

thread_local! {
    static ACTIVE_EXECUTOR: Cell<*mut LoomExecutor> = const { Cell::new(ptr::null_mut()) };
    static ACTIVE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn enter_executor(executor: *mut LoomExecutor) {
    ACTIVE_EXECUTOR.with(|active| {
        let current = active.get();
        debug_assert!(current.is_null() || current == executor);
        if current.is_null() {
            active.set(executor);
        }
    });
    ACTIVE_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
}

pub(crate) fn leave_executor() {
    ACTIVE_DEPTH.with(|depth| {
        let current = depth.get();
        debug_assert!(current > 0);
        let remaining = current.saturating_sub(1);
        depth.set(remaining);
        if remaining == 0 {
            ACTIVE_EXECUTOR.with(|active| active.set(ptr::null_mut()));
        }
    });
}

/// Activates an executor-owned heap for synchronous generated code. Scheduler
/// resumes nest this activation while running an async root on the same thread.
#[unsafe(export_name = "loom_gc_activate_executor")]
pub unsafe extern "C" fn activate_executor(executor: *mut LoomExecutor) -> i32 {
    if executor.is_null() {
        return crate::WAIT_INVALID_ARGUMENT;
    }
    let compatible = ACTIVE_EXECUTOR.with(|active| {
        let current = active.get();
        current.is_null() || current == executor
    });
    if !compatible {
        return crate::WAIT_INVALID_ARGUMENT;
    }
    enter_executor(executor);
    crate::WAIT_OK
}

#[unsafe(export_name = "loom_gc_deactivate_executor")]
pub unsafe extern "C" fn deactivate_executor(executor: *mut LoomExecutor) -> i32 {
    if executor.is_null()
        || !ACTIVE_EXECUTOR.with(|active| active.get() == executor)
        || ACTIVE_DEPTH.with(|depth| depth.get() == 0)
    {
        return crate::WAIT_INVALID_ARGUMENT;
    }
    leave_executor();
    crate::WAIT_OK
}

#[unsafe(export_name = "loom_gc_alloc_value")]
pub extern "C" fn allocate_value() -> *mut c_void {
    let mut allocation = Box::new(ValueSlot::default());
    let pointer = (&raw mut *allocation).cast::<c_void>();
    ACTIVE_EXECUTOR.with(|active| {
        let executor = active.get();
        if executor.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: ACTIVE_EXECUTOR is set only around its single-threaded
            // resume call and the scheduler holds no Rust reference then.
            unsafe { (*executor).gc_values.push(allocation) };
        }
    });
    pointer
}

#[unsafe(export_name = "loom_gc_alloc_value_node")]
pub extern "C" fn allocate_value_node() -> *mut c_void {
    let mut allocation = Box::new(ValueNode {
        value: ValueSlot::default(),
        next: ptr::null_mut(),
    });
    let pointer = (&raw mut *allocation).cast::<c_void>();
    ACTIVE_EXECUTOR.with(|active| {
        let executor = active.get();
        if executor.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: see allocate_value.
            unsafe { (*executor).gc_nodes.push(allocation) };
        }
    });
    pointer
}

pub(crate) fn retain_bytes(bytes: Vec<u8>) -> (*const u8, u64) {
    let length = bytes.len() as u64;
    if bytes.is_empty() {
        return (std::ptr::NonNull::<u8>::dangling().as_ptr(), 0);
    }
    let bytes = bytes.into_boxed_slice();
    let pointer = bytes.as_ptr();
    ACTIVE_EXECUTOR.with(|active| {
        let executor = active.get();
        if executor.is_null() {
            let _ = Box::into_raw(bytes);
        } else {
            // SAFETY: see allocate_value. Text payloads are immutable after
            // publication and collection runs only at generated safepoints.
            unsafe { (*executor).gc_bytes.push(bytes) };
        }
    });
    (pointer, length)
}

/// Appends one already-evaluated value to a checked native List.
///
/// The generated-code ABI passes uniform value slots. A nonzero result means
/// the caller violated the checked MIR contract; allocation failure remains a
/// process-level OOM fault.
#[unsafe(export_name = "loom_runtime_list_add")]
pub unsafe extern "C" fn list_add(list: *mut ValueSlot, value: *const ValueSlot) -> i32 {
    if list.is_null() || value.is_null() {
        return 1;
    }
    // SAFETY: generated checked MIR provides live, aligned ValueSlot pointers.
    let list = unsafe { &mut *list };
    if list.words[0] != VALUE_TAG_LIST {
        return 1;
    }
    let node = allocate_value_node().cast::<ValueNode>();
    if node.is_null() {
        return 1;
    }
    // SAFETY: allocate_value_node returns a fresh initialized ValueNode and
    // `value` remains live for the duration of this call.
    unsafe {
        (*node).value = *value;
        (*node).next = ptr::null_mut();
    }
    let mut current = list.words[4] as *mut ValueNode;
    if current.is_null() {
        list.words[4] = node as u64;
    } else {
        // SAFETY: the List data word is a runtime-owned, null-terminated chain.
        unsafe {
            while !(*current).next.is_null() {
                current = (*current).next;
            }
            (*current).next = node;
        }
    }
    list.words[2] = list.words[2]
        .checked_add(1)
        .unwrap_or_else(|| std::process::abort());
    0
}

/// Returns a borrowed pointer to a List element, or null when out of range.
#[unsafe(export_name = "loom_runtime_list_get")]
pub unsafe extern "C" fn list_get(list: *const ValueSlot, index: i64) -> *const ValueSlot {
    if list.is_null() || index < 0 {
        return ptr::null();
    }
    // SAFETY: generated checked MIR provides a live, aligned ValueSlot pointer.
    let list = unsafe { &*list };
    if list.words[0] != VALUE_TAG_LIST || index.cast_unsigned() >= list.words[2] {
        return ptr::null();
    }
    let mut node = list.words[4] as *const ValueNode;
    for _ in 0..index {
        if node.is_null() {
            return ptr::null();
        }
        // SAFETY: List chains are runtime-owned and null-terminated.
        node = unsafe { (*node).next };
    }
    if node.is_null() {
        ptr::null()
    } else {
        // SAFETY: node is non-null and belongs to the live List chain.
        unsafe { &raw const (*node).value }
    }
}

/// Witness argument lists are immutable compiler metadata. They are kept in a
/// non-moving executor arena because generated call sites can hold their raw
/// address transiently; they never contain user heap values.
#[unsafe(export_name = "loom_gc_alloc_witness_node")]
pub extern "C" fn allocate_witness_node() -> *mut c_void {
    let mut allocation = Box::new([0_usize; 2]);
    let pointer = (&raw mut *allocation).cast::<c_void>();
    ACTIVE_EXECUTOR.with(|active| {
        let executor = active.get();
        if executor.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: see allocate_value.
            unsafe { (*executor).metadata_nodes.push(allocation) };
        }
    });
    pointer
}

struct HeapIndex {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
    bytes: HashSet<usize>,
}

impl HeapIndex {
    fn new(executor: &LoomExecutor) -> Self {
        Self {
            values: executor
                .gc_values
                .iter()
                .map(|value| (&raw const **value) as usize)
                .collect(),
            nodes: executor
                .gc_nodes
                .iter()
                .map(|node| (&raw const **node) as usize)
                .collect(),
            bytes: executor
                .gc_bytes
                .iter()
                .map(|bytes| bytes.as_ptr() as usize)
                .collect(),
        }
    }
}

#[derive(Default)]
struct Marks {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
    bytes: HashSet<usize>,
}

struct TraceContext {
    index: *const HeapIndex,
    marks: *mut Marks,
}

unsafe extern "C" fn trace_slot(slot: *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    let context = unsafe { &mut *context.cast::<TraceContext>() };
    let index = unsafe { &*context.index };
    let marks = unsafe { &mut *context.marks };
    trace_value(unsafe { &*slot.cast::<ValueSlot>() }, index, marks);
}

fn trace_value(value: &ValueSlot, index: &HeapIndex, marks: &mut Marks) {
    match value.words[0] {
        VALUE_TAG_TEXT => {
            let Ok(address) = usize::try_from(value.words[4]) else {
                return;
            };
            if index.bytes.contains(&address) {
                marks.bytes.insert(address);
            }
        }
        VALUE_TAG_RECORD | VALUE_TAG_CONSTRAINT_ERROR | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
            trace_nodes(
                value.words[4] as *const ValueNode,
                value.words[2],
                index,
                marks,
            );
        }
        VALUE_TAG_ENUM => {
            trace_nodes(
                value.words[4] as *const ValueNode,
                value.words[3],
                index,
                marks,
            );
        }
        VALUE_TAG_REFINED | VALUE_TAG_DYN => {
            trace_value_pointer(value.words[4] as *const ValueSlot, index, marks);
        }
        VALUE_TAG_TASK_OUTCOME if value.words[2] == TASK_COMPLETED => {
            trace_value_pointer(value.words[4] as *const ValueSlot, index, marks);
        }
        _ => {}
    }
}

fn trace_value_pointer(pointer: *const ValueSlot, index: &HeapIndex, marks: &mut Marks) {
    if pointer.is_null() {
        return;
    }
    let address = pointer as usize;
    if index.values.contains(&address) && !marks.values.insert(address) {
        return;
    }
    // SAFETY: checked MIR only stores live Value pointers in managed fields;
    // untracked pointers belong to the runtime result arena or process arena.
    trace_value(unsafe { &*pointer }, index, marks);
}

fn trace_nodes(mut pointer: *const ValueNode, count: u64, index: &HeapIndex, marks: &mut Marks) {
    for _ in 0..count {
        if pointer.is_null() {
            return;
        }
        let address = pointer as usize;
        if index.nodes.contains(&address) && !marks.nodes.insert(address) {
            return;
        }
        // SAFETY: aggregate counts and chains were validated/constructed by
        // compiler or runtime code and remain live until this safepoint.
        let node = unsafe { &*pointer };
        trace_value(&node.value, index, marks);
        pointer = node.next;
    }
}

fn rewrite_value(
    value: &mut ValueSlot,
    values: &HashMap<usize, *mut ValueSlot>,
    nodes: &HashMap<usize, *mut ValueNode>,
    bytes: &HashMap<usize, *const u8>,
) {
    let Ok(address) = usize::try_from(value.words[4]) else {
        return;
    };
    match value.words[0] {
        VALUE_TAG_TEXT => {
            if let Some(pointer) = bytes.get(&address) {
                value.words[4] = *pointer as u64;
            }
        }
        VALUE_TAG_RECORD
        | VALUE_TAG_CONSTRAINT_ERROR
        | VALUE_TAG_ENUM
        | VALUE_TAG_TUPLE
        | VALUE_TAG_LIST => {
            if let Some(pointer) = nodes.get(&address) {
                value.words[4] = *pointer as u64;
            }
        }
        VALUE_TAG_REFINED | VALUE_TAG_DYN | VALUE_TAG_TASK_OUTCOME => {
            if let Some(pointer) = values.get(&address) {
                value.words[4] = *pointer as u64;
            }
        }
        _ => {}
    }
}

pub(crate) fn collect(executor: &mut LoomExecutor) {
    executor.gc_collections = executor.gc_collections.saturating_add(1);
    if executor.gc_values.is_empty() && executor.gc_nodes.is_empty() && executor.gc_bytes.is_empty()
    {
        return;
    }
    let index = HeapIndex::new(executor);
    let mut marks = Marks::default();
    let mut trace_context = TraceContext {
        index: &raw const index,
        marks: &raw mut marks,
    };
    for task in &executor.tasks {
        let task = (&raw const **task).cast_mut();
        unsafe { trace_task_roots(task, Some(trace_slot), (&raw mut trace_context).cast()) };
    }

    let before = executor
        .gc_values
        .len()
        .saturating_add(executor.gc_nodes.len())
        .saturating_add(executor.gc_bytes.len());
    executor
        .gc_values
        .retain(|value| marks.values.contains(&((&raw const **value) as usize)));
    executor
        .gc_nodes
        .retain(|node| marks.nodes.contains(&((&raw const **node) as usize)));
    executor
        .gc_bytes
        .retain(|bytes| marks.bytes.contains(&(bytes.as_ptr() as usize)));
    let after = executor
        .gc_values
        .len()
        .saturating_add(executor.gc_nodes.len())
        .saturating_add(executor.gc_bytes.len());
    executor.gc_reclaimed = executor
        .gc_reclaimed
        .saturating_add((before.saturating_sub(after)) as u64);

    let mut value_moves = HashMap::with_capacity(executor.gc_values.len());
    for value in &mut executor.gc_values {
        let old = (&raw mut **value) as usize;
        let mut replacement = Box::new(**value);
        let new = &raw mut *replacement;
        *value = replacement;
        value_moves.insert(old, new);
    }
    let mut node_moves = HashMap::with_capacity(executor.gc_nodes.len());
    for node in &mut executor.gc_nodes {
        let old = (&raw mut **node) as usize;
        let mut replacement = Box::new(ValueNode {
            value: node.value,
            next: node.next,
        });
        let new = &raw mut *replacement;
        *node = replacement;
        node_moves.insert(old, new);
    }
    let mut byte_moves = HashMap::with_capacity(executor.gc_bytes.len());
    for bytes in &mut executor.gc_bytes {
        let old = bytes.as_ptr() as usize;
        let replacement = bytes.to_vec().into_boxed_slice();
        let new = replacement.as_ptr();
        *bytes = replacement;
        byte_moves.insert(old, new);
    }
    executor.gc_relocations = executor.gc_relocations.saturating_add(
        (value_moves
            .len()
            .saturating_add(node_moves.len())
            .saturating_add(byte_moves.len())) as u64,
    );

    for task in &mut executor.tasks {
        for slot in &mut task.slots {
            rewrite_value(slot, &value_moves, &node_moves, &byte_moves);
        }
    }
    for value in &mut executor.gc_values {
        rewrite_value(value, &value_moves, &node_moves, &byte_moves);
    }
    for node in &mut executor.gc_nodes {
        rewrite_value(&mut node.value, &value_moves, &node_moves, &byte_moves);
        if let Some(next) = node_moves.get(&(node.next as usize)) {
            node.next = *next;
        }
    }
}
