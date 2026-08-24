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
use crate::scheduler::{ValueNode, ValueSlot};

const VALUE_TAG_RECORD: u64 = 5;
const VALUE_TAG_ENUM: u64 = 6;
const VALUE_TAG_REFINED: u64 = 7;
const VALUE_TAG_TUPLE: u64 = 10;
const VALUE_TAG_LIST: u64 = 12;
const VALUE_TAG_TASK_OUTCOME: u64 = 13;
const TASK_COMPLETED: u64 = 0;

thread_local! {
    static ACTIVE_EXECUTOR: Cell<*mut LoomExecutor> = const { Cell::new(ptr::null_mut()) };
}

pub(crate) fn enter_executor(executor: *mut LoomExecutor) {
    ACTIVE_EXECUTOR.with(|active| {
        debug_assert!(active.get().is_null());
        active.set(executor);
    });
}

pub(crate) fn leave_executor() {
    ACTIVE_EXECUTOR.with(|active| active.set(ptr::null_mut()));
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
        }
    }
}

#[derive(Default)]
struct Marks {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
}

fn trace_value(value: &ValueSlot, index: &HeapIndex, marks: &mut Marks) {
    match value.words[0] {
        VALUE_TAG_RECORD | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
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
        VALUE_TAG_REFINED => trace_value_pointer(value.words[4] as *const ValueSlot, index, marks),
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
) {
    let Ok(address) = usize::try_from(value.words[4]) else {
        return;
    };
    match value.words[0] {
        VALUE_TAG_RECORD | VALUE_TAG_ENUM | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
            if let Some(pointer) = nodes.get(&address) {
                value.words[4] = *pointer as u64;
            }
        }
        VALUE_TAG_REFINED | VALUE_TAG_TASK_OUTCOME => {
            if let Some(pointer) = values.get(&address) {
                value.words[4] = *pointer as u64;
            }
        }
        _ => {}
    }
}

pub(crate) fn collect(executor: &mut LoomExecutor) {
    executor.gc_collections = executor.gc_collections.saturating_add(1);
    if executor.gc_values.is_empty() && executor.gc_nodes.is_empty() {
        return;
    }
    let index = HeapIndex::new(executor);
    let mut marks = Marks::default();
    for task in &executor.tasks {
        for slot in &task.slots {
            trace_value(slot, &index, &mut marks);
        }
    }
    for value in &executor.result_values {
        trace_value(value, &index, &mut marks);
    }
    for node in &executor.result_nodes {
        trace_value(&node.value, &index, &mut marks);
    }

    let before = executor
        .gc_values
        .len()
        .saturating_add(executor.gc_nodes.len());
    executor
        .gc_values
        .retain(|value| marks.values.contains(&((&raw const **value) as usize)));
    executor
        .gc_nodes
        .retain(|node| marks.nodes.contains(&((&raw const **node) as usize)));
    let after = executor
        .gc_values
        .len()
        .saturating_add(executor.gc_nodes.len());
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
    executor.gc_relocations = executor
        .gc_relocations
        .saturating_add((value_moves.len().saturating_add(node_moves.len())) as u64);

    for task in &mut executor.tasks {
        for slot in &mut task.slots {
            rewrite_value(slot, &value_moves, &node_moves);
        }
    }
    for value in &mut executor.result_values {
        rewrite_value(value, &value_moves, &node_moves);
    }
    for node in &mut executor.result_nodes {
        rewrite_value(&mut node.value, &value_moves, &node_moves);
        if let Some(next) = node_moves.get(&(node.next as usize)) {
            node.next = *next;
        }
    }
    for value in &mut executor.gc_values {
        rewrite_value(value, &value_moves, &node_moves);
    }
    for node in &mut executor.gc_nodes {
        rewrite_value(&mut node.value, &value_moves, &node_moves);
        if let Some(next) = node_moves.get(&(node.next as usize)) {
            node.next = *next;
        }
    }
}
