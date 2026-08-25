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

use loom_runtime_abi::{
    TASK_COMPLETED, VALUE_TAG_CONSTRAINT_ERROR, VALUE_TAG_DYN, VALUE_TAG_ENUM, VALUE_TAG_LIST,
    VALUE_TAG_RECORD, VALUE_TAG_REFINED, VALUE_TAG_TASK_OUTCOME, VALUE_TAG_TEXT, VALUE_TAG_TUPLE,
    VALUE_WORD_AUX, VALUE_WORD_DATA, VALUE_WORD_SCALAR, VALUE_WORD_TAG,
};

use crate::reactor::LoomExecutor;
use crate::runtime::LoomRuntime;
use crate::scheduler::{LoomTask, ValueNode, ValueSlot, trace_task_roots};
use crate::text;

pub(crate) struct ListNodeIndex {
    pub(crate) length: u64,
    pub(crate) tail: *mut ValueNode,
    pub(crate) nodes: Option<Vec<*mut ValueNode>>,
}

/// Runtime-owned storage for managed Loom values.
///
/// The heap is deliberately independent from the async reactor and scheduler:
/// synchronous generated code needs managed allocation, but must not need an
/// operating-system poller or worker-completion channel. Collection is still
/// driven only by the scheduler safepoint between coroutine resume calls.
#[derive(Default)]
pub(crate) struct LoomHeap {
    pub(crate) values: Vec<Box<ValueSlot>>,
    pub(crate) nodes: Vec<Box<ValueNode>>,
    pub(crate) sequences: Vec<Box<[u64]>>,
    /// Derived, non-owning indexes for native List chains. Collection clears
    /// these before relocating nodes, so they are never roots and never retain
    /// stale pointers across a safepoint.
    pub(crate) list_node_indexes: HashMap<usize, ListNodeIndex>,
    /// Immutable compiler witness metadata is non-moving but shares the
    /// runtime-owned allocation lifetime with managed values.
    pub(crate) metadata_nodes: Vec<Box<[usize; 2]>>,
    pub(crate) collections: u64,
    pub(crate) relocations: u64,
    pub(crate) reclaimed: u64,
}

thread_local! {
    static ACTIVE_RUNTIME: Cell<*mut LoomRuntime> = const { Cell::new(ptr::null_mut()) };
    static ACTIVE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

fn enter_runtime(runtime: *mut LoomRuntime) {
    ACTIVE_RUNTIME.with(|active| {
        let current = active.get();
        debug_assert!(current.is_null() || current == runtime);
        if current.is_null() {
            active.set(runtime);
        }
    });
    ACTIVE_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
}

fn leave_runtime() {
    ACTIVE_DEPTH.with(|depth| {
        let current = depth.get();
        debug_assert!(current > 0);
        let remaining = current.saturating_sub(1);
        depth.set(remaining);
        if remaining == 0 {
            ACTIVE_RUNTIME.with(|active| active.set(ptr::null_mut()));
        }
    });
}

pub(crate) fn runtime_is_active(runtime: *mut LoomRuntime) -> bool {
    !runtime.is_null()
        && ACTIVE_RUNTIME.with(|active| active.get() == runtime)
        && ACTIVE_DEPTH.with(|depth| depth.get() != 0)
}

/// Returns the runtime installed for the current generated-code interval.
///
/// This is an internal routing identity, not a general-purpose way to access
/// the heap. A null result means that generated code has no active runtime on
/// this thread.
pub(crate) fn active_runtime_pointer() -> *mut LoomRuntime {
    if ACTIVE_DEPTH.with(|depth| depth.get() == 0) {
        ptr::null_mut()
    } else {
        ACTIVE_RUNTIME.with(Cell::get)
    }
}

pub(crate) fn enter_executor(executor: *mut LoomExecutor) {
    debug_assert!(!executor.is_null());
    // SAFETY: scheduler callers hold a live executor for the generated-code
    // interval, and its runtime attachment remains valid until executor Drop.
    enter_runtime(unsafe { (*executor).runtime_pointer() });
}

pub(crate) fn leave_executor() {
    leave_runtime();
}

/// Activates a standalone runtime heap for synchronous generated code.
#[unsafe(export_name = "loom_runtime_activate_v1")]
pub unsafe extern "C" fn activate_runtime_v1(runtime: *mut LoomRuntime) -> i32 {
    if runtime.is_null() {
        return crate::WAIT_INVALID_ARGUMENT;
    }
    let compatible = ACTIVE_RUNTIME.with(|active| {
        let current = active.get();
        current.is_null() || current == runtime
    });
    if !compatible {
        return crate::WAIT_INVALID_ARGUMENT;
    }
    enter_runtime(runtime);
    crate::WAIT_OK
}

#[unsafe(export_name = "loom_runtime_deactivate_v1")]
pub unsafe extern "C" fn deactivate_runtime_v1(runtime: *mut LoomRuntime) -> i32 {
    if runtime.is_null()
        || !ACTIVE_RUNTIME.with(|active| active.get() == runtime)
        || ACTIVE_DEPTH.with(|depth| depth.get() == 0)
    {
        return crate::WAIT_INVALID_ARGUMENT;
    }
    leave_runtime();
    crate::WAIT_OK
}

#[unsafe(export_name = "loom_gc_alloc_value")]
pub extern "C" fn allocate_value() -> *mut c_void {
    let mut allocation = Box::new(ValueSlot::default());
    let pointer = (&raw mut *allocation).cast::<c_void>();
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: ACTIVE_RUNTIME is set only around a single-threaded
            // generated-code interval and collection cannot run during it.
            unsafe { (*runtime).heap.values.push(allocation) };
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
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: see allocate_value.
            unsafe { (*runtime).heap.nodes.push(allocation) };
        }
    });
    pointer
}

fn retain_sequence(allocation: Box<[u64]>, object: *mut c_void) -> *mut c_void {
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: see allocate_value. Sequence objects are immutable after
            // publication and collection runs only at generated safepoints.
            unsafe { (*runtime).heap.sequences.push(allocation) };
        }
    });
    object
}

pub(crate) fn retain_text(bytes: &[u8]) -> Option<*mut c_void> {
    let (allocation, object) = text::allocate_text_storage(bytes)?;
    Some(retain_sequence(allocation, object.cast()))
}

pub(crate) fn retain_byte_sequence(bytes: &[u8]) -> Option<*mut c_void> {
    let (allocation, object) = text::allocate_byte_storage(bytes)?;
    Some(retain_sequence(allocation, object.cast()))
}

pub(crate) fn text_value(bytes: &[u8]) -> Option<ValueSlot> {
    retain_text(bytes).map(text::value)
}

pub(crate) fn byte_value(bytes: &[u8]) -> Option<ValueSlot> {
    retain_byte_sequence(bytes).map(text::value)
}

unsafe fn list_chain(mut node: *mut ValueNode, count: u64) -> Option<Vec<*mut ValueNode>> {
    let count = usize::try_from(count).ok()?;
    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        if node.is_null() {
            return None;
        }
        nodes.push(node);
        // SAFETY: callers pass a runtime-created List head and its checked
        // element count. The bounded walk rejects a prematurely-ended chain.
        node = unsafe { (*node).next };
    }
    node.is_null().then_some(nodes)
}

unsafe fn list_tail(mut node: *mut ValueNode, count: u64) -> Option<*mut ValueNode> {
    let count = usize::try_from(count).ok()?;
    let mut tail = ptr::null_mut();
    for _ in 0..count {
        if node.is_null() {
            return None;
        }
        tail = node;
        // SAFETY: the same checked, bounded-chain invariant as list_chain.
        node = unsafe { (*node).next };
    }
    node.is_null().then_some(tail)
}

unsafe fn list_node_at(mut node: *mut ValueNode, index: usize) -> Option<*mut ValueNode> {
    for _ in 0..index {
        if node.is_null() {
            return None;
        }
        // SAFETY: callers already checked the index against the List count.
        node = unsafe { (*node).next };
    }
    (!node.is_null()).then_some(node)
}

fn has_active_runtime() -> bool {
    ACTIVE_RUNTIME.with(|active| !active.get().is_null())
}

fn cached_list_node(head: *mut ValueNode, count: u64, index: usize) -> Option<*mut ValueNode> {
    if head.is_null() {
        return None;
    }
    let count = usize::try_from(count).ok()?;
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            return None;
        }
        // SAFETY: ACTIVE_RUNTIME is installed only for its single-threaded
        // generated-code interval. We copy one raw node pointer and do not
        // retain a Rust borrow across any runtime allocation.
        unsafe {
            (*runtime)
                .heap
                .list_node_indexes
                .get(&(head as usize))
                .filter(|entry| entry.length == count as u64)
                .and_then(|entry| entry.nodes.as_ref())
                .and_then(|nodes| nodes.get(index))
                .copied()
        }
    })
}

fn cached_list_tail(head: *mut ValueNode, count: u64) -> Option<*mut ValueNode> {
    if head.is_null() {
        return None;
    }
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            return None;
        }
        // SAFETY: see cached_list_node.
        unsafe {
            (*runtime)
                .heap
                .list_node_indexes
                .get(&(head as usize))
                .filter(|entry| entry.length == count)
                .map(|entry| entry.tail)
                .filter(|tail| !tail.is_null())
        }
    })
}

fn cache_list_chain(head: *mut ValueNode, count: u64, nodes: Vec<*mut ValueNode>) {
    if head.is_null() {
        return;
    }
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if !runtime.is_null() {
            // SAFETY: see cached_list_node. This derived index owns no nodes
            // and is discarded before the collector can relocate them.
            unsafe {
                let tail = nodes.last().copied().unwrap_or(ptr::null_mut());
                (*runtime).heap.list_node_indexes.insert(
                    head as usize,
                    ListNodeIndex {
                        length: count,
                        tail,
                        nodes: Some(nodes),
                    },
                );
            }
        }
    });
}

fn cache_list_tail(head: *mut ValueNode, count: u64, tail: *mut ValueNode) {
    if head.is_null() || tail.is_null() {
        return;
    }
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if !runtime.is_null() {
            // SAFETY: see cache_list_chain.
            unsafe {
                (*runtime).heap.list_node_indexes.insert(
                    head as usize,
                    ListNodeIndex {
                        length: count,
                        tail,
                        nodes: None,
                    },
                );
            }
        }
    });
}

fn append_cached_list_node(head: *mut ValueNode, count: u64, node: *mut ValueNode) -> bool {
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            return false;
        }
        // SAFETY: see cached_list_node. list_add is the only native chain
        // mutator and updates this index in the same call as the next link.
        unsafe {
            let Some(entry) = (*runtime).heap.list_node_indexes.get_mut(&(head as usize)) else {
                return false;
            };
            if entry.length != count {
                return false;
            }
            entry.length = entry
                .length
                .checked_add(1)
                .unwrap_or_else(|| std::process::abort());
            entry.tail = node;
            if let Some(nodes) = &mut entry.nodes {
                nodes.push(node);
            }
            true
        }
    })
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
    let head = list.words[VALUE_WORD_DATA] as *mut ValueNode;
    let count = list.words[VALUE_WORD_AUX];
    let new_count = count
        .checked_add(1)
        .unwrap_or_else(|| std::process::abort());
    let tail = if head.is_null() {
        if count != 0 {
            return 1;
        }
        None
    } else if let Some(tail) = cached_list_tail(head, count) {
        Some(tail)
    } else {
        let Some(tail) = (unsafe { list_tail(head, count) }) else {
            return 1;
        };
        Some(tail)
    };
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
    if head.is_null() {
        list.words[VALUE_WORD_DATA] = node as u64;
        cache_list_tail(node, new_count, node);
    } else {
        let Some(tail) = tail else {
            return 1;
        };
        // SAFETY: the checked chain walk or runtime-local index selected the
        // final node in this List's runtime-owned chain.
        unsafe {
            (*tail).next = node;
        }
        if !append_cached_list_node(head, count, node) {
            cache_list_tail(head, new_count, node);
        }
    }
    list.words[VALUE_WORD_AUX] = new_count;
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
    if list.words[VALUE_WORD_TAG] != VALUE_TAG_LIST
        || index.cast_unsigned() >= list.words[VALUE_WORD_AUX]
    {
        return ptr::null();
    }
    let head = list.words[VALUE_WORD_DATA] as *mut ValueNode;
    let index = usize::try_from(index).unwrap_or_else(|_| unreachable!());
    let node = if !has_active_runtime() {
        let Some(node) = (unsafe { list_node_at(head, index) }) else {
            return ptr::null();
        };
        node
    } else if let Some(node) = cached_list_node(head, list.words[VALUE_WORD_AUX], index) {
        node
    } else {
        let Some(nodes) = (unsafe { list_chain(head, list.words[VALUE_WORD_AUX]) }) else {
            return ptr::null();
        };
        let node = nodes[index];
        cache_list_chain(head, list.words[VALUE_WORD_AUX], nodes);
        node
    };
    if node.is_null() {
        ptr::null()
    } else {
        // SAFETY: node is non-null and belongs to the live List chain.
        unsafe { &raw const (*node).value }
    }
}

/// Witness argument lists are immutable compiler metadata. They are kept in a
/// non-moving runtime arena because generated call sites can hold their raw
/// address transiently; they never contain user heap values.
#[unsafe(export_name = "loom_gc_alloc_witness_node")]
pub extern "C" fn allocate_witness_node() -> *mut c_void {
    let mut allocation = Box::new([0_usize; 2]);
    let pointer = (&raw mut *allocation).cast::<c_void>();
    ACTIVE_RUNTIME.with(|active| {
        let runtime = active.get();
        if runtime.is_null() {
            let _ = Box::into_raw(allocation);
        } else {
            // SAFETY: see allocate_value.
            unsafe { (*runtime).heap.metadata_nodes.push(allocation) };
        }
    });
    pointer
}

struct HeapIndex {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
    sequences: HashSet<usize>,
}

impl HeapIndex {
    fn new(heap: &LoomHeap) -> Self {
        Self {
            values: heap
                .values
                .iter()
                .map(|value| (&raw const **value) as usize)
                .collect(),
            nodes: heap
                .nodes
                .iter()
                .map(|node| (&raw const **node) as usize)
                .collect(),
            sequences: heap
                .sequences
                .iter()
                .map(|sequence| sequence.as_ptr() as usize)
                .collect(),
        }
    }
}

#[derive(Default)]
struct Marks {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
    sequences: HashSet<usize>,
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
    match value.words[VALUE_WORD_TAG] {
        VALUE_TAG_TEXT => {
            let Some(object) = text::object(value) else {
                return;
            };
            let address = object as usize;
            if index.sequences.contains(&address) {
                marks.sequences.insert(address);
            }
        }
        VALUE_TAG_RECORD | VALUE_TAG_CONSTRAINT_ERROR | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
            trace_nodes(
                value.words[VALUE_WORD_DATA] as *const ValueNode,
                value.words[VALUE_WORD_AUX],
                index,
                marks,
            );
        }
        VALUE_TAG_ENUM => {
            trace_nodes(
                value.words[VALUE_WORD_DATA] as *const ValueNode,
                value.words[VALUE_WORD_SCALAR],
                index,
                marks,
            );
        }
        VALUE_TAG_REFINED | VALUE_TAG_DYN => {
            trace_value_pointer(
                value.words[VALUE_WORD_DATA] as *const ValueSlot,
                index,
                marks,
            );
        }
        VALUE_TAG_TASK_OUTCOME if value.words[VALUE_WORD_AUX] == TASK_COMPLETED as u64 => {
            trace_value_pointer(
                value.words[VALUE_WORD_DATA] as *const ValueSlot,
                index,
                marks,
            );
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
        let newly_marked = !index.nodes.contains(&address) || marks.nodes.insert(address);
        // A shared head may first be reached through a shorter bounded view.
        // Continue walking an already-marked chain so a later longer view can
        // still mark its tail, while avoiding duplicate child traversal.
        if newly_marked {
            // SAFETY: aggregate counts and chains were validated/constructed
            // by compiler or runtime code and remain live until this safepoint.
            trace_value(unsafe { &(*pointer).value }, index, marks);
        }
        // SAFETY: the same bounded-chain invariant applies to its next link.
        pointer = unsafe { (*pointer).next };
    }
}

fn rewrite_value(
    value: &mut ValueSlot,
    values: &HashMap<usize, *mut ValueSlot>,
    nodes: &HashMap<usize, *mut ValueNode>,
    sequences: &HashMap<usize, *mut c_void>,
) {
    let Ok(address) = usize::try_from(value.words[VALUE_WORD_DATA]) else {
        return;
    };
    match value.words[VALUE_WORD_TAG] {
        VALUE_TAG_TEXT => {
            if let Some(pointer) = sequences.get(&address) {
                value.words[VALUE_WORD_DATA] = *pointer as u64;
            }
        }
        VALUE_TAG_RECORD
        | VALUE_TAG_CONSTRAINT_ERROR
        | VALUE_TAG_ENUM
        | VALUE_TAG_TUPLE
        | VALUE_TAG_LIST => {
            if let Some(pointer) = nodes.get(&address) {
                value.words[VALUE_WORD_DATA] = *pointer as u64;
            }
        }
        VALUE_TAG_REFINED | VALUE_TAG_DYN | VALUE_TAG_TASK_OUTCOME => {
            if let Some(pointer) = values.get(&address) {
                value.words[VALUE_WORD_DATA] = *pointer as u64;
            }
        }
        _ => {}
    }
}

struct RewriteContext<'maps> {
    values: &'maps HashMap<usize, *mut ValueSlot>,
    nodes: &'maps HashMap<usize, *mut ValueNode>,
    sequences: &'maps HashMap<usize, *mut c_void>,
}

unsafe extern "C" fn rewrite_slot(slot: *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    let context = unsafe { &*context.cast::<RewriteContext<'_>>() };
    rewrite_value(
        unsafe { &mut *slot.cast::<ValueSlot>() },
        context.values,
        context.nodes,
        context.sequences,
    );
}

pub(crate) fn collect(executor: &mut LoomExecutor) {
    let runtime = executor.runtime_pointer();
    // SAFETY: the attached runtime and executor are separate stable
    // allocations. The scheduler owns `&mut executor` at this safepoint, so
    // no generated code can access the heap while its task roots are traced.
    unsafe { collect_heap(&mut (*runtime).heap, &mut executor.tasks) };
}

fn collect_heap(heap: &mut LoomHeap, tasks: &mut [Box<LoomTask>]) {
    heap.collections = heap.collections.saturating_add(1);
    // List indexes are derived accelerators, not roots. Drop every raw node
    // pointer before filtering or relocating the heap; the next add/get lazily
    // rebuilds an index from the rewritten List head.
    heap.list_node_indexes.clear();
    if heap.values.is_empty() && heap.nodes.is_empty() && heap.sequences.is_empty() {
        return;
    }
    let index = HeapIndex::new(heap);
    let mut marks = Marks::default();
    let mut trace_context = TraceContext {
        index: &raw const index,
        marks: &raw mut marks,
    };
    for task in tasks.iter() {
        let task = (&raw const **task).cast_mut();
        unsafe { trace_task_roots(task, Some(trace_slot), (&raw mut trace_context).cast()) };
    }

    let before = heap
        .values
        .len()
        .saturating_add(heap.nodes.len())
        .saturating_add(heap.sequences.len());
    heap.values
        .retain(|value| marks.values.contains(&((&raw const **value) as usize)));
    heap.nodes
        .retain(|node| marks.nodes.contains(&((&raw const **node) as usize)));
    heap.sequences
        .retain(|sequence| marks.sequences.contains(&(sequence.as_ptr() as usize)));
    let after = heap
        .values
        .len()
        .saturating_add(heap.nodes.len())
        .saturating_add(heap.sequences.len());
    heap.reclaimed = heap
        .reclaimed
        .saturating_add((before.saturating_sub(after)) as u64);

    let mut value_moves = HashMap::with_capacity(heap.values.len());
    for value in &mut heap.values {
        let old = (&raw mut **value) as usize;
        let mut replacement = Box::new(**value);
        let new = &raw mut *replacement;
        *value = replacement;
        value_moves.insert(old, new);
    }
    let mut node_moves = HashMap::with_capacity(heap.nodes.len());
    for node in &mut heap.nodes {
        let old = (&raw mut **node) as usize;
        let mut replacement = Box::new(ValueNode {
            value: node.value,
            next: node.next,
        });
        let new = &raw mut *replacement;
        *node = replacement;
        node_moves.insert(old, new);
    }
    let mut sequence_moves = HashMap::with_capacity(heap.sequences.len());
    for sequence in &mut heap.sequences {
        let old = sequence.as_ptr() as usize;
        let mut replacement = sequence.to_vec().into_boxed_slice();
        let new = replacement.as_mut_ptr().cast::<c_void>();
        *sequence = replacement;
        sequence_moves.insert(old, new);
    }
    heap.relocations = heap.relocations.saturating_add(
        (value_moves
            .len()
            .saturating_add(node_moves.len())
            .saturating_add(sequence_moves.len())) as u64,
    );

    let mut rewrite_context = RewriteContext {
        values: &value_moves,
        nodes: &node_moves,
        sequences: &sequence_moves,
    };
    for task in tasks {
        unsafe {
            trace_task_roots(
                &raw mut **task,
                Some(rewrite_slot),
                (&raw mut rewrite_context).cast(),
            );
        }
    }
    for value in &mut heap.values {
        rewrite_value(value, &value_moves, &node_moves, &sequence_moves);
    }
    for node in &mut heap.nodes {
        rewrite_value(&mut node.value, &value_moves, &node_moves, &sequence_moves);
        if let Some(next) = node_moves.get(&(node.next as usize)) {
            node.next = *next;
        }
    }
}
