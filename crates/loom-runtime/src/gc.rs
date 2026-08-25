//! Precise moving heap for compiler-generated Loom values.
//!
//! Collection runs only at compiler-known synchronous safepoints or between
//! coroutine resume calls. Synchronous generated code publishes precise
//! shadow-stack roots; every value live across `.await` is in a compiler-
//! described Task slot. The runtime can therefore relocate objects without
//! pinning or exposing addresses to source programs.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::Ordering;

use loom_runtime_abi::{
    GC_ABI_MISMATCH, GC_FRAME_ORDER, GC_INVALID_ARGUMENT, GC_OK, GC_ROOT_FRAME_LINKED,
    GC_ROOT_STACK_NOT_EMPTY, LoomGcRootDescriptor, LoomGcRootFrame, LoomWitnessInstance,
    SHADOW_STACK_ABI_VERSION, TASK_COMPLETED, VALUE_SLOT_WORDS, VALUE_TAG_CONSTRAINT_ERROR,
    VALUE_TAG_DYN, VALUE_TAG_ENUM, VALUE_TAG_LIST, VALUE_TAG_RECORD, VALUE_TAG_REFINED,
    VALUE_TAG_TASK_OUTCOME, VALUE_TAG_TEXT, VALUE_TAG_TUPLE, VALUE_WORD_AUX, VALUE_WORD_DATA,
    VALUE_WORD_SCALAR, VALUE_WORD_TAG, VALUE_WORD_WITNESS,
};

use crate::reactor::LoomExecutor;
use crate::runtime::LoomRuntime;
use crate::scheduler::{LoomTask, LoomTraceVisitor, ValueNode, ValueSlot, trace_task_roots};
use crate::text;
use crate::witness::{WitnessArena, clone_witnesses, walk_witnesses};

pub(crate) struct ListNodeIndex {
    pub(crate) length: u64,
    pub(crate) tail: *mut ValueNode,
    pub(crate) nodes: Option<Vec<*mut ValueNode>>,
}

pub(crate) const MIN_GC_THRESHOLD_BYTES: usize = 64 * 1024;

/// Runtime-owned storage for managed Loom values.
///
/// The heap is deliberately independent from the async reactor and scheduler:
/// synchronous generated code needs managed allocation, but must not need an
/// operating-system poller or worker-completion channel. Collection is driven
/// by thresholded compiler or scheduler safepoint polls, never by allocation.
#[derive(Default)]
pub(crate) struct LoomHeap {
    pub(crate) values: Vec<Box<ValueSlot>>,
    pub(crate) nodes: Vec<Box<ValueNode>>,
    pub(crate) sequences: Vec<Box<[u64]>>,
    /// Derived, non-owning indexes for native List chains. Collection clears
    /// these before relocating nodes, so they are never roots and never retain
    /// stale pointers across a safepoint.
    pub(crate) list_node_indexes: HashMap<usize, ListNodeIndex>,
    /// Immutable proof instances remain non-moving because generated hidden
    /// arguments may hold their raw addresses across a safepoint. Unlike
    /// compiler descriptor globals, this arena is marked from owned `dyn`
    /// values and swept with the moving value heap.
    pub(crate) witnesses: WitnessArena,
    pub(crate) collections: u64,
    pub(crate) relocations: u64,
    pub(crate) reclaimed: u64,
    /// Approximate bytes currently held by moving-heap allocations. It is
    /// charged at allocation and reset to the exact live footprint after a
    /// collection.
    pub(crate) allocation_charge: usize,
    /// A compiler safepoint is a cheap poll until `allocation_charge` reaches
    /// this threshold. After collection it tracks max(minimum, 2 * live).
    pub(crate) next_gc_threshold: usize,
    /// Deterministic stress mode for runtime tests. Production safepoints are
    /// always threshold-driven.
    #[cfg(test)]
    pub(crate) collect_on_every_poll: bool,
}

thread_local! {
    static ACTIVE_RUNTIME: Cell<*mut LoomRuntime> = const { Cell::new(ptr::null_mut()) };
    static ACTIVE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

fn enter_runtime(runtime: *mut LoomRuntime) -> bool {
    if runtime.is_null() {
        return false;
    }
    let current_runtime = ACTIVE_RUNTIME.with(Cell::get);
    let current_depth = ACTIVE_DEPTH.with(Cell::get);
    if (!current_runtime.is_null() && current_runtime != runtime) || current_depth == u32::MAX {
        return false;
    }
    // SAFETY: active_depth is atomic and remains live for the complete Runtime
    // lifetime. Do not touch any non-atomic Runtime field until this thread
    // acquires the inactive runtime or proves it owns the nested activation.
    let runtime_depth = unsafe { &(*runtime).active_depth };
    if runtime_depth
        .compare_exchange(
            current_depth,
            current_depth + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    if current_depth == 0 && unsafe { (*runtime).has_sync_roots() } {
        let restored = runtime_depth.compare_exchange(1, 0, Ordering::Release, Ordering::Relaxed);
        debug_assert!(restored.is_ok());
        return false;
    }
    if current_depth == 0 {
        ACTIVE_RUNTIME.with(|active| active.set(runtime));
    }
    ACTIVE_DEPTH.with(|depth| depth.set(current_depth + 1));
    true
}

fn leave_runtime() -> i32 {
    let runtime = ACTIVE_RUNTIME.with(Cell::get);
    let depth = ACTIVE_DEPTH.with(Cell::get);
    if runtime.is_null() || depth == 0 {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: ACTIVE_RUNTIME is installed only from a successful activation
    // owned by this thread.
    if unsafe { (*runtime).has_sync_roots() } {
        return GC_ROOT_STACK_NOT_EMPTY;
    }
    let runtime_depth = unsafe { &(*runtime).active_depth };
    if runtime_depth
        .compare_exchange(depth, depth - 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return GC_INVALID_ARGUMENT;
    }
    let remaining = depth - 1;
    ACTIVE_DEPTH.with(|active_depth| active_depth.set(remaining));
    if remaining == 0 {
        ACTIVE_RUNTIME.with(|active| active.set(ptr::null_mut()));
    }
    GC_OK
}

pub(crate) fn runtime_is_active(runtime: *mut LoomRuntime) -> bool {
    !runtime.is_null() && unsafe { &(*runtime).active_depth }.load(Ordering::Acquire) != 0
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
    if !enter_runtime(unsafe { (*executor).runtime_pointer() }) {
        std::process::abort();
    }
}

pub(crate) fn leave_executor() {
    if leave_runtime() != GC_OK {
        std::process::abort();
    }
}

/// Activates a standalone runtime heap for synchronous generated code.
#[unsafe(export_name = "loom_runtime_activate_v1")]
pub unsafe extern "C" fn activate_runtime_v1(runtime: *mut LoomRuntime) -> i32 {
    if enter_runtime(runtime) {
        GC_OK
    } else {
        GC_INVALID_ARGUMENT
    }
}

#[unsafe(export_name = "loom_runtime_deactivate_v1")]
pub unsafe extern "C" fn deactivate_runtime_v1(runtime: *mut LoomRuntime) -> i32 {
    if runtime.is_null()
        || !ACTIVE_RUNTIME.with(|active| active.get() == runtime)
        || ACTIVE_DEPTH.with(|depth| depth.get() == 0)
    {
        return GC_INVALID_ARGUMENT;
    }
    leave_runtime()
}

unsafe fn validate_root_descriptor(
    descriptor: *const LoomGcRootDescriptor,
    validate_every_state: bool,
) -> i32 {
    let Some(descriptor) = (unsafe { descriptor.as_ref() }) else {
        return GC_INVALID_ARGUMENT;
    };
    if descriptor.abi_version != SHADOW_STACK_ABI_VERSION {
        return GC_ABI_MISMATCH;
    }
    if descriptor.flags != 0 {
        return GC_INVALID_ARGUMENT;
    }
    let (Ok(slot_count), Ok(state_count), Ok(bitmap_words)) = (
        usize::try_from(descriptor.slot_count),
        usize::try_from(descriptor.state_count),
        usize::try_from(descriptor.live_bitmap_words),
    ) else {
        return GC_INVALID_ARGUMENT;
    };
    if slot_count == 0
        || state_count == 0
        || bitmap_words != slot_count.div_ceil(64)
        || descriptor.live_bitmaps.is_null()
    {
        return GC_INVALID_ARGUMENT;
    }
    debug_assert_eq!(size_of::<ValueSlot>(), VALUE_SLOT_WORDS * size_of::<u64>());
    let Some(total_words) = state_count.checked_mul(bitmap_words) else {
        return GC_INVALID_ARGUMENT;
    };
    if total_words > isize::MAX as usize / size_of::<u64>() {
        return GC_INVALID_ARGUMENT;
    }
    let tail_bits = slot_count % 64;
    if validate_every_state && tail_bits != 0 {
        let allowed = (1_u64 << tail_bits) - 1;
        for state in 0..state_count {
            let Some(index) = state
                .checked_mul(bitmap_words)
                .and_then(|row| row.checked_add(bitmap_words - 1))
            else {
                return GC_INVALID_ARGUMENT;
            };
            // SAFETY: the immutable descriptor contract provides the checked
            // state_count * live_bitmap_words table for every linked frame.
            if unsafe { *descriptor.live_bitmaps.add(index) } & !allowed != 0 {
                return GC_INVALID_ARGUMENT;
            }
        }
    }
    GC_OK
}

unsafe fn validate_root_frame(frame: *const LoomGcRootFrame, linked: bool) -> i32 {
    let Some(frame) = (unsafe { frame.as_ref() }) else {
        return GC_INVALID_ARGUMENT;
    };
    if frame.abi_version != SHADOW_STACK_ABI_VERSION {
        return GC_ABI_MISMATCH;
    }
    let expected_flags = if linked { GC_ROOT_FRAME_LINKED } else { 0 };
    if frame.flags != expected_flags || (!linked && !frame.previous.is_null()) {
        return GC_INVALID_ARGUMENT;
    }
    let status = unsafe { validate_root_descriptor(frame.descriptor, !linked) };
    if status != GC_OK {
        return status;
    }
    // SAFETY: descriptor validation established this immutable descriptor.
    let descriptor = unsafe { &*frame.descriptor };
    if frame.state >= descriptor.state_count || frame.slots.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    let (Ok(slot_count), Ok(bitmap_words), Ok(state)) = (
        usize::try_from(descriptor.slot_count),
        usize::try_from(descriptor.live_bitmap_words),
        usize::try_from(frame.state),
    ) else {
        return GC_INVALID_ARGUMENT;
    };
    let Some(bitmap_row) = state.checked_mul(bitmap_words) else {
        return GC_INVALID_ARGUMENT;
    };
    let tail_bits = slot_count % 64;
    if tail_bits != 0 {
        let allowed = (1_u64 << tail_bits) - 1;
        let tail = unsafe { *descriptor.live_bitmaps.add(bitmap_row + bitmap_words - 1) };
        if tail & !allowed != 0 {
            return GC_INVALID_ARGUMENT;
        }
    }
    if !linked {
        for index in 0..slot_count {
            // The slot-pointer array is immutable while the frame is linked,
            // so checking every entry once keeps safepoint polling O(depth).
            if unsafe { (*frame.slots.add(index)).is_null() } {
                return GC_INVALID_ARGUMENT;
            }
        }
    }
    GC_OK
}

unsafe fn visit_sync_roots(
    mut frame: *mut LoomGcRootFrame,
    depth: u64,
    visitor: Option<LoomTraceVisitor>,
    context: *mut c_void,
) -> i32 {
    if frame.is_null() != (depth == 0) {
        return GC_FRAME_ORDER;
    }
    let Ok(depth) = usize::try_from(depth) else {
        return GC_FRAME_ORDER;
    };
    for _ in 0..depth {
        if frame.is_null() {
            return GC_FRAME_ORDER;
        }
        let status = unsafe { validate_root_frame(frame, true) };
        if status != GC_OK {
            return status;
        }
        // SAFETY: validation established a live descriptor, bitmap row and
        // slot-pointer array for this compiler-created frame.
        let frame_ref = unsafe { &*frame };
        let descriptor = unsafe { &*frame_ref.descriptor };
        let slot_count = usize::try_from(descriptor.slot_count).unwrap_or_else(|_| unreachable!());
        let bitmap_words =
            usize::try_from(descriptor.live_bitmap_words).unwrap_or_else(|_| unreachable!());
        let state = usize::try_from(frame_ref.state).unwrap_or_else(|_| unreachable!());
        let bitmap_row = state
            .checked_mul(bitmap_words)
            .unwrap_or_else(|| unreachable!());
        if let Some(visitor) = visitor {
            for index in 0..slot_count {
                let word = unsafe { *descriptor.live_bitmaps.add(bitmap_row + index / 64) };
                if word & (1_u64 << (index % 64)) != 0 {
                    let root = unsafe { *frame_ref.slots.add(index) };
                    unsafe { visitor(root, context) };
                }
            }
        }
        // SAFETY: a linked frame's previous field is runtime-owned state.
        frame = unsafe { (*frame).previous };
    }
    if frame.is_null() {
        GC_OK
    } else {
        GC_FRAME_ORDER
    }
}

/// Links one compiler-described native frame into the active Runtime's precise
/// shadow stack. Push is allocation-free and collection is never triggered by
/// this operation.
#[unsafe(export_name = "loom_gc_root_push_v1")]
pub unsafe extern "C" fn root_push_v1(frame: *mut LoomGcRootFrame) -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    let status = unsafe { validate_root_frame(frame, false) };
    if status != GC_OK {
        return status;
    }
    // SAFETY: ACTIVE_RUNTIME gives exclusive generated-code access to its
    // process-local root chain. The validated frame remains live until pop.
    let runtime = unsafe { &mut *runtime };
    let Some(depth) = runtime.sync_root_depth.checked_add(1) else {
        return GC_INVALID_ARGUMENT;
    };
    unsafe {
        (*frame).previous = runtime.sync_root_top;
        (*frame).flags = GC_ROOT_FRAME_LINKED;
    }
    runtime.sync_root_top = frame;
    runtime.sync_root_depth = depth;
    GC_OK
}

/// Pops exactly the active Runtime's most recently pushed native root frame.
#[unsafe(export_name = "loom_gc_root_pop_v1")]
pub unsafe extern "C" fn root_pop_v1(frame: *mut LoomGcRootFrame) -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() || frame.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: ACTIVE_RUNTIME serializes root-chain mutation.
    let runtime = unsafe { &mut *runtime };
    if runtime.sync_root_top != frame || runtime.sync_root_depth == 0 {
        return GC_FRAME_ORDER;
    }
    let status = unsafe { validate_root_frame(frame, true) };
    if status != GC_OK {
        return status;
    }
    // SAFETY: top identity and linked-frame validation established ownership
    // of these runtime-maintained fields.
    runtime.sync_root_top = unsafe { (*frame).previous };
    runtime.sync_root_depth -= 1;
    unsafe {
        (*frame).previous = ptr::null_mut();
        (*frame).flags = 0;
    }
    if runtime.sync_root_top.is_null() != (runtime.sync_root_depth == 0) {
        std::process::abort();
    }
    GC_OK
}

#[unsafe(export_name = "loom_gc_alloc_value")]
pub extern "C" fn allocate_value() -> *mut c_void {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return ptr::null_mut();
    }
    let mut allocation = Box::new(ValueSlot::default());
    let pointer = (&raw mut *allocation).cast::<c_void>();
    // SAFETY: ACTIVE_RUNTIME is set only around a single-threaded generated
    // interval and allocation itself never invokes collection.
    unsafe {
        (*runtime).heap.values.push(allocation);
        (*runtime).heap.allocation_charge = (*runtime)
            .heap
            .allocation_charge
            .saturating_add(size_of::<ValueSlot>());
    }
    pointer
}

#[unsafe(export_name = "loom_gc_alloc_value_node")]
pub extern "C" fn allocate_value_node() -> *mut c_void {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return ptr::null_mut();
    }
    let mut allocation = Box::new(ValueNode {
        value: ValueSlot::default(),
        next: ptr::null_mut(),
    });
    let pointer = (&raw mut *allocation).cast::<c_void>();
    // SAFETY: see allocate_value.
    unsafe {
        (*runtime).heap.nodes.push(allocation);
        (*runtime).heap.allocation_charge = (*runtime)
            .heap
            .allocation_charge
            .saturating_add(size_of::<ValueNode>());
    }
    pointer
}

fn retain_sequence(allocation: Box<[u64]>, object: *mut c_void) -> Option<*mut c_void> {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return None;
    }
    // SAFETY: see allocate_value. Sequence objects are immutable after
    // publication and collection runs only at generated safepoints.
    let charge = allocation.len().saturating_mul(size_of::<u64>());
    unsafe {
        (*runtime).heap.sequences.push(allocation);
        (*runtime).heap.allocation_charge =
            (*runtime).heap.allocation_charge.saturating_add(charge);
    }
    Some(object)
}

pub(crate) fn retain_text(bytes: &[u8]) -> Option<*mut c_void> {
    let (allocation, object) = text::allocate_text_storage(bytes)?;
    retain_sequence(allocation, object.cast())
}

pub(crate) fn retain_byte_sequence(bytes: &[u8]) -> Option<*mut c_void> {
    let (allocation, object) = text::allocate_byte_storage(bytes)?;
    retain_sequence(allocation, object.cast())
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

/// Deep-clones one immutable conformance proof into the active Runtime's
/// non-moving, traced proof arena.
///
/// The clone is not published until its complete prerequisite DAG has been
/// validated. Allocation itself never collects; generated code must store the
/// returned root in an initialized owned `dyn` value before its next
/// safepoint.
#[unsafe(export_name = "loom_gc_clone_witness_v1")]
pub unsafe extern "C" fn clone_witness_v1(
    source: *const LoomWitnessInstance,
) -> *const LoomWitnessInstance {
    let runtime = active_runtime_pointer();
    if runtime.is_null() || source.is_null() {
        return ptr::null();
    }
    let Some(staged) = (unsafe { clone_witnesses(&[source]) }) else {
        return ptr::null();
    };
    let charge = staged.allocation_bytes();
    // SAFETY: ACTIVE_RUNTIME is installed only around one generated-code
    // interval. Adopting stable Boxes cannot invalidate the staged root.
    let heap = unsafe { &mut (*runtime).heap };
    let roots = heap.witnesses.adopt(staged);
    heap.allocation_charge = heap.allocation_charge.saturating_add(charge);
    roots[0]
}

struct HeapIndex {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
    sequences: HashSet<usize>,
    witnesses: HashSet<usize>,
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
            witnesses: heap.witnesses.addresses().collect(),
        }
    }
}

#[derive(Default)]
struct Marks {
    values: HashSet<usize>,
    nodes: HashSet<usize>,
    sequences: HashSet<usize>,
    witnesses: HashSet<usize>,
    invalid_witness: bool,
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
        VALUE_TAG_REFINED => {
            trace_value_pointer(
                value.words[VALUE_WORD_DATA] as *const ValueSlot,
                index,
                marks,
            );
        }
        VALUE_TAG_DYN => {
            trace_value_pointer(
                value.words[VALUE_WORD_DATA] as *const ValueSlot,
                index,
                marks,
            );
            trace_witness_pointer(
                value.words[VALUE_WORD_WITNESS] as *const LoomWitnessInstance,
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

fn trace_witness_pointer(
    pointer: *const LoomWitnessInstance,
    index: &HeapIndex,
    marks: &mut Marks,
) {
    let valid = unsafe {
        walk_witnesses(pointer, |instance| {
            let address = instance as usize;
            if index.witnesses.contains(&address) {
                marks.witnesses.insert(address);
            }
        })
    };
    if !valid {
        marks.invalid_witness = true;
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

#[cfg(test)]
pub(crate) fn collect(executor: &mut LoomExecutor) {
    collect_executor(executor, true);
}

pub(crate) fn poll(executor: &mut LoomExecutor) {
    collect_executor(executor, false);
}

fn collect_executor(executor: &mut LoomExecutor, force: bool) {
    let runtime = executor.runtime_pointer();
    // SAFETY: the attached runtime is a stable allocation distinct from the
    // executor. Scheduler collection runs outside generated code, so a valid
    // execution has no synchronous native frames at this boundary.
    let runtime_ref = unsafe { &mut *runtime };
    let root_top = runtime_ref.sync_root_top;
    let root_depth = runtime_ref.sync_root_depth;
    // SAFETY: the attached runtime and executor are separate stable
    // allocations. The scheduler owns `&mut executor` at this safepoint, so
    // no generated code can access the heap while its task roots are traced.
    let status = unsafe {
        collect_heap(
            &mut runtime_ref.heap,
            &mut executor.tasks,
            root_top,
            root_depth,
            force,
        )
    };
    if status != GC_OK {
        std::process::abort();
    }
}

/// Runs an explicit precise moving collection for the active Runtime.
///
/// Allocation never invokes this entry point implicitly. The compiler must
/// publish every live native `Value` in a pushed root frame before calling the
/// safepoint; attached coroutine Task roots are traced in the same collection.
#[unsafe(export_name = "loom_gc_safepoint_v1")]
pub unsafe extern "C" fn safepoint_v1() -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    unsafe { collect_active_runtime(runtime, false) }
}

unsafe fn collect_active_runtime(runtime: *mut LoomRuntime, force: bool) -> i32 {
    // SAFETY: the caller obtained this pointer from ACTIVE_RUNTIME and owns
    // the only generated-code interval allowed to mutate its heap and roots.
    let runtime_ref = unsafe { &mut *runtime };
    let root_top = runtime_ref.sync_root_top;
    let root_depth = runtime_ref.sync_root_depth;
    let executor = runtime_ref
        .attached_executor_pointer()
        .cast::<LoomExecutor>();
    if executor.is_null() {
        let mut no_tasks: [Box<LoomTask>; 0] = [];
        return unsafe {
            collect_heap(
                &mut runtime_ref.heap,
                &mut no_tasks,
                root_top,
                root_depth,
                force,
            )
        };
    }
    // SAFETY: a Runtime has at most one stable executor attachment. The
    // active generated-code interval is single-threaded with its scheduler.
    let executor_ref = unsafe { &mut *executor };
    if executor_ref.runtime_pointer() != runtime {
        return GC_INVALID_ARGUMENT;
    }
    unsafe {
        collect_heap(
            &mut runtime_ref.heap,
            &mut executor_ref.tasks,
            root_top,
            root_depth,
            force,
        )
    }
}

unsafe fn collect_heap(
    heap: &mut LoomHeap,
    tasks: &mut [Box<LoomTask>],
    root_top: *mut LoomGcRootFrame,
    root_depth: u64,
    force: bool,
) -> i32 {
    let root_status = unsafe { visit_sync_roots(root_top, root_depth, None, ptr::null_mut()) };
    if root_status != GC_OK {
        return root_status;
    }
    let should_collect = force || heap.allocation_charge >= heap.next_gc_threshold;
    #[cfg(test)]
    let should_collect = should_collect || heap.collect_on_every_poll;
    if !should_collect {
        return GC_OK;
    }
    heap.collections = heap.collections.saturating_add(1);
    // List indexes are derived accelerators, not roots. Drop every raw node
    // pointer before filtering or relocating the heap; the next add/get lazily
    // rebuilds an index from the rewritten List head.
    heap.list_node_indexes.clear();
    if heap.values.is_empty()
        && heap.nodes.is_empty()
        && heap.sequences.is_empty()
        && heap.witnesses.is_empty()
    {
        heap.allocation_charge = 0;
        heap.next_gc_threshold = MIN_GC_THRESHOLD_BYTES;
        return GC_OK;
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
    let root_status = unsafe {
        visit_sync_roots(
            root_top,
            root_depth,
            Some(trace_slot),
            (&raw mut trace_context).cast(),
        )
    };
    if root_status != GC_OK {
        return root_status;
    }
    if marks.invalid_witness {
        return GC_INVALID_ARGUMENT;
    }

    let before = heap
        .values
        .len()
        .saturating_add(heap.nodes.len())
        .saturating_add(heap.sequences.len())
        .saturating_add(heap.witnesses.len());
    heap.values
        .retain(|value| marks.values.contains(&((&raw const **value) as usize)));
    heap.nodes
        .retain(|node| marks.nodes.contains(&((&raw const **node) as usize)));
    heap.sequences
        .retain(|sequence| marks.sequences.contains(&(sequence.as_ptr() as usize)));
    heap.witnesses.retain_marked(&marks.witnesses);
    let after = heap
        .values
        .len()
        .saturating_add(heap.nodes.len())
        .saturating_add(heap.sequences.len())
        .saturating_add(heap.witnesses.len());
    heap.reclaimed = heap
        .reclaimed
        .saturating_add((before.saturating_sub(after)) as u64);

    unsafe { relocate_marked_heap(heap, tasks, root_top, root_depth) }
}

struct FromSpace {
    values: Vec<Box<ValueSlot>>,
    nodes: Vec<Box<ValueNode>>,
    sequences: Vec<Box<[u64]>>,
}

struct HeapRelocation {
    from_space: FromSpace,
    values: HashMap<usize, *mut ValueSlot>,
    nodes: HashMap<usize, *mut ValueNode>,
    sequences: HashMap<usize, *mut c_void>,
}

fn evacuate_marked_heap(heap: &mut LoomHeap) -> HeapRelocation {
    // Keep the complete from-space live until every replacement is allocated
    // and every reference is rewritten. Besides preventing accidental
    // use-after-free, this makes the old and new address sets disjoint, so
    // rewriting aliased/nested root descriptions is naturally idempotent.
    let from_space = FromSpace {
        values: std::mem::take(&mut heap.values),
        nodes: std::mem::take(&mut heap.nodes),
        sequences: std::mem::take(&mut heap.sequences),
    };

    let mut values = Vec::with_capacity(from_space.values.len());
    let mut value_moves = HashMap::with_capacity(from_space.values.len());
    for value in &from_space.values {
        let old = (&raw const **value) as usize;
        let mut replacement = Box::new(**value);
        let new = &raw mut *replacement;
        values.push(replacement);
        value_moves.insert(old, new);
    }
    let mut nodes = Vec::with_capacity(from_space.nodes.len());
    let mut node_moves = HashMap::with_capacity(from_space.nodes.len());
    for node in &from_space.nodes {
        let old = (&raw const **node) as usize;
        let mut replacement = Box::new(ValueNode {
            value: node.value,
            next: node.next,
        });
        let new = &raw mut *replacement;
        nodes.push(replacement);
        node_moves.insert(old, new);
    }
    let mut sequences = Vec::with_capacity(from_space.sequences.len());
    let mut sequence_moves = HashMap::with_capacity(from_space.sequences.len());
    for sequence in &from_space.sequences {
        let old = sequence.as_ptr() as usize;
        let mut replacement = sequence.to_vec().into_boxed_slice();
        let new = replacement.as_mut_ptr().cast::<c_void>();
        sequences.push(replacement);
        sequence_moves.insert(old, new);
    }
    heap.values = values;
    heap.nodes = nodes;
    heap.sequences = sequences;
    debug_assert!(
        value_moves
            .values()
            .all(|pointer| !value_moves.contains_key(&(*pointer as usize)))
    );
    debug_assert!(
        node_moves
            .values()
            .all(|pointer| !node_moves.contains_key(&(*pointer as usize)))
    );
    debug_assert!(
        sequence_moves
            .values()
            .all(|pointer| !sequence_moves.contains_key(&(*pointer as usize)))
    );
    HeapRelocation {
        from_space,
        values: value_moves,
        nodes: node_moves,
        sequences: sequence_moves,
    }
}

unsafe fn relocate_marked_heap(
    heap: &mut LoomHeap,
    tasks: &mut [Box<LoomTask>],
    root_top: *mut LoomGcRootFrame,
    root_depth: u64,
) -> i32 {
    let HeapRelocation {
        from_space,
        values: value_moves,
        nodes: node_moves,
        sequences: sequence_moves,
    } = evacuate_marked_heap(heap);
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
    let root_status = unsafe {
        visit_sync_roots(
            root_top,
            root_depth,
            Some(rewrite_slot),
            (&raw mut rewrite_context).cast(),
        )
    };
    if root_status != GC_OK {
        return root_status;
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
    drop(from_space);
    let live_bytes = heap
        .values
        .len()
        .saturating_mul(size_of::<ValueSlot>())
        .saturating_add(heap.nodes.len().saturating_mul(size_of::<ValueNode>()))
        .saturating_add(
            heap.sequences
                .iter()
                .map(|sequence| sequence.len().saturating_mul(size_of::<u64>()))
                .fold(0_usize, usize::saturating_add),
        )
        .saturating_add(heap.witnesses.allocation_bytes());
    heap.allocation_charge = live_bytes;
    heap.next_gc_threshold = live_bytes.saturating_mul(2).max(MIN_GC_THRESHOLD_BYTES);
    GC_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};
    use crate::scheduler::{task_slot, task_spawn};

    struct TestRootFrame<const ROOTS: usize> {
        roots: Box<[ValueSlot; ROOTS]>,
        slots: Box<[*mut c_void; ROOTS]>,
        _live_bitmaps: Box<[u64]>,
        descriptor: Box<LoomGcRootDescriptor>,
        header: Box<LoomGcRootFrame>,
    }

    impl<const ROOTS: usize> TestRootFrame<ROOTS> {
        fn new(state_count: usize, live_bitmaps: &[u64]) -> Self {
            assert!(ROOTS > 0 && state_count > 0);
            let bitmap_words = ROOTS.div_ceil(64);
            assert_eq!(live_bitmaps.len(), state_count * bitmap_words);
            let mut roots = Box::new([ValueSlot::default(); ROOTS]);
            let slots = Box::new(std::array::from_fn(|index| {
                (&raw mut roots[index]).cast::<c_void>()
            }));
            let live_bitmaps = live_bitmaps.to_vec().into_boxed_slice();
            let descriptor = Box::new(LoomGcRootDescriptor {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: ROOTS as u64,
                state_count: state_count as u64,
                live_bitmap_words: bitmap_words as u64,
                live_bitmaps: live_bitmaps.as_ptr(),
            });
            let header = Box::new(LoomGcRootFrame {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 0,
                descriptor: &raw const *descriptor,
                slots: slots.as_ptr(),
                previous: ptr::null_mut(),
            });
            Self {
                roots,
                slots,
                _live_bitmaps: live_bitmaps,
                descriptor,
                header,
            }
        }

        fn all_live() -> Self {
            let bitmap_words = ROOTS.div_ceil(64);
            let mut bitmaps = vec![u64::MAX; bitmap_words];
            let tail = ROOTS % 64;
            if tail != 0 {
                bitmaps[bitmap_words - 1] = (1_u64 << tail) - 1;
            }
            Self::new(1, &bitmaps)
        }

        fn pointer(&mut self) -> *mut LoomGcRootFrame {
            &raw mut *self.header
        }
    }

    fn indirect(pointer: *mut ValueSlot) -> ValueSlot {
        let mut value = ValueSlot::default();
        value.words[VALUE_WORD_TAG] = VALUE_TAG_REFINED;
        value.words[VALUE_WORD_DATA] = pointer as u64;
        value
    }

    unsafe extern "C" fn completed_task(_task: *mut LoomTask, _executor: *mut LoomExecutor) -> i32 {
        TASK_COMPLETED
    }

    unsafe fn force_next_safepoint(runtime: *mut LoomRuntime) {
        unsafe { (*runtime).heap.next_gc_threshold = 0 };
    }

    #[test]
    fn standalone_safepoint_moves_live_root_and_reclaims_dead_allocation() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let mut frame = TestRootFrame::<1>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            let live = allocate_value().cast::<ValueSlot>();
            let dead = allocate_value().cast::<ValueSlot>();
            assert!(!live.is_null() && !dead.is_null());
            frame.roots[0] = indirect(live);
            assert_eq!((*runtime).heap.collections, 0);

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            let moved = frame.roots[0].words[VALUE_WORD_DATA] as *mut ValueSlot;
            assert!(!moved.is_null());
            assert_ne!(moved, live);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_eq!((*runtime).heap.reclaimed, 1);

            frame.roots[0] = ValueSlot::default();
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert!((*runtime).heap.values.is_empty());
            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn nested_frames_are_traced_and_popped_in_strict_order() {
        let runtime = runtime_create_v1();
        let mut outer = TestRootFrame::<1>::all_live();
        let mut inner = TestRootFrame::<1>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(outer.pointer()), GC_OK);
            outer.roots[0] = indirect(allocate_value().cast());
            assert_eq!(root_push_v1(inner.pointer()), GC_OK);
            inner.roots[0] = indirect(allocate_value().cast());

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.values.len(), 2);
            let outer_after_first = outer.roots[0].words[VALUE_WORD_DATA];
            assert_eq!(root_pop_v1(inner.pointer()), GC_OK);
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_ne!(outer.roots[0].words[VALUE_WORD_DATA], outer_after_first);

            assert_eq!(root_pop_v1(outer.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn bad_lifo_versions_and_unpopped_deactivation_are_rejected() {
        let runtime = runtime_create_v1();
        let mut outer = TestRootFrame::<1>::all_live();
        let mut inner = TestRootFrame::<1>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(outer.pointer()), GC_OK);
            assert_eq!(root_push_v1(inner.pointer()), GC_OK);
            assert_eq!(root_pop_v1(outer.pointer()), GC_FRAME_ORDER);
            assert_eq!(deactivate_runtime_v1(runtime), GC_ROOT_STACK_NOT_EMPTY);
            assert_eq!(runtime_destroy_v1(runtime), GC_INVALID_ARGUMENT);
            assert_eq!(root_pop_v1(inner.pointer()), GC_OK);
            assert_eq!(root_pop_v1(outer.pointer()), GC_OK);

            let mut bad_descriptor_frame = TestRootFrame::<1>::all_live();
            bad_descriptor_frame.descriptor.abi_version = SHADOW_STACK_ABI_VERSION + 1;
            assert_eq!(
                root_push_v1(bad_descriptor_frame.pointer()),
                GC_ABI_MISMATCH,
            );
            let mut bad_frame = TestRootFrame::<1>::all_live();
            bad_frame.header.abi_version = SHADOW_STACK_ABI_VERSION + 1;
            assert_eq!(root_push_v1(bad_frame.pointer()), GC_ABI_MISMATCH);

            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn active_safepoint_unifies_task_and_native_frame_roots() {
        let runtime = runtime_create_v1();
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let task = unsafe { task_spawn(executor, Some(completed_task), 1, 0) };
        assert!(!task.is_null());
        let mut frame = TestRootFrame::<1>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            let task_live = allocate_value().cast::<ValueSlot>();
            let frame_live = allocate_value().cast::<ValueSlot>();
            let dead = allocate_value().cast::<ValueSlot>();
            assert!(!task_live.is_null() && !frame_live.is_null() && !dead.is_null());
            *task_slot(task, 0).cast::<ValueSlot>() = indirect(task_live);
            frame.roots[0] = indirect(frame_live);

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.values.len(), 2);
            assert_ne!(
                (*task_slot(task, 0).cast::<ValueSlot>()).words[VALUE_WORD_DATA],
                task_live as u64,
            );
            assert_ne!(frame.roots[0].words[VALUE_WORD_DATA], frame_live as u64);

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn relocation_is_idempotent_for_the_same_task_slot_in_nested_root_frames() {
        let runtime = runtime_create_v1();
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let task = unsafe { task_spawn(executor, Some(completed_task), 1, 0) };
        assert!(!task.is_null());
        let task_root = unsafe { task_slot(task, 0).cast::<ValueSlot>() };
        assert!(!task_root.is_null());
        let mut outer = TestRootFrame::<1>::all_live();
        let mut inner = TestRootFrame::<1>::all_live();
        outer.slots[0] = task_root.cast();
        outer.header.slots = outer.slots.as_ptr();
        inner.slots[0] = task_root.cast();
        inner.header.slots = inner.slots.as_ptr();

        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(outer.pointer()), GC_OK);
            assert_eq!(root_push_v1(inner.pointer()), GC_OK);

            let tail = allocate_value_node().cast::<ValueNode>();
            let head = allocate_value_node().cast::<ValueNode>();
            let child = allocate_value().cast::<ValueSlot>();
            assert!(!head.is_null() && !tail.is_null());
            assert!(!child.is_null());
            (*child).words[VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_INT;
            (*child).words[VALUE_WORD_SCALAR] = 42;
            (*tail).value = text_value(b"rooted text").expect("retain test Text");
            let text_before = (*tail).value.words[VALUE_WORD_DATA];
            (*tail).next = ptr::null_mut();
            (*head).value.words[VALUE_WORD_TAG] = VALUE_TAG_REFINED;
            (*head).value.words[VALUE_WORD_DATA] = child as u64;
            (*head).next = tail;
            (*task_root).words[VALUE_WORD_TAG] = VALUE_TAG_RECORD;
            (*task_root).words[VALUE_WORD_AUX] = 2;
            (*task_root).words[VALUE_WORD_DATA] = head as u64;

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            let moved_head = (*task_root).words[VALUE_WORD_DATA] as *mut ValueNode;
            assert!(!moved_head.is_null());
            assert_ne!(moved_head, head);
            assert_eq!((*moved_head).value.words[VALUE_WORD_TAG], VALUE_TAG_REFINED,);
            let moved_child = (*moved_head).value.words[VALUE_WORD_DATA] as *mut ValueSlot;
            assert!(!moved_child.is_null());
            assert_ne!(moved_child, child);
            assert_eq!(
                (*moved_child).words[VALUE_WORD_TAG],
                loom_runtime_abi::VALUE_TAG_INT,
            );
            assert_eq!((*moved_child).words[VALUE_WORD_SCALAR], 42);
            let moved_tail = (*moved_head).next;
            assert!(!moved_tail.is_null());
            assert_eq!((*moved_tail).value.words[VALUE_WORD_TAG], VALUE_TAG_TEXT,);
            assert_ne!((*moved_tail).value.words[VALUE_WORD_DATA], text_before);
            assert_eq!(
                text::text_value_bytes(&(*moved_tail).value),
                Some(&b"rooted text"[..]),
            );
            assert!((*moved_tail).next.is_null());

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            let twice_moved_head = (*task_root).words[VALUE_WORD_DATA] as *mut ValueNode;
            assert!(!twice_moved_head.is_null());
            assert_ne!(twice_moved_head, moved_head);
            let twice_moved_tail = (*twice_moved_head).next;
            assert!(!twice_moved_tail.is_null());
            assert_eq!(
                text::text_value_bytes(&(*twice_moved_tail).value),
                Some(&b"rooted text"[..]),
            );

            assert_eq!(root_pop_v1(inner.pointer()), GC_OK);
            assert_eq!(root_pop_v1(outer.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn managed_allocators_fail_closed_without_an_active_runtime() {
        assert!(active_runtime_pointer().is_null());
        assert!(allocate_value().is_null());
        assert!(allocate_value_node().is_null());
        assert!(unsafe { clone_witness_v1(ptr::null()) }.is_null());
        assert!(retain_text(b"not leaked").is_none());
        assert!(retain_byte_sequence(b"not leaked").is_none());

        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        assert!(allocate_value().is_null());
        unsafe {
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn owned_dyn_traces_nonmoving_proof_dag_and_sweeps_it_when_dead() {
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<1>::all_live();
        let leaf_descriptor = loom_runtime_abi::LoomWitnessDescriptor {
            prerequisite_count: 0,
            method_count: 0,
            methods: ptr::null(),
        };
        let applied_descriptor = loom_runtime_abi::LoomWitnessDescriptor {
            prerequisite_count: 1,
            method_count: 0,
            methods: ptr::null(),
        };
        let leaf = LoomWitnessInstance {
            descriptor: &raw const leaf_descriptor,
            prerequisites: ptr::null(),
        };
        let prerequisites = [&raw const leaf];
        let applied = LoomWitnessInstance {
            descriptor: &raw const applied_descriptor,
            prerequisites: prerequisites.as_ptr(),
        };
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            let data = allocate_value().cast::<ValueSlot>();
            let witness = clone_witness_v1(&raw const applied);
            assert!(!data.is_null() && !witness.is_null());
            frame.roots[0].words[VALUE_WORD_TAG] = VALUE_TAG_DYN;
            frame.roots[0].words[VALUE_WORD_DATA] = data as u64;
            frame.roots[0].words[VALUE_WORD_WITNESS] = witness as u64;
            assert_eq!((*runtime).heap.witnesses.len(), 2);

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_eq!((*runtime).heap.witnesses.len(), 2);
            assert_ne!(frame.roots[0].words[VALUE_WORD_DATA], data as u64);
            assert_eq!(frame.roots[0].words[VALUE_WORD_WITNESS], witness as u64);

            frame.roots[0] = ValueSlot::default();
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert!((*runtime).heap.values.is_empty());
            assert!((*runtime).heap.witnesses.is_empty());
            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn current_state_selects_only_compiler_proven_live_slots() {
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<2>::new(2, &[0b01, 0b10]);
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            let first = allocate_value().cast::<ValueSlot>();
            let initially_dead = allocate_value().cast::<ValueSlot>();
            frame.roots[0] = indirect(first);
            frame.roots[1] = indirect(initially_dead);
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_ne!(frame.roots[0].words[VALUE_WORD_DATA], first as u64);
            assert_eq!(frame.roots[1].words[VALUE_WORD_DATA], initially_dead as u64);

            let second = allocate_value().cast::<ValueSlot>();
            frame.roots[1] = indirect(second);
            frame.header.state = 1;
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_ne!(frame.roots[1].words[VALUE_WORD_DATA], second as u64);

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn invalid_empty_state_tail_bits_and_state_index_are_rejected() {
        let runtime = runtime_create_v1();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);

            let empty_descriptor = LoomGcRootDescriptor {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: 0,
                state_count: 0,
                live_bitmap_words: 0,
                live_bitmaps: ptr::null(),
            };
            let mut empty_frame = LoomGcRootFrame {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 0,
                descriptor: &raw const empty_descriptor,
                slots: ptr::null(),
                previous: ptr::null_mut(),
            };
            assert_eq!(root_push_v1(&raw mut empty_frame), GC_INVALID_ARGUMENT,);

            let mut tail_bits = TestRootFrame::<1>::new(1, &[0b10]);
            assert_eq!(root_push_v1(tail_bits.pointer()), GC_INVALID_ARGUMENT);
            let mut bad_state = TestRootFrame::<1>::all_live();
            bad_state.header.state = 1;
            assert_eq!(root_push_v1(bad_state.pointer()), GC_INVALID_ARGUMENT);

            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn safepoint_polls_threshold_and_resizes_to_twice_live_space() {
        const ROOTS: usize = 1024;
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<ROOTS>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            for root in frame.roots.iter_mut() {
                *root = indirect(allocate_value().cast());
            }
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.collections, 0);

            (*runtime).heap.next_gc_threshold = (*runtime).heap.allocation_charge;
            assert_eq!(safepoint_v1(), GC_OK);
            let live_bytes = ROOTS * size_of::<ValueSlot>();
            assert_eq!((*runtime).heap.collections, 1);
            assert_eq!((*runtime).heap.allocation_charge, live_bytes);
            assert_eq!((*runtime).heap.next_gc_threshold, live_bytes * 2);

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }
}
