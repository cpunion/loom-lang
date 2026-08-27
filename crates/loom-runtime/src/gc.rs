//! Precise moving heap for compiler-generated Loom values.
//!
//! Collection runs at compiler-known synchronous safepoints, managed
//! allocation slowpaths, or between coroutine resume calls. Synchronous
//! generated code publishes precise shadow-stack roots; every value live
//! across `.await` is in a compiler-described Task slot. The runtime can
//! therefore relocate objects without pinning or exposing addresses to source
//! programs.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::{Cell, RefCell, UnsafeCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::{align_of, size_of};
use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering;

use loom_runtime_abi::{
    DYN_FLAG_MUTABLE, GC_ABI_MISMATCH, GC_DESCRIPTOR_INVALID, GC_FRAME_ORDER, GC_INVALID_ARGUMENT,
    GC_MAX_OBJECT_ALIGNMENT, GC_MAX_OBJECT_BYTES, GC_MAX_OBJECT_POINTERS, GC_MAX_ROOT_BITMAP_WORDS,
    GC_MAX_ROOT_DEPTH, GC_MAX_ROOT_SLOTS, GC_MAX_ROOT_STATES, GC_OK, GC_RESOURCE_LIMIT,
    GC_ROOT_FRAME_LINKED, GC_ROOT_STACK_NOT_EMPTY, LoomGcObjectDescriptor, LoomGcRootDescriptor,
    LoomGcRootFrame, LoomGcTypedRootDescriptor, LoomGcTypedRootFrame, LoomWitnessInstance,
    SHADOW_STACK_ABI_VERSION, TASK_COMPLETED, TYPED_GC_ABI_VERSION, TYPED_SHADOW_STACK_ABI_VERSION,
    VALUE_TAG_CONSTRAINT_ERROR, VALUE_TAG_DYN, VALUE_TAG_ENUM, VALUE_TAG_LIST, VALUE_TAG_RECORD,
    VALUE_TAG_REFINED, VALUE_TAG_TASK_OUTCOME, VALUE_TAG_TEXT, VALUE_TAG_TUPLE, VALUE_WORD_AUX,
    VALUE_WORD_DATA, VALUE_WORD_SCALAR, VALUE_WORD_TAG, VALUE_WORD_WITNESS,
};

use crate::reactor::LoomExecutor;
use crate::runtime::LoomRuntime;
use crate::scheduler::{
    LoomTask, LoomTraceVisitor, ValueNode, ValueSlot, trace_task_roots, trace_typed_task_roots,
};
use crate::text;
use crate::witness::{WitnessArena, clone_witnesses, walk_witnesses};

pub(crate) struct ListNodeIndex {
    pub(crate) length: u64,
    pub(crate) tail: *mut ValueNode,
    pub(crate) nodes: Option<Vec<*mut ValueNode>>,
}

pub(crate) const MIN_GC_THRESHOLD_BYTES: usize = 64 * 1024;

struct TypedAllocation {
    pointer: NonNull<u8>,
    layout: Layout,
    pointer_offsets: Box<[usize]>,
}

impl TypedAllocation {
    fn new(layout: Layout, pointer_offsets: Box<[usize]>) -> Self {
        // SAFETY: descriptor validation constructed a nonzero, valid Layout.
        let pointer = unsafe { alloc_zeroed(layout) };
        let Some(pointer) = NonNull::new(pointer) else {
            handle_alloc_error(layout);
        };
        Self {
            pointer,
            layout,
            pointer_offsets,
        }
    }

    fn evacuate(&self) -> Self {
        let replacement = Self::new(self.layout, self.pointer_offsets.clone());
        // SAFETY: both allocations have the same non-overlapping valid Layout.
        unsafe {
            ptr::copy_nonoverlapping(
                self.pointer.as_ptr(),
                replacement.pointer.as_ptr(),
                self.layout.size(),
            );
        }
        replacement
    }

    fn address(&self) -> usize {
        self.pointer.as_ptr() as usize
    }

    fn pointer(&self) -> *mut c_void {
        self.pointer.as_ptr().cast()
    }

    fn allocation_bytes(&self) -> usize {
        self.layout.size()
    }

    #[allow(clippy::cast_ptr_alignment)]
    unsafe fn pointer_cell(&self, offset: usize) -> *mut *mut c_void {
        debug_assert!(offset + size_of::<*mut c_void>() <= self.layout.size());
        // Descriptor validation guarantees both the allocation alignment and
        // the offset's pointer alignment before metadata reaches this object.
        unsafe { self.pointer.as_ptr().add(offset).cast::<*mut c_void>() }
    }
}

impl Drop for TypedAllocation {
    fn drop(&mut self) {
        // SAFETY: this allocation was created with the same Layout and remains
        // uniquely owned by this side table entry.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

/// Runtime-owned storage for managed Loom values.
///
/// The heap is deliberately independent from the async reactor and scheduler:
/// synchronous generated code needs managed allocation, but must not need an
/// operating-system poller or worker-completion channel. Compiler polls and
/// allocation slowpaths share the same precise task/shadow-stack root set.
#[derive(Default)]
pub(crate) struct LoomHeap {
    pub(crate) values: Vec<Box<ValueSlot>>,
    pub(crate) nodes: Vec<Box<ValueNode>>,
    pub(crate) sequences: Vec<Box<[u64]>>,
    /// Precisely described typed objects. Object bytes contain no universal
    /// tag; the side table owns copied fixed-pointer offsets for tracing.
    typed_objects: Vec<TypedAllocation>,
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
    /// Deterministic stress mode which turns every managed allocator call
    /// into a collection boundary. Unlike `collect_on_every_poll`, this also
    /// covers runtime helpers which do not issue an explicit safepoint.
    #[cfg(test)]
    pub(crate) collect_before_every_allocation: bool,
}

impl LoomHeap {
    pub(crate) fn new() -> Self {
        Self {
            next_gc_threshold: MIN_GC_THRESHOLD_BYTES,
            ..Self::default()
        }
    }

    pub(crate) fn typed_object_count(&self) -> usize {
        self.typed_objects.len()
    }
}

thread_local! {
    static ACTIVE_RUNTIME: Cell<*mut LoomRuntime> = const { Cell::new(ptr::null_mut()) };
    static ACTIVE_DEPTH: Cell<u32> = const { Cell::new(0) };
    static ACTIVE_ROOT_BASELINES: RefCell<Vec<RootBaseline>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct RootBaseline {
    sync_top: *mut LoomGcRootFrame,
    sync_depth: u64,
    typed_top: *mut LoomGcTypedRootFrame,
    typed_depth: u64,
}

#[derive(Clone)]
struct ActivationSnapshot {
    active_runtime: *mut LoomRuntime,
    active_depth: u32,
    root_baselines: Vec<RootBaseline>,
    runtime_depth: u32,
    roots: RootBaseline,
}

/// One recoverable activation around a non-suspending cleanup callback.
///
/// Ordinary generated-code intervals use `enter_executor`/`leave_executor`
/// and retain their fail-fast invariant checks. Cleanup callbacks are an ABI
/// boundary, however, and a malformed callback must not strand thread-local
/// activation state or a dangling stack-root pointer before the scheduler can
/// turn the violation into a typed Task fault.
pub(crate) struct RecoverableExecutorActivation {
    runtime: NonNull<LoomRuntime>,
    snapshot: ActivationSnapshot,
    restored: bool,
}

impl RecoverableExecutorActivation {
    pub(crate) fn enter(executor: *mut LoomExecutor) -> Option<Self> {
        if executor.is_null() {
            return None;
        }
        // SAFETY: scheduler callers retain a live executor and its attached
        // Runtime for the complete cleanup callback.
        let runtime = NonNull::new(unsafe { (*executor).runtime_pointer() })?;
        let active_runtime = ACTIVE_RUNTIME.with(Cell::get);
        let active_depth = ACTIVE_DEPTH.with(Cell::get);
        let root_baselines = ACTIVE_ROOT_BASELINES.with(|baselines| baselines.borrow().clone());
        let runtime_depth = unsafe { runtime.as_ref().active_depth.load(Ordering::Acquire) };
        let roots = unsafe { RootBaseline::capture(runtime.as_ptr()) };

        // A recoverable scope can nest only into the same internally
        // consistent activation that ordinary enter_runtime accepts. If its
        // entry state is already corrupt, do not call generated cleanup code.
        if active_runtime.is_null() != (active_depth == 0)
            || (!active_runtime.is_null() && active_runtime != runtime.as_ptr())
            || usize::try_from(active_depth).ok() != Some(root_baselines.len())
            || runtime_depth != active_depth
        {
            return None;
        }

        let snapshot = ActivationSnapshot {
            active_runtime,
            active_depth,
            root_baselines,
            runtime_depth,
            roots,
        };
        if !enter_runtime(runtime.as_ptr()) {
            return None;
        }
        Some(Self {
            runtime,
            snapshot,
            restored: false,
        })
    }

    fn expected_state_is_intact(&self) -> bool {
        let Some(expected_depth) = self.snapshot.active_depth.checked_add(1) else {
            return false;
        };
        let baselines_match = ACTIVE_ROOT_BASELINES.with(|baselines| {
            let baselines = baselines.borrow();
            baselines.len() == self.snapshot.root_baselines.len() + 1
                && baselines[..self.snapshot.root_baselines.len()] == self.snapshot.root_baselines
                && baselines.last().copied() == Some(self.snapshot.roots)
        });
        ACTIVE_RUNTIME.with(|active| active.get() == self.runtime.as_ptr())
            && ACTIVE_DEPTH.with(|depth| depth.get() == expected_depth)
            && baselines_match
            && unsafe {
                self.runtime.as_ref().active_depth.load(Ordering::Acquire) == expected_depth
                    && self.snapshot.roots.matches(self.runtime.as_ptr())
            }
    }

    fn restore(&mut self) -> bool {
        if self.restored {
            return true;
        }
        let intact = self.expected_state_is_intact();
        let observed_runtime = ACTIVE_RUNTIME.with(Cell::get);

        // A callback can deactivate this Runtime and activate another one
        // before returning. Such a switch is a protocol defect, but the
        // public activation ABI guarantees that the unexpected Runtime was
        // inactive and root-empty when this thread acquired it. Release that
        // leaked ownership before restoring the exact entry snapshot.
        if !observed_runtime.is_null()
            && observed_runtime != self.runtime.as_ptr()
            && observed_runtime != self.snapshot.active_runtime
        {
            unsafe {
                (*observed_runtime).sync_root_top = ptr::null_mut();
                (*observed_runtime).sync_root_depth = 0;
                (*observed_runtime).typed_root_top = ptr::null_mut();
                (*observed_runtime).typed_root_depth = 0;
                (*observed_runtime).active_depth.store(0, Ordering::Release);
            }
        }

        unsafe {
            let runtime = self.runtime.as_mut();
            runtime.sync_root_top = self.snapshot.roots.sync_top;
            runtime.sync_root_depth = self.snapshot.roots.sync_depth;
            runtime.typed_root_top = self.snapshot.roots.typed_top;
            runtime.typed_root_depth = self.snapshot.roots.typed_depth;
            runtime
                .active_depth
                .store(self.snapshot.runtime_depth, Ordering::Release);
        }
        ACTIVE_ROOT_BASELINES.with(|baselines| {
            baselines
                .borrow_mut()
                .clone_from(&self.snapshot.root_baselines);
        });
        ACTIVE_DEPTH.with(|depth| depth.set(self.snapshot.active_depth));
        ACTIVE_RUNTIME.with(|active| active.set(self.snapshot.active_runtime));
        self.restored = true;
        intact
    }

    /// Restores the exact state captured before entry and reports whether the
    /// callback returned with the one activation and both root chains intact.
    pub(crate) fn finish(mut self) -> bool {
        self.restore()
    }
}

impl Drop for RecoverableExecutorActivation {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.restore();
        }
    }
}

impl RootBaseline {
    unsafe fn capture(runtime: *const LoomRuntime) -> Self {
        unsafe {
            Self {
                sync_top: (*runtime).sync_root_top,
                sync_depth: (*runtime).sync_root_depth,
                typed_top: (*runtime).typed_root_top,
                typed_depth: (*runtime).typed_root_depth,
            }
        }
    }

    unsafe fn matches(self, runtime: *const LoomRuntime) -> bool {
        unsafe {
            (*runtime).sync_root_top == self.sync_top
                && (*runtime).sync_root_depth == self.sync_depth
                && (*runtime).typed_root_top == self.typed_top
                && (*runtime).typed_root_depth == self.typed_depth
        }
    }
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
    // SAFETY: the successful atomic transition either acquired an inactive
    // Runtime or proved this thread owns the nested activation.
    let baseline = unsafe { RootBaseline::capture(runtime) };
    ACTIVE_ROOT_BASELINES.with(|baselines| baselines.borrow_mut().push(baseline));
    ACTIVE_DEPTH.with(|depth| depth.set(current_depth + 1));
    true
}

fn leave_runtime() -> i32 {
    let runtime = ACTIVE_RUNTIME.with(Cell::get);
    let depth = ACTIVE_DEPTH.with(Cell::get);
    if runtime.is_null() || depth == 0 {
        return GC_INVALID_ARGUMENT;
    }
    let Ok(baseline_depth) = usize::try_from(depth) else {
        return GC_INVALID_ARGUMENT;
    };
    let baseline = ACTIVE_ROOT_BASELINES.with(|baselines| {
        let baselines = baselines.borrow();
        (baselines.len() == baseline_depth)
            .then_some(baselines.last().copied())
            .flatten()
    });
    let Some(baseline) = baseline else {
        return GC_INVALID_ARGUMENT;
    };
    // SAFETY: ACTIVE_RUNTIME is installed only from a successful activation
    // owned by this thread. Each layer must restore both root chains exactly
    // to the top/depth pair captured on entry; outer roots remain valid across
    // a nested runtime callback.
    if !unsafe { baseline.matches(runtime) } {
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
    ACTIVE_ROOT_BASELINES.with(|baselines| {
        let popped = baselines.borrow_mut().pop();
        debug_assert!(popped.is_some());
    });
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

fn is_aligned_for<T>(pointer: *const T) -> bool {
    !pointer.is_null() && (pointer as usize).is_multiple_of(align_of::<T>())
}

struct ValidatedRootShape {
    slot_count: usize,
    bitmap_words: usize,
}

#[allow(clippy::too_many_arguments)]
unsafe fn validate_root_shape(
    abi_version: u32,
    expected_abi_version: u32,
    flags: u32,
    slot_count: u64,
    state_count: u64,
    live_bitmap_words: u64,
    live_bitmaps: *const u64,
    validate_every_state: bool,
    invalid_status: i32,
) -> Result<ValidatedRootShape, i32> {
    if abi_version != expected_abi_version {
        return Err(GC_ABI_MISMATCH);
    }
    if flags != 0 || slot_count == 0 || state_count == 0 {
        return Err(invalid_status);
    }
    if slot_count > GC_MAX_ROOT_SLOTS || state_count > GC_MAX_ROOT_STATES {
        return Err(GC_RESOURCE_LIMIT);
    }
    let expected_bitmap_words = slot_count.div_ceil(64);
    if live_bitmap_words != expected_bitmap_words {
        return Err(invalid_status);
    }
    let Some(total_words) = state_count.checked_mul(live_bitmap_words) else {
        return Err(GC_RESOURCE_LIMIT);
    };
    if total_words > GC_MAX_ROOT_BITMAP_WORDS {
        return Err(GC_RESOURCE_LIMIT);
    }
    if !is_aligned_for(live_bitmaps) {
        return Err(invalid_status);
    }
    let (Ok(slot_count), Ok(state_count), Ok(bitmap_words), Ok(total_words)) = (
        usize::try_from(slot_count),
        usize::try_from(state_count),
        usize::try_from(live_bitmap_words),
        usize::try_from(total_words),
    ) else {
        return Err(GC_RESOURCE_LIMIT);
    };
    if total_words > isize::MAX as usize / size_of::<u64>() {
        return Err(GC_RESOURCE_LIMIT);
    }
    let tail_bits = slot_count % 64;
    if validate_every_state && tail_bits != 0 {
        let allowed = (1_u64 << tail_bits) - 1;
        for state in 0..state_count {
            let index = state * bitmap_words + bitmap_words - 1;
            // SAFETY: the descriptor contract provides the bounded immutable
            // state_count * live_bitmap_words table checked above.
            if unsafe { *live_bitmaps.add(index) } & !allowed != 0 {
                return Err(invalid_status);
            }
        }
    }
    Ok(ValidatedRootShape {
        slot_count,
        bitmap_words,
    })
}

unsafe fn validate_root_descriptor(
    descriptor: *const LoomGcRootDescriptor,
    validate_every_state: bool,
) -> Result<ValidatedRootShape, i32> {
    if !is_aligned_for(descriptor) {
        return Err(GC_INVALID_ARGUMENT);
    }
    // SAFETY: the ABI requires a readable descriptor at this aligned pointer.
    let descriptor = unsafe { &*descriptor };
    unsafe {
        validate_root_shape(
            descriptor.abi_version,
            SHADOW_STACK_ABI_VERSION,
            descriptor.flags,
            descriptor.slot_count,
            descriptor.state_count,
            descriptor.live_bitmap_words,
            descriptor.live_bitmaps,
            validate_every_state,
            GC_INVALID_ARGUMENT,
        )
    }
}

unsafe fn validate_typed_root_descriptor(
    descriptor: *const LoomGcTypedRootDescriptor,
    validate_every_state: bool,
) -> Result<ValidatedRootShape, i32> {
    if !is_aligned_for(descriptor) {
        return Err(GC_INVALID_ARGUMENT);
    }
    // SAFETY: the ABI requires a readable descriptor at this aligned pointer.
    let descriptor = unsafe { &*descriptor };
    unsafe {
        validate_root_shape(
            descriptor.abi_version,
            TYPED_SHADOW_STACK_ABI_VERSION,
            descriptor.flags,
            descriptor.slot_count,
            descriptor.state_count,
            descriptor.live_bitmap_words,
            descriptor.live_bitmaps,
            validate_every_state,
            GC_DESCRIPTOR_INVALID,
        )
    }
}

unsafe fn validate_root_frame(frame: *const LoomGcRootFrame, linked: bool) -> i32 {
    if !is_aligned_for(frame) {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: the ABI requires a readable frame at this aligned pointer.
    let frame = unsafe { &*frame };
    if frame.abi_version != SHADOW_STACK_ABI_VERSION {
        return GC_ABI_MISMATCH;
    }
    let expected_flags = if linked { GC_ROOT_FRAME_LINKED } else { 0 };
    if frame.flags != expected_flags || (!linked && !frame.previous.is_null()) {
        return GC_INVALID_ARGUMENT;
    }
    let shape = match unsafe { validate_root_descriptor(frame.descriptor, !linked) } {
        Ok(shape) => shape,
        Err(status) => return status,
    };
    // SAFETY: descriptor validation established this immutable descriptor.
    let descriptor = unsafe { &*frame.descriptor };
    if frame.state >= descriptor.state_count || !is_aligned_for(frame.slots) {
        return GC_INVALID_ARGUMENT;
    }
    let Ok(state) = usize::try_from(frame.state) else {
        return GC_INVALID_ARGUMENT;
    };
    let bitmap_row = state * shape.bitmap_words;
    let tail_bits = shape.slot_count % 64;
    if tail_bits != 0 {
        let allowed = (1_u64 << tail_bits) - 1;
        let tail = unsafe {
            *descriptor
                .live_bitmaps
                .add(bitmap_row + shape.bitmap_words - 1)
        };
        if tail & !allowed != 0 {
            return GC_INVALID_ARGUMENT;
        }
    }
    if !linked {
        for index in 0..shape.slot_count {
            // The slot-pointer array is immutable while the frame is linked,
            // so checking every entry once keeps safepoint polling O(depth).
            let slot = unsafe { *frame.slots.add(index) };
            if !is_aligned_for(slot.cast::<ValueSlot>()) {
                return GC_INVALID_ARGUMENT;
            }
        }
    }
    GC_OK
}

unsafe fn validate_typed_root_frame(frame: *const LoomGcTypedRootFrame, linked: bool) -> i32 {
    if !is_aligned_for(frame) {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: the ABI requires a readable frame at this aligned pointer.
    let frame = unsafe { &*frame };
    if frame.abi_version != TYPED_SHADOW_STACK_ABI_VERSION {
        return GC_ABI_MISMATCH;
    }
    let expected_flags = if linked { GC_ROOT_FRAME_LINKED } else { 0 };
    if frame.flags != expected_flags || (!linked && !frame.previous.is_null()) {
        return GC_DESCRIPTOR_INVALID;
    }
    let shape = match unsafe { validate_typed_root_descriptor(frame.descriptor, !linked) } {
        Ok(shape) => shape,
        Err(status) => return status,
    };
    // SAFETY: descriptor validation established this immutable descriptor.
    let descriptor = unsafe { &*frame.descriptor };
    if frame.state >= descriptor.state_count || !is_aligned_for(frame.slots) {
        return GC_DESCRIPTOR_INVALID;
    }
    let Ok(state) = usize::try_from(frame.state) else {
        return GC_DESCRIPTOR_INVALID;
    };
    let bitmap_row = state * shape.bitmap_words;
    let tail_bits = shape.slot_count % 64;
    if tail_bits != 0 {
        let allowed = (1_u64 << tail_bits) - 1;
        let tail = unsafe {
            *descriptor
                .live_bitmaps
                .add(bitmap_row + shape.bitmap_words - 1)
        };
        if tail & !allowed != 0 {
            return GC_DESCRIPTOR_INVALID;
        }
    }
    if !linked {
        for index in 0..shape.slot_count {
            // Typed entries point to writable pointer-sized cells. Validate
            // each cell once while the array is still unlinked and immutable.
            let slot = unsafe { *frame.slots.add(index) };
            if !is_aligned_for(slot.cast::<*mut c_void>()) {
                return GC_DESCRIPTOR_INVALID;
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
    if depth > GC_MAX_ROOT_DEPTH {
        return GC_RESOURCE_LIMIT;
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

type TypedRootVisitor = unsafe extern "C" fn(*mut *mut c_void, *mut c_void);

unsafe fn visit_typed_roots(
    mut frame: *mut LoomGcTypedRootFrame,
    depth: u64,
    visitor: Option<TypedRootVisitor>,
    context: *mut c_void,
) -> i32 {
    if frame.is_null() != (depth == 0) {
        return GC_FRAME_ORDER;
    }
    if depth > GC_MAX_ROOT_DEPTH {
        return GC_RESOURCE_LIMIT;
    }
    let Ok(depth) = usize::try_from(depth) else {
        return GC_FRAME_ORDER;
    };
    for _ in 0..depth {
        if frame.is_null() {
            return GC_FRAME_ORDER;
        }
        let status = unsafe { validate_typed_root_frame(frame, true) };
        if status != GC_OK {
            return status;
        }
        // SAFETY: validation established a live descriptor, bitmap row and
        // slot-pointer array for this compiler-created typed frame.
        let frame_ref = unsafe { &*frame };
        let descriptor = unsafe { &*frame_ref.descriptor };
        let slot_count = usize::try_from(descriptor.slot_count).unwrap_or_else(|_| unreachable!());
        let bitmap_words =
            usize::try_from(descriptor.live_bitmap_words).unwrap_or_else(|_| unreachable!());
        let state = usize::try_from(frame_ref.state).unwrap_or_else(|_| unreachable!());
        let bitmap_row = state * bitmap_words;
        if let Some(visitor) = visitor {
            for index in 0..slot_count {
                let word = unsafe { *descriptor.live_bitmaps.add(bitmap_row + index / 64) };
                if word & (1_u64 << (index % 64)) != 0 {
                    let root = unsafe { *frame_ref.slots.add(index) };
                    unsafe { visitor(root.cast(), context) };
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
        return GC_RESOURCE_LIMIT;
    };
    if depth > GC_MAX_ROOT_DEPTH {
        return GC_RESOURCE_LIMIT;
    }
    unsafe {
        (*frame).previous = runtime.sync_root_top;
        (*frame).flags = GC_ROOT_FRAME_LINKED;
    }
    runtime.sync_root_top = frame;
    runtime.sync_root_depth = depth;
    GC_OK
}

/// Links one compiler-described typed frame into the active Runtime's
/// independent direct-pointer shadow stack. Push cannot collect or allocate.
#[unsafe(export_name = "loom_gc_typed_root_push_v1")]
pub unsafe extern "C" fn typed_root_push_v1(frame: *mut LoomGcTypedRootFrame) -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    let status = unsafe { validate_typed_root_frame(frame, false) };
    if status != GC_OK {
        return status;
    }
    // SAFETY: ACTIVE_RUNTIME gives exclusive generated-code access to the
    // independent typed root chain. The frame remains live until pop.
    let runtime = unsafe { &mut *runtime };
    let Some(depth) = runtime.typed_root_depth.checked_add(1) else {
        return GC_RESOURCE_LIMIT;
    };
    if depth > GC_MAX_ROOT_DEPTH {
        return GC_RESOURCE_LIMIT;
    }
    unsafe {
        (*frame).previous = runtime.typed_root_top;
        (*frame).flags = GC_ROOT_FRAME_LINKED;
    }
    runtime.typed_root_top = frame;
    runtime.typed_root_depth = depth;
    GC_OK
}

/// Pops exactly the most recently linked typed direct-pointer root frame.
#[unsafe(export_name = "loom_gc_typed_root_pop_v1")]
pub unsafe extern "C" fn typed_root_pop_v1(frame: *mut LoomGcTypedRootFrame) -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() || frame.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: ACTIVE_RUNTIME serializes typed root-chain mutation.
    let runtime = unsafe { &mut *runtime };
    if runtime.typed_root_top != frame || runtime.typed_root_depth == 0 {
        return GC_FRAME_ORDER;
    }
    let status = unsafe { validate_typed_root_frame(frame, true) };
    if status != GC_OK {
        return status;
    }
    // SAFETY: top identity and linked-frame validation established ownership
    // of these runtime-maintained fields.
    runtime.typed_root_top = unsafe { (*frame).previous };
    runtime.typed_root_depth -= 1;
    unsafe {
        (*frame).previous = ptr::null_mut();
        (*frame).flags = 0;
    }
    if runtime.typed_root_top.is_null() != (runtime.typed_root_depth == 0) {
        std::process::abort();
    }
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

/// Short-lived precise handle scope for Rust runtime helpers.
///
/// Compiler frames point at LLVM allocas. Runtime helpers instead need a
/// dynamically-sized set of address-stable slots for partially-built values
/// and shallow copies of inputs. Every mutable field which the collector can
/// rewrite is behind `UnsafeCell`; helper code accesses slots only by value so
/// no Rust reference survives a managed allocation boundary.
pub(crate) struct RuntimeRootScope {
    roots: Box<[UnsafeCell<ValueSlot>]>,
    _slots: Box<[*mut c_void]>,
    _live_bitmaps: Box<[u64]>,
    _descriptor: Box<LoomGcRootDescriptor>,
    frame: Box<UnsafeCell<LoomGcRootFrame>>,
    linked: bool,
}

impl RuntimeRootScope {
    pub(crate) fn with_count(count: usize) -> Result<Self, i32> {
        Self::from_values(vec![ValueSlot::default(); count])
    }

    pub(crate) fn from_values(values: Vec<ValueSlot>) -> Result<Self, i32> {
        if values.is_empty() || active_runtime_pointer().is_null() {
            return Err(GC_INVALID_ARGUMENT);
        }
        let roots = values
            .into_iter()
            .map(UnsafeCell::new)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let slots = roots
            .iter()
            .map(|root| root.get().cast::<c_void>())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let bitmap_words = roots.len().div_ceil(64);
        let mut live_bitmaps = vec![u64::MAX; bitmap_words].into_boxed_slice();
        let tail = roots.len() % 64;
        if tail != 0 {
            live_bitmaps[bitmap_words - 1] = (1_u64 << tail) - 1;
        }
        let descriptor = Box::new(LoomGcRootDescriptor {
            abi_version: SHADOW_STACK_ABI_VERSION,
            flags: 0,
            slot_count: roots.len() as u64,
            state_count: 1,
            live_bitmap_words: bitmap_words as u64,
            live_bitmaps: live_bitmaps.as_ptr(),
        });
        let frame = Box::new(UnsafeCell::new(LoomGcRootFrame {
            abi_version: SHADOW_STACK_ABI_VERSION,
            flags: 0,
            state: 0,
            descriptor: &raw const *descriptor,
            slots: slots.as_ptr(),
            previous: ptr::null_mut(),
        }));
        let mut scope = Self {
            roots,
            _slots: slots,
            _live_bitmaps: live_bitmaps,
            _descriptor: descriptor,
            frame,
            linked: false,
        };
        let status = unsafe { root_push_v1(scope.frame.get()) };
        if status != GC_OK {
            return Err(status);
        }
        scope.linked = true;
        Ok(scope)
    }

    pub(crate) fn len(&self) -> usize {
        self.roots.len()
    }

    pub(crate) fn read(&self, index: usize) -> ValueSlot {
        assert!(index < self.roots.len(), "runtime root index is in bounds");
        // SAFETY: roots live for the linked scope and ValueSlot is Copy. The
        // active runtime is single-threaded, so collection cannot interleave
        // with this individual load.
        unsafe { self.roots[index].get().read() }
    }

    pub(crate) fn write(&self, index: usize, value: ValueSlot) {
        assert!(index < self.roots.len(), "runtime root index is in bounds");
        // SAFETY: see read. UnsafeCell explicitly permits collector rewrites
        // through the same stable storage between helper operations.
        unsafe { self.roots[index].get().write(value) };
    }

    pub(crate) fn pointer(&self, index: usize) -> *mut ValueSlot {
        assert!(index < self.roots.len(), "runtime root index is in bounds");
        self.roots[index].get()
    }
}

impl Drop for RuntimeRootScope {
    fn drop(&mut self) {
        if self.linked {
            let status = unsafe { root_pop_v1(self.frame.get()) };
            if status != GC_OK {
                std::process::abort();
            }
            self.linked = false;
        }
    }
}

fn aggregate_count_word(tag: u64) -> Option<usize> {
    match tag {
        VALUE_TAG_RECORD | VALUE_TAG_CONSTRAINT_ERROR | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
            Some(VALUE_WORD_AUX)
        }
        VALUE_TAG_ENUM => Some(VALUE_WORD_SCALAR),
        _ => None,
    }
}

/// Incremental aggregate constructor which keeps its partial chain reachable
/// and reloadable through a `RuntimeRootScope`.
pub(crate) struct NodeStream<'scope> {
    roots: &'scope RuntimeRootScope,
    aggregate: usize,
}

impl<'scope> NodeStream<'scope> {
    pub(crate) fn new(roots: &'scope RuntimeRootScope, aggregate: usize, value: ValueSlot) -> Self {
        roots.write(aggregate, value);
        Self { roots, aggregate }
    }

    pub(crate) fn prepend(&self, value: usize) -> i32 {
        if value >= self.roots.len() {
            return GC_INVALID_ARGUMENT;
        }
        let node = allocate_value_node().cast::<ValueNode>();
        if node.is_null() {
            return GC_INVALID_ARGUMENT;
        }
        // Allocation may have relocated every prior object. Reload both the
        // child and partial aggregate only after it returns.
        let child = self.roots.read(value);
        let mut aggregate = self.roots.read(self.aggregate);
        let Some(count_word) = aggregate_count_word(aggregate.words[VALUE_WORD_TAG]) else {
            return GC_INVALID_ARGUMENT;
        };
        let count = aggregate.words[count_word];
        unsafe {
            (*node).value = child;
            (*node).next = aggregate.words[VALUE_WORD_DATA] as *mut ValueNode;
        }
        aggregate.words[VALUE_WORD_DATA] = node as u64;
        aggregate.words[count_word] = count
            .checked_add(1)
            .unwrap_or_else(|| std::process::abort());
        self.roots.write(self.aggregate, aggregate);
        GC_OK
    }
}

fn managed_allocation_slowpath(runtime: *mut LoomRuntime, incoming: usize) -> i32 {
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    // Do not borrow the heap across collection: an attached executor routes
    // back to the same Runtime while tracing Task roots.
    let should_collect = unsafe {
        (*runtime).heap.allocation_charge.saturating_add(incoming)
            >= (*runtime).heap.next_gc_threshold
    };
    #[cfg(test)]
    let should_collect =
        should_collect || unsafe { (*runtime).heap.collect_before_every_allocation };
    if !should_collect {
        return GC_OK;
    }
    // All runtime and compiler callers must publish stable roots before an
    // allocator call. `force` is required because the projected charge, not
    // only the current charge, selected this boundary.
    unsafe { collect_active_runtime(runtime, true) }
}

struct ValidatedObjectShape {
    layout: Layout,
    pointer_offsets: Box<[usize]>,
}

unsafe fn validate_object_descriptor(
    descriptor: *const LoomGcObjectDescriptor,
    allocation_size: u64,
) -> Result<ValidatedObjectShape, i32> {
    if !is_aligned_for(descriptor) {
        return Err(GC_INVALID_ARGUMENT);
    }
    // SAFETY: the ABI requires a readable descriptor at this aligned pointer.
    let descriptor = unsafe { &*descriptor };
    if descriptor.abi_version != TYPED_GC_ABI_VERSION {
        return Err(GC_ABI_MISMATCH);
    }
    if descriptor.flags != 0
        || descriptor.fixed_size == 0
        || descriptor.fixed_size > allocation_size
        || descriptor.object_align == 0
        || !descriptor.object_align.is_power_of_two()
    {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    if allocation_size > GC_MAX_OBJECT_BYTES
        || descriptor.object_align > GC_MAX_OBJECT_ALIGNMENT
        || descriptor.pointer_count > GC_MAX_OBJECT_POINTERS
    {
        return Err(GC_RESOURCE_LIMIT);
    }
    if (descriptor.pointer_count == 0) != descriptor.pointer_offsets.is_null() {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    if descriptor.pointer_count != 0
        && (!is_aligned_for(descriptor.pointer_offsets)
            || descriptor.object_align < align_of::<*mut c_void>() as u64)
    {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    let (Ok(allocation_size), Ok(object_align), Ok(pointer_count), Ok(fixed_size)) = (
        usize::try_from(allocation_size),
        usize::try_from(descriptor.object_align),
        usize::try_from(descriptor.pointer_count),
        usize::try_from(descriptor.fixed_size),
    ) else {
        return Err(GC_RESOURCE_LIMIT);
    };
    let Ok(layout) = Layout::from_size_align(allocation_size, object_align) else {
        return Err(GC_DESCRIPTOR_INVALID);
    };
    let mut pointer_offsets = Vec::with_capacity(pointer_count);
    let mut previous = None;
    for index in 0..pointer_count {
        // SAFETY: the descriptor contract supplies the bounded immutable
        // pointer-offset table checked above.
        let raw_offset = unsafe { *descriptor.pointer_offsets.add(index) };
        let Ok(offset) = usize::try_from(raw_offset) else {
            return Err(GC_DESCRIPTOR_INVALID);
        };
        let Some(end) = offset.checked_add(size_of::<*mut c_void>()) else {
            return Err(GC_DESCRIPTOR_INVALID);
        };
        if !offset.is_multiple_of(align_of::<*mut c_void>())
            || end > fixed_size
            || previous.is_some_and(|previous| offset <= previous)
        {
            return Err(GC_DESCRIPTOR_INVALID);
        }
        pointer_offsets.push(offset);
        previous = Some(offset);
    }
    Ok(ValidatedObjectShape {
        layout,
        pointer_offsets: pointer_offsets.into_boxed_slice(),
    })
}

/// Allocates one zero-initialized precisely described typed object.
///
/// The runtime copies all validated pointer offsets before any collection and
/// never retains either caller metadata pointer. `allocation_size` may exceed
/// the fixed pointer-bearing prefix only for pointer-free trailing bytes. The
/// caller-owned output cell must have a stable non-heap address for this whole
/// call because descriptor validation can be followed by a collection.
pub(crate) unsafe fn allocate_typed_object(
    descriptor: *const LoomGcObjectDescriptor,
    allocation_size: u64,
    output: *mut *mut c_void,
) -> i32 {
    if !is_aligned_for(output) {
        return GC_INVALID_ARGUMENT;
    }
    // Publish null before descriptor validation or a collection boundary so a
    // live output root can never expose partially initialized object bytes.
    unsafe { output.write(ptr::null_mut()) };
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    let shape = match unsafe { validate_object_descriptor(descriptor, allocation_size) } {
        Ok(shape) => shape,
        Err(status) => return status,
    };
    let status = managed_allocation_slowpath(runtime, shape.layout.size());
    if status != GC_OK {
        return status;
    }
    let allocation = TypedAllocation::new(shape.layout, shape.pointer_offsets);
    let pointer = allocation.pointer();
    // SAFETY: ACTIVE_RUNTIME serializes heap access. Collection completed
    // before the heap borrow and the fresh zeroed allocation is not published
    // until its validated metadata is owned by the heap.
    unsafe {
        (*runtime).heap.typed_objects.push(allocation);
        (*runtime).heap.allocation_charge = (*runtime)
            .heap
            .allocation_charge
            .saturating_add(shape.layout.size());
        output.write(pointer);
    }
    GC_OK
}

#[unsafe(export_name = "loom_gc_typed_alloc_v1")]
pub unsafe extern "C" fn typed_alloc_v1(
    descriptor: *const LoomGcObjectDescriptor,
    allocation_size: u64,
    output: *mut *mut c_void,
) -> i32 {
    unsafe { allocate_typed_object(descriptor, allocation_size, output) }
}

#[unsafe(export_name = "loom_gc_alloc_value")]
pub extern "C" fn allocate_value() -> *mut c_void {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return ptr::null_mut();
    }
    if managed_allocation_slowpath(runtime, size_of::<ValueSlot>()) != GC_OK {
        return ptr::null_mut();
    }
    let mut allocation = Box::new(ValueSlot::default());
    let pointer = (&raw mut *allocation).cast::<c_void>();
    // SAFETY: ACTIVE_RUNTIME is set only around a single-threaded generated
    // interval. Collection completed before the heap borrow and fresh Box.
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
    if managed_allocation_slowpath(runtime, size_of::<ValueNode>()) != GC_OK {
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
    let charge = allocation.len().saturating_mul(size_of::<u64>());
    // The complete sequence is staged in this Rust-owned Box before
    // collection. Thus a source &[u8] borrowed from the moving heap has
    // already been consumed and `object` is not yet part of the managed heap.
    if managed_allocation_slowpath(runtime, charge) != GC_OK {
        return None;
    }
    // SAFETY: see allocate_value. The staged object is immutable after adopt.
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
    // Copy both inputs into runtime-owned stable roots before the allocator can
    // move anything. The mutable destination itself is required by the ABI to
    // be an address-stable compiler or Task slot.
    let initial = unsafe { *list };
    if initial.words[VALUE_WORD_TAG] != VALUE_TAG_LIST {
        return 1;
    }
    let Ok(roots) = RuntimeRootScope::from_values(vec![initial, unsafe { *value }]) else {
        return 1;
    };
    let node = allocate_value_node().cast::<ValueNode>();
    if node.is_null() {
        return 1;
    }

    // The allocation may have collected, invalidating every pre-call chain
    // pointer and clearing the derived tail cache. Reload before validation.
    let mut updated = roots.read(0);
    let head = updated.words[VALUE_WORD_DATA] as *mut ValueNode;
    let count = updated.words[VALUE_WORD_AUX];
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
    // No collection can occur between initializing this fresh node and
    // publishing the matching List head/count.
    unsafe {
        (*node).value = roots.read(1);
        (*node).next = ptr::null_mut();
    }
    if head.is_null() {
        updated.words[VALUE_WORD_DATA] = node as u64;
    } else {
        let Some(tail) = tail else {
            return 1;
        };
        // SAFETY: the checked chain walk or runtime-local index selected the
        // final node in this List's runtime-owned chain.
        unsafe {
            (*tail).next = node;
        }
    }
    updated.words[VALUE_WORD_AUX] = new_count;
    // Publish the matching head/count immediately after linking. Cache
    // maintenance below is Rust-only and cannot safepoint, but keeping this
    // invariant tight makes future changes fail safe.
    roots.write(0, updated);
    if head.is_null() {
        cache_list_tail(node, new_count, node);
    } else if !append_cached_list_node(head, count, node) {
        cache_list_tail(head, new_count, node);
    }
    unsafe { list.write(roots.read(0)) };
    0
}

/// Returns `1` and copies a List element into stable caller storage, `0` when
/// out of range, or `-1` for invalid ABI input.
#[unsafe(export_name = "loom_runtime_list_get")]
pub unsafe extern "C" fn list_get(
    list: *const ValueSlot,
    index: i64,
    output: *mut ValueSlot,
) -> i32 {
    if list.is_null() || output.is_null() {
        return -1;
    }
    if index < 0 {
        return 0;
    }
    // SAFETY: generated checked MIR provides a live, aligned ValueSlot pointer.
    let list = unsafe { &*list };
    if list.words[VALUE_WORD_TAG] != VALUE_TAG_LIST {
        return -1;
    }
    if index.cast_unsigned() >= list.words[VALUE_WORD_AUX] {
        return 0;
    }
    let head = list.words[VALUE_WORD_DATA] as *mut ValueNode;
    let index = usize::try_from(index).unwrap_or_else(|_| unreachable!());
    let node = if !has_active_runtime() {
        let Some(node) = (unsafe { list_node_at(head, index) }) else {
            return -1;
        };
        node
    } else if let Some(node) = cached_list_node(head, list.words[VALUE_WORD_AUX], index) {
        node
    } else {
        let Some(nodes) = (unsafe { list_chain(head, list.words[VALUE_WORD_AUX]) }) else {
            return -1;
        };
        let node = nodes[index];
        cache_list_chain(head, list.words[VALUE_WORD_AUX], nodes);
        node
    };
    if node.is_null() {
        -1
    } else {
        // SAFETY: node is non-null and belongs to the live List chain. The
        // stable caller slot closes the old borrowed-interior-pointer window.
        unsafe { output.write((*node).value) };
        1
    }
}

const CLONE_ROOT_COUNT: usize = 6;
const BUILD_ROOT_COUNT: usize = 3;
const CLONE_ROOT_SOURCE: usize = 0;
const CLONE_ROOT_OUTPUT: usize = 1;
const CLONE_ROOT_CURSOR: usize = 2;
const CLONE_ROOT_TAIL: usize = 3;
const CLONE_ROOT_CHILD_SOURCE: usize = 4;
const CLONE_ROOT_CHILD_RESULT: usize = 5;

static CLONE_ROOT_BITMAP: [u64; 1] = [(1_u64 << CLONE_ROOT_COUNT) - 1];
static BUILD_ROOT_BITMAP: [u64; 1] = [(1_u64 << BUILD_ROOT_COUNT) - 1];

/// A root descriptor contains a pointer, so Rust does not infer `Sync`. These
/// two instances point only at immutable process-lifetime bitmap arrays.
struct SharedRootDescriptor(LoomGcRootDescriptor);

// SAFETY: the wrapped descriptors and the bitmap storage they reference are
// immutable process-lifetime statics.
unsafe impl Sync for SharedRootDescriptor {}

static CLONE_ROOT_DESCRIPTOR: SharedRootDescriptor = SharedRootDescriptor(LoomGcRootDescriptor {
    abi_version: SHADOW_STACK_ABI_VERSION,
    flags: 0,
    slot_count: CLONE_ROOT_COUNT as u64,
    state_count: 1,
    live_bitmap_words: 1,
    live_bitmaps: CLONE_ROOT_BITMAP.as_ptr(),
});

static BUILD_ROOT_DESCRIPTOR: SharedRootDescriptor = SharedRootDescriptor(LoomGcRootDescriptor {
    abi_version: SHADOW_STACK_ABI_VERSION,
    flags: 0,
    slot_count: BUILD_ROOT_COUNT as u64,
    state_count: 1,
    live_bitmap_words: 1,
    live_bitmaps: BUILD_ROOT_BITMAP.as_ptr(),
});

#[derive(Clone, Copy)]
enum BoxCloneKind {
    Refined,
    Dynamic,
    CompletedOutcome,
}

#[derive(Clone, Copy)]
enum ClonePhase {
    Dispatch,
    Aggregate,
    AwaitAggregateChild,
    AwaitBoxChild(BoxCloneKind),
    Done,
}

/// One non-moving explicit clone continuation. Each work item owns a precise
/// shadow-stack frame, so arbitrary Loom nesting consumes heap work storage
/// rather than the native call stack. `Box<CloneWork>` keeps every slot and
/// the intrusive frame header address-stable while linked.
struct CloneWork {
    roots: [ValueSlot; CLONE_ROOT_COUNT],
    slots: [*mut c_void; CLONE_ROOT_COUNT],
    frame: LoomGcRootFrame,
    phase: ClonePhase,
}

impl CloneWork {
    unsafe fn new(source: *const ValueSlot) -> Option<Box<Self>> {
        if source.is_null() {
            return None;
        }
        // No Loom safepoint is permitted before this shallow copy and the
        // resulting local root frame is linked.
        let source = unsafe { *source };
        let mut work = Box::new(Self {
            roots: [ValueSlot::default(); CLONE_ROOT_COUNT],
            slots: [ptr::null_mut(); CLONE_ROOT_COUNT],
            frame: LoomGcRootFrame {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 0,
                descriptor: &raw const CLONE_ROOT_DESCRIPTOR.0,
                slots: ptr::null(),
                previous: ptr::null_mut(),
            },
            phase: ClonePhase::Dispatch,
        });
        work.roots[CLONE_ROOT_SOURCE] = source;
        for index in 0..CLONE_ROOT_COUNT {
            work.slots[index] = (&raw mut work.roots[index]).cast();
        }
        work.frame.slots = work.slots.as_ptr();
        Some(work)
    }

    fn frame_pointer(&mut self) -> *mut LoomGcRootFrame {
        &raw mut self.frame
    }
}

fn synthetic_list(head: *mut ValueNode, count: u64) -> ValueSlot {
    let mut value = ValueSlot::default();
    value.words[VALUE_WORD_TAG] = VALUE_TAG_LIST;
    value.words[VALUE_WORD_AUX] = count;
    value.words[VALUE_WORD_DATA] = head as u64;
    value
}

unsafe fn pop_clone_work(work: &mut CloneWork) -> i32 {
    unsafe { root_pop_v1(work.frame_pointer()) }
}

unsafe fn unwind_clone_work(stack: &mut Vec<Box<CloneWork>>, output: *mut ValueSlot) -> i32 {
    if !output.is_null() {
        // No collection occurs while the linked frames are unwound.
        unsafe { output.write(ValueSlot::default()) };
    }
    while let Some(work) = stack.last_mut() {
        let status = unsafe { pop_clone_work(work) };
        if status != GC_OK {
            return status;
        }
        stack.pop();
    }
    GC_OK
}

unsafe fn start_clone_child(stack: &mut Vec<Box<CloneWork>>, source: *const ValueSlot) -> i32 {
    let Some(mut child) = (unsafe { CloneWork::new(source) }) else {
        return GC_INVALID_ARGUMENT;
    };
    let status = unsafe { root_push_v1(child.frame_pointer()) };
    if status != GC_OK {
        return status;
    }
    stack.push(child);
    GC_OK
}

unsafe fn finish_clone_child(stack: &mut Vec<Box<CloneWork>>) -> i32 {
    if stack.len() < 2 {
        return GC_FRAME_ORDER;
    }
    let child_result = {
        let child = stack.last().unwrap_or_else(|| unreachable!());
        child.roots[CLONE_ROOT_OUTPUT]
    };
    let parent_index = stack.len() - 2;
    stack[parent_index].roots[CLONE_ROOT_CHILD_RESULT] = child_result;
    let status = {
        let child = stack.last_mut().unwrap_or_else(|| unreachable!());
        unsafe { pop_clone_work(child) }
    };
    if status == GC_OK {
        stack.pop();
    }
    status
}

fn aggregate_count(value: &ValueSlot) -> Option<u64> {
    match value.words[VALUE_WORD_TAG] {
        VALUE_TAG_RECORD | VALUE_TAG_CONSTRAINT_ERROR | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
            Some(value.words[VALUE_WORD_AUX])
        }
        VALUE_TAG_ENUM => Some(value.words[VALUE_WORD_SCALAR]),
        _ => None,
    }
}

/// Deep-clones one universal Value into caller-owned stable storage.
///
/// `source` may be an interior pointer into the moving heap because it is read
/// exactly once before any safepoint. `output` must be an address-stable Value
/// slot which the caller keeps live across this call. The implementation uses
/// an explicit non-moving work stack and never keeps a heap-derived pointer
/// across a helper allocation poll.
#[unsafe(export_name = "loom_gc_clone_value_v1")]
#[allow(clippy::too_many_lines)]
pub unsafe extern "C" fn clone_value_v1(output: *mut c_void, source: *const c_void) -> i32 {
    let output = output.cast::<ValueSlot>();
    let source = source.cast::<ValueSlot>();
    if output.is_null() || source.is_null() || active_runtime_pointer().is_null() {
        return GC_INVALID_ARGUMENT;
    }
    // Preserve aliasing input before initializing the stable result slot.
    let Some(mut top) = (unsafe { CloneWork::new(source) }) else {
        return GC_INVALID_ARGUMENT;
    };
    unsafe { output.write(ValueSlot::default()) };
    let status = unsafe { root_push_v1(top.frame_pointer()) };
    if status != GC_OK {
        return status;
    }
    let mut stack = vec![top];

    let status = loop {
        let phase = stack.last().map_or(ClonePhase::Done, |work| work.phase);
        match phase {
            ClonePhase::Dispatch => {
                let work = stack.last_mut().unwrap_or_else(|| unreachable!());
                let source = work.roots[CLONE_ROOT_SOURCE];
                work.roots[CLONE_ROOT_OUTPUT] = source;
                if let Some(count) = aggregate_count(&source) {
                    work.roots[CLONE_ROOT_CURSOR] =
                        synthetic_list(source.words[VALUE_WORD_DATA] as *mut ValueNode, count);
                    work.roots[CLONE_ROOT_OUTPUT] = synthetic_list(ptr::null_mut(), 0);
                    work.phase = ClonePhase::Aggregate;
                    continue;
                }
                let kind = match source.words[VALUE_WORD_TAG] {
                    VALUE_TAG_REFINED => Some(BoxCloneKind::Refined),
                    VALUE_TAG_DYN => Some(BoxCloneKind::Dynamic),
                    VALUE_TAG_TASK_OUTCOME
                        if source.words[VALUE_WORD_AUX] == TASK_COMPLETED as u64 =>
                    {
                        Some(BoxCloneKind::CompletedOutcome)
                    }
                    _ => None,
                };
                let Some(kind) = kind else {
                    work.phase = ClonePhase::Done;
                    continue;
                };
                let inner = source.words[VALUE_WORD_DATA] as *const ValueSlot;
                if inner.is_null() {
                    break GC_INVALID_ARGUMENT;
                }
                // The inner pointer is consumed before any safepoint. Its
                // shallow copy becomes a precise parent-frame root.
                work.roots[CLONE_ROOT_CHILD_SOURCE] = unsafe { *inner };
                work.phase = ClonePhase::AwaitBoxChild(kind);
                let child_source = &raw const work.roots[CLONE_ROOT_CHILD_SOURCE];
                let status = unsafe { start_clone_child(&mut stack, child_source) };
                if status != GC_OK {
                    break status;
                }
            }
            ClonePhase::Aggregate => {
                let work = stack.last_mut().unwrap_or_else(|| unreachable!());
                let remaining = work.roots[CLONE_ROOT_CURSOR].words[VALUE_WORD_AUX];
                if remaining == 0 {
                    if work.roots[CLONE_ROOT_CURSOR].words[VALUE_WORD_DATA] != 0 {
                        break GC_INVALID_ARGUMENT;
                    }
                    let head = work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_DATA];
                    let expected = aggregate_count(&work.roots[CLONE_ROOT_SOURCE])
                        .unwrap_or_else(|| unreachable!());
                    if work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_AUX] != expected {
                        break GC_INVALID_ARGUMENT;
                    }
                    work.roots[CLONE_ROOT_OUTPUT] = work.roots[CLONE_ROOT_SOURCE];
                    work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_DATA] = head;
                    work.phase = ClonePhase::Done;
                    continue;
                }
                let node = work.roots[CLONE_ROOT_CURSOR].words[VALUE_WORD_DATA] as *const ValueNode;
                if node.is_null() {
                    break GC_INVALID_ARGUMENT;
                }
                // Consume every derived source address before a child can
                // poll. Advancing the cursor first leaves only precise Value
                // roots live across the nested clone.
                let node_ref = unsafe { &*node };
                work.roots[CLONE_ROOT_CHILD_SOURCE] = node_ref.value;
                work.roots[CLONE_ROOT_CURSOR].words[VALUE_WORD_DATA] = node_ref.next as u64;
                work.roots[CLONE_ROOT_CURSOR].words[VALUE_WORD_AUX] = remaining - 1;
                work.phase = ClonePhase::AwaitAggregateChild;
                let child_source = &raw const work.roots[CLONE_ROOT_CHILD_SOURCE];
                let status = unsafe { start_clone_child(&mut stack, child_source) };
                if status != GC_OK {
                    break status;
                }
            }
            ClonePhase::AwaitAggregateChild => {
                let node = allocate_value_node().cast::<ValueNode>();
                if node.is_null() {
                    break GC_INVALID_ARGUMENT;
                }
                let work = stack.last_mut().unwrap_or_else(|| unreachable!());
                // The poll may have relocated every managed pointer, so only
                // reloaded root values are used below.
                unsafe {
                    (*node).value = work.roots[CLONE_ROOT_CHILD_RESULT];
                    (*node).next = ptr::null_mut();
                }
                let built = work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_AUX];
                if built == 0 {
                    work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_DATA] = node as u64;
                    work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_AUX] = 1;
                    work.roots[CLONE_ROOT_TAIL] = synthetic_list(node, 1);
                } else {
                    let tail = work.roots[CLONE_ROOT_TAIL].words[VALUE_WORD_DATA] as *mut ValueNode;
                    if tail.is_null() {
                        break GC_INVALID_ARGUMENT;
                    }
                    // No safepoint is permitted between linking the initialized
                    // node and publishing the matching count/tail state.
                    unsafe { (*tail).next = node };
                    work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_AUX] = built
                        .checked_add(1)
                        .unwrap_or_else(|| std::process::abort());
                    work.roots[CLONE_ROOT_TAIL].words[VALUE_WORD_DATA] = node as u64;
                }
                work.roots[CLONE_ROOT_CHILD_RESULT] = ValueSlot::default();
                work.phase = ClonePhase::Aggregate;
            }
            ClonePhase::AwaitBoxChild(kind) => {
                let boxed = allocate_value().cast::<ValueSlot>();
                if boxed.is_null() {
                    break GC_INVALID_ARGUMENT;
                }
                let work = stack.last_mut().unwrap_or_else(|| unreachable!());
                unsafe { boxed.write(work.roots[CLONE_ROOT_CHILD_RESULT]) };
                work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_DATA] = boxed as u64;
                if matches!(kind, BoxCloneKind::Dynamic) {
                    work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_SCALAR] = 0;
                    work.roots[CLONE_ROOT_OUTPUT].words[VALUE_WORD_AUX] &= DYN_FLAG_MUTABLE;
                }
                work.roots[CLONE_ROOT_CHILD_RESULT] = ValueSlot::default();
                work.phase = ClonePhase::Done;
            }
            ClonePhase::Done => {
                if stack.len() == 1 {
                    unsafe { output.write(stack[0].roots[CLONE_ROOT_OUTPUT]) };
                    let status = {
                        let top = stack.last_mut().unwrap_or_else(|| unreachable!());
                        unsafe { pop_clone_work(top) }
                    };
                    if status == GC_OK {
                        stack.pop();
                    }
                    break status;
                }
                let status = unsafe { finish_clone_child(&mut stack) };
                if status != GC_OK {
                    break status;
                }
            }
        }
    };

    if status == GC_OK {
        return GC_OK;
    }
    let unwind_status = unsafe { unwind_clone_work(&mut stack, output) };
    if unwind_status == GC_OK {
        status
    } else {
        unwind_status
    }
}

/// Builds one `ValueNode` chain from an immutable array of stable Value-slot
/// pointers. Elements are shallow-moved into fresh nodes in source order.
/// Every source pointer must remain address-stable and GC-updatable for the
/// duration of the call; the output is a synthetic List root containing the
/// finished head and count.
#[unsafe(export_name = "loom_gc_build_value_nodes_v1")]
pub unsafe extern "C" fn build_value_nodes_v1(
    output: *mut c_void,
    sources: *const *const c_void,
    count: u64,
) -> i32 {
    let output = output.cast::<ValueSlot>();
    if output.is_null() || active_runtime_pointer().is_null() {
        return GC_INVALID_ARGUMENT;
    }
    let Ok(count) = usize::try_from(count) else {
        return GC_INVALID_ARGUMENT;
    };
    if count == 0 {
        unsafe { output.write(synthetic_list(ptr::null_mut(), 0)) };
        return GC_OK;
    }
    if sources.is_null() || count > isize::MAX as usize / size_of::<*const c_void>() {
        return GC_INVALID_ARGUMENT;
    }
    for index in 0..count {
        let source = unsafe { *sources.add(index) }.cast::<ValueSlot>();
        if source.is_null() || source == output {
            return GC_INVALID_ARGUMENT;
        }
    }

    unsafe { output.write(synthetic_list(ptr::null_mut(), 0)) };
    let mut tail = synthetic_list(ptr::null_mut(), 0);
    let mut source_scratch = ValueSlot::default();
    let slots = [
        output.cast::<c_void>(),
        (&raw mut tail).cast::<c_void>(),
        (&raw mut source_scratch).cast::<c_void>(),
    ];
    let mut frame = LoomGcRootFrame {
        abi_version: SHADOW_STACK_ABI_VERSION,
        flags: 0,
        state: 0,
        descriptor: &raw const BUILD_ROOT_DESCRIPTOR.0,
        slots: slots.as_ptr(),
        previous: ptr::null_mut(),
    };
    let status = unsafe { root_push_v1(&raw mut frame) };
    if status != GC_OK {
        unsafe { output.write(ValueSlot::default()) };
        return status;
    }

    let mut status = GC_OK;
    for index in 0..count {
        let source = unsafe { *sources.add(index) }.cast::<ValueSlot>();
        // The caller contract makes this address stable across prior polls.
        // Copy it before the next allocation boundary.
        source_scratch = unsafe { *source };
        let node = allocate_value_node().cast::<ValueNode>();
        if node.is_null() {
            status = GC_INVALID_ARGUMENT;
            break;
        }
        unsafe {
            (*node).value = source_scratch;
            (*node).next = ptr::null_mut();
        }
        let built = unsafe { (*output).words[VALUE_WORD_AUX] };
        if built == 0 {
            unsafe {
                (*output).words[VALUE_WORD_DATA] = node as u64;
                (*output).words[VALUE_WORD_AUX] = 1;
            }
            tail = synthetic_list(node, 1);
        } else {
            let tail_pointer = tail.words[VALUE_WORD_DATA] as *mut ValueNode;
            if tail_pointer.is_null() {
                status = GC_INVALID_ARGUMENT;
                break;
            }
            unsafe { (*tail_pointer).next = node };
            unsafe {
                (*output).words[VALUE_WORD_AUX] = built
                    .checked_add(1)
                    .unwrap_or_else(|| std::process::abort());
            }
            tail.words[VALUE_WORD_DATA] = node as u64;
        }
    }
    if status == GC_OK && unsafe { (*output).words[VALUE_WORD_AUX] } != count as u64 {
        status = GC_INVALID_ARGUMENT;
    }
    if status != GC_OK {
        unsafe { output.write(ValueSlot::default()) };
    }
    let pop_status = unsafe { root_pop_v1(&raw mut frame) };
    if status == GC_OK { pop_status } else { status }
}

/// Deep-clones one immutable conformance proof into the active Runtime's
/// non-moving, traced proof arena.
///
/// The clone is not published until its complete prerequisite DAG has been
/// validated and copied into native staging storage. Its allocation slowpath
/// may collect before the staged proof graph is adopted. Generated code must
/// store the returned root in an initialized owned `dyn` value before its next
/// safepoint or managed allocation.
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
    if managed_allocation_slowpath(runtime, charge) != GC_OK {
        return ptr::null();
    }
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
    typed_objects: HashMap<usize, *const TypedAllocation>,
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
            typed_objects: heap
                .typed_objects
                .iter()
                .map(|object| (object.address(), &raw const *object))
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
    typed_objects: HashSet<usize>,
    witnesses: HashSet<usize>,
    invalid_witness: bool,
}

struct TraceContext {
    index: *const HeapIndex,
    marks: *mut Marks,
    work: Vec<TraceItem>,
}

unsafe extern "C" fn trace_slot(slot: *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    let context = unsafe { &mut *context.cast::<TraceContext>() };
    let index = unsafe { &*context.index };
    let marks = unsafe { &mut *context.marks };
    trace_value(slot.cast::<ValueSlot>(), index, marks, &mut context.work);
}

struct TypedTraceContext {
    index: *const HeapIndex,
    marks: *mut Marks,
    work: Vec<usize>,
}

unsafe extern "C" fn trace_typed_slot(slot: *mut *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    let context = unsafe { &mut *context.cast::<TypedTraceContext>() };
    let index = unsafe { &*context.index };
    let marks = unsafe { &mut *context.marks };
    // SAFETY: typed root-frame validation established writable pointer-sized
    // storage for every live slot.
    let pointer = unsafe { slot.read() };
    trace_typed_pointer(pointer, index, marks, &mut context.work);
}

fn trace_typed_pointer(
    pointer: *mut c_void,
    index: &HeapIndex,
    marks: &mut Marks,
    work: &mut Vec<usize>,
) {
    debug_assert!(work.is_empty());
    let address = pointer as usize;
    if pointer.is_null()
        || !index.typed_objects.contains_key(&address)
        || !marks.typed_objects.insert(address)
    {
        return;
    }
    work.push(address);
    while let Some(address) = work.pop() {
        let allocation_pointer = index
            .typed_objects
            .get(&address)
            .copied()
            .unwrap_or_else(|| unreachable!());
        // SAFETY: HeapIndex was built from the immutable typed-object vector,
        // which is not swept or moved until tracing completes.
        let allocation = unsafe { &*allocation_pointer };
        for &offset in &allocation.pointer_offsets {
            // SAFETY: descriptor validation proved every copied offset names
            // an aligned pointer-sized cell inside this allocation.
            let child = unsafe { allocation.pointer_cell(offset).read() };
            let child_address = child as usize;
            // Null and untracked immortal/static pointers are intentionally
            // ignored. Only exact managed allocation bases enter the worklist.
            if !child.is_null()
                && index.typed_objects.contains_key(&child_address)
                && marks.typed_objects.insert(child_address)
            {
                work.push(child_address);
            }
        }
    }
}

enum TraceItem {
    Value(*const ValueSlot),
    Nodes(*const ValueNode, u64),
}

/// Traces one complete value graph without consuming native stack in
/// proportion to Loom nesting depth. A node-chain continuation is scheduled
/// before its child value, keeping the worklist proportional to structural
/// depth rather than the width of a flat aggregate.
fn trace_value(
    value: *const ValueSlot,
    index: &HeapIndex,
    marks: &mut Marks,
    work: &mut Vec<TraceItem>,
) {
    debug_assert!(work.is_empty());
    work.push(TraceItem::Value(value));
    while let Some(item) = work.pop() {
        match item {
            TraceItem::Value(value) => {
                if value.is_null() {
                    continue;
                }
                // SAFETY: roots and managed children are checked compiler or
                // runtime values. Managed pointers are admitted below only
                // after consulting the current heap index.
                let value = unsafe { &*value };
                match value.words[VALUE_WORD_TAG] {
                    VALUE_TAG_TEXT => {
                        let Some(object) = text::object(value) else {
                            continue;
                        };
                        let address = object as usize;
                        if index.sequences.contains(&address) {
                            marks.sequences.insert(address);
                        }
                    }
                    VALUE_TAG_RECORD
                    | VALUE_TAG_CONSTRAINT_ERROR
                    | VALUE_TAG_TUPLE
                    | VALUE_TAG_LIST => work.push(TraceItem::Nodes(
                        value.words[VALUE_WORD_DATA] as *const ValueNode,
                        value.words[VALUE_WORD_AUX],
                    )),
                    VALUE_TAG_ENUM => work.push(TraceItem::Nodes(
                        value.words[VALUE_WORD_DATA] as *const ValueNode,
                        value.words[VALUE_WORD_SCALAR],
                    )),
                    VALUE_TAG_REFINED => schedule_value_pointer(
                        value.words[VALUE_WORD_DATA] as *const ValueSlot,
                        index,
                        marks,
                        work,
                    ),
                    VALUE_TAG_DYN => {
                        schedule_value_pointer(
                            value.words[VALUE_WORD_DATA] as *const ValueSlot,
                            index,
                            marks,
                            work,
                        );
                        trace_witness_pointer(
                            value.words[VALUE_WORD_WITNESS] as *const LoomWitnessInstance,
                            index,
                            marks,
                        );
                    }
                    VALUE_TAG_TASK_OUTCOME
                        if value.words[VALUE_WORD_AUX] == TASK_COMPLETED as u64 =>
                    {
                        schedule_value_pointer(
                            value.words[VALUE_WORD_DATA] as *const ValueSlot,
                            index,
                            marks,
                            work,
                        );
                    }
                    _ => {}
                }
            }
            TraceItem::Nodes(pointer, count) => {
                if pointer.is_null() || count == 0 {
                    continue;
                }
                let address = pointer as usize;
                let newly_marked = !index.nodes.contains(&address) || marks.nodes.insert(address);
                // SAFETY: aggregate counts and chains are compiler/runtime
                // constructed. The same bounded invariant permits reading the
                // child and continuation before either is scheduled.
                let node = unsafe { &*pointer };
                work.push(TraceItem::Nodes(node.next, count - 1));
                // A shared head may first be reached through a shorter view.
                // Continue the chain even when already marked, but avoid
                // tracing its child graph more than once.
                if newly_marked {
                    work.push(TraceItem::Value(&raw const node.value));
                }
            }
        }
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

fn schedule_value_pointer(
    pointer: *const ValueSlot,
    index: &HeapIndex,
    marks: &mut Marks,
    work: &mut Vec<TraceItem>,
) {
    if pointer.is_null() {
        return;
    }
    let address = pointer as usize;
    if index.values.contains(&address) && !marks.values.insert(address) {
        return;
    }
    // SAFETY: checked MIR only stores live Value pointers in managed fields;
    // untracked pointers belong to the runtime result arena or process arena.
    work.push(TraceItem::Value(pointer));
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

struct TypedRewriteContext<'maps> {
    typed_objects: &'maps HashMap<usize, *mut c_void>,
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

unsafe extern "C" fn rewrite_typed_slot(slot: *mut *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    let context = unsafe { &*context.cast::<TypedRewriteContext<'_>>() };
    // SAFETY: typed root validation established pointer-sized writable storage.
    let address = unsafe { slot.read() } as usize;
    if let Some(pointer) = context.typed_objects.get(&address) {
        unsafe { slot.write(*pointer) };
    }
}

fn rewrite_typed_object(
    allocation: &mut TypedAllocation,
    typed_objects: &HashMap<usize, *mut c_void>,
) {
    for &offset in &allocation.pointer_offsets {
        // SAFETY: copied descriptor metadata proved this aligned cell is fully
        // inside the allocation, and collection has exclusive heap access.
        let slot = unsafe { allocation.pointer_cell(offset) };
        let address = unsafe { slot.read() } as usize;
        if let Some(pointer) = typed_objects.get(&address) {
            unsafe { slot.write(*pointer) };
        }
    }
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
    let typed_root_top = runtime_ref.typed_root_top;
    let typed_root_depth = runtime_ref.typed_root_depth;
    // SAFETY: the attached runtime and executor are separate stable
    // allocations. The scheduler owns `&mut executor` at this safepoint, so
    // no generated code can access the heap while its task roots are traced.
    let status = unsafe {
        collect_heap(
            &mut runtime_ref.heap,
            &mut executor.tasks,
            root_top,
            root_depth,
            typed_root_top,
            typed_root_depth,
            force,
        )
    };
    if status != GC_OK {
        std::process::abort();
    }
}

/// Runs an explicit precise moving collection for the active Runtime.
///
/// Managed allocation slowpaths share this collector but do not call the
/// exported symbol. The compiler must publish every live universal `Value` or
/// typed managed pointer in its matching root chain before calling either this
/// safepoint or a managed allocator; attached coroutine Task roots are traced
/// in the same collection.
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
    let typed_root_top = runtime_ref.typed_root_top;
    let typed_root_depth = runtime_ref.typed_root_depth;
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
                typed_root_top,
                typed_root_depth,
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
            typed_root_top,
            typed_root_depth,
            force,
        )
    }
}

unsafe fn validate_root_chains(
    root_top: *mut LoomGcRootFrame,
    root_depth: u64,
    typed_root_top: *mut LoomGcTypedRootFrame,
    typed_root_depth: u64,
) -> i32 {
    let status = unsafe { visit_sync_roots(root_top, root_depth, None, ptr::null_mut()) };
    if status != GC_OK {
        return status;
    }
    unsafe { visit_typed_roots(typed_root_top, typed_root_depth, None, ptr::null_mut()) }
}

unsafe fn trace_scheduler_roots(
    tasks: &[Box<LoomTask>],
    trace_context: &mut TraceContext,
    typed_trace_context: &mut TypedTraceContext,
) {
    for task in tasks {
        let task = (&raw const **task).cast_mut();
        unsafe { trace_task_roots(task, Some(trace_slot), ptr::from_mut(trace_context).cast()) };
        unsafe {
            trace_typed_task_roots(
                task,
                Some(trace_typed_slot),
                ptr::from_mut(typed_trace_context).cast(),
            );
        }
    }
}

unsafe fn collect_heap(
    heap: &mut LoomHeap,
    tasks: &mut [Box<LoomTask>],
    root_top: *mut LoomGcRootFrame,
    root_depth: u64,
    typed_root_top: *mut LoomGcTypedRootFrame,
    typed_root_depth: u64,
    force: bool,
) -> i32 {
    let root_status =
        unsafe { validate_root_chains(root_top, root_depth, typed_root_top, typed_root_depth) };
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
        && heap.typed_objects.is_empty()
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
        work: Vec::new(),
    };
    let mut typed_trace_context = TypedTraceContext {
        index: &raw const index,
        marks: &raw mut marks,
        work: Vec::new(),
    };
    unsafe { trace_scheduler_roots(tasks, &mut trace_context, &mut typed_trace_context) };
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
    let root_status = unsafe {
        visit_typed_roots(
            typed_root_top,
            typed_root_depth,
            Some(trace_typed_slot),
            (&raw mut typed_trace_context).cast(),
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
        .saturating_add(heap.typed_objects.len())
        .saturating_add(heap.witnesses.len());
    heap.values
        .retain(|value| marks.values.contains(&((&raw const **value) as usize)));
    heap.nodes
        .retain(|node| marks.nodes.contains(&((&raw const **node) as usize)));
    heap.sequences
        .retain(|sequence| marks.sequences.contains(&(sequence.as_ptr() as usize)));
    heap.typed_objects
        .retain(|object| marks.typed_objects.contains(&object.address()));
    heap.witnesses.retain_marked(&marks.witnesses);
    let after = heap
        .values
        .len()
        .saturating_add(heap.nodes.len())
        .saturating_add(heap.sequences.len())
        .saturating_add(heap.typed_objects.len())
        .saturating_add(heap.witnesses.len());
    heap.reclaimed = heap
        .reclaimed
        .saturating_add((before.saturating_sub(after)) as u64);

    unsafe {
        relocate_marked_heap(
            heap,
            tasks,
            root_top,
            root_depth,
            typed_root_top,
            typed_root_depth,
        )
    }
}

struct FromSpace {
    values: Vec<Box<ValueSlot>>,
    nodes: Vec<Box<ValueNode>>,
    sequences: Vec<Box<[u64]>>,
    typed_objects: Vec<TypedAllocation>,
}

struct HeapRelocation {
    from_space: FromSpace,
    values: HashMap<usize, *mut ValueSlot>,
    nodes: HashMap<usize, *mut ValueNode>,
    sequences: HashMap<usize, *mut c_void>,
    typed_objects: HashMap<usize, *mut c_void>,
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
        typed_objects: std::mem::take(&mut heap.typed_objects),
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
    let mut typed_objects = Vec::with_capacity(from_space.typed_objects.len());
    let mut typed_object_moves = HashMap::with_capacity(from_space.typed_objects.len());
    for object in &from_space.typed_objects {
        let old = object.address();
        let replacement = object.evacuate();
        let new = replacement.pointer();
        typed_objects.push(replacement);
        typed_object_moves.insert(old, new);
    }
    heap.values = values;
    heap.nodes = nodes;
    heap.sequences = sequences;
    heap.typed_objects = typed_objects;
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
    debug_assert!(
        typed_object_moves
            .values()
            .all(|pointer| !typed_object_moves.contains_key(&(*pointer as usize)))
    );
    HeapRelocation {
        from_space,
        values: value_moves,
        nodes: node_moves,
        sequences: sequence_moves,
        typed_objects: typed_object_moves,
    }
}

unsafe fn relocate_marked_heap(
    heap: &mut LoomHeap,
    tasks: &mut [Box<LoomTask>],
    root_top: *mut LoomGcRootFrame,
    root_depth: u64,
    typed_root_top: *mut LoomGcTypedRootFrame,
    typed_root_depth: u64,
) -> i32 {
    let HeapRelocation {
        from_space,
        values: value_moves,
        nodes: node_moves,
        sequences: sequence_moves,
        typed_objects: typed_object_moves,
    } = evacuate_marked_heap(heap);
    heap.relocations = heap.relocations.saturating_add(
        (value_moves
            .len()
            .saturating_add(node_moves.len())
            .saturating_add(sequence_moves.len())
            .saturating_add(typed_object_moves.len())) as u64,
    );

    let mut rewrite_context = RewriteContext {
        values: &value_moves,
        nodes: &node_moves,
        sequences: &sequence_moves,
    };
    for task in tasks.iter_mut() {
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
    let mut typed_rewrite_context = TypedRewriteContext {
        typed_objects: &typed_object_moves,
    };
    for task in tasks.iter_mut() {
        unsafe {
            trace_typed_task_roots(
                &raw mut **task,
                Some(rewrite_typed_slot),
                (&raw mut typed_rewrite_context).cast(),
            );
        }
    }
    let root_status = unsafe {
        visit_typed_roots(
            typed_root_top,
            typed_root_depth,
            Some(rewrite_typed_slot),
            (&raw mut typed_rewrite_context).cast(),
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
    for object in &mut heap.typed_objects {
        rewrite_typed_object(object, &typed_object_moves);
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
        .saturating_add(
            heap.typed_objects
                .iter()
                .map(TypedAllocation::allocation_bytes)
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

    struct TestTypedRootFrame<const ROOTS: usize> {
        roots: Box<[*mut c_void; ROOTS]>,
        slots: Box<[*mut c_void; ROOTS]>,
        _live_bitmaps: Box<[u64]>,
        _descriptor: Box<LoomGcTypedRootDescriptor>,
        header: Box<LoomGcTypedRootFrame>,
    }

    impl<const ROOTS: usize> TestTypedRootFrame<ROOTS> {
        fn new(state_count: usize, live_bitmaps: &[u64]) -> Self {
            assert!(ROOTS > 0 && state_count > 0);
            let bitmap_words = ROOTS.div_ceil(64);
            assert_eq!(live_bitmaps.len(), state_count * bitmap_words);
            let mut roots = Box::new([ptr::null_mut::<c_void>(); ROOTS]);
            let slots = Box::new(std::array::from_fn(|index| {
                (&raw mut roots[index]).cast::<c_void>()
            }));
            let live_bitmaps = live_bitmaps.to_vec().into_boxed_slice();
            let descriptor = Box::new(LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: ROOTS as u64,
                state_count: state_count as u64,
                live_bitmap_words: bitmap_words as u64,
                live_bitmaps: live_bitmaps.as_ptr(),
            });
            let header = Box::new(LoomGcTypedRootFrame {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
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
                _descriptor: descriptor,
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

        fn pointer(&mut self) -> *mut LoomGcTypedRootFrame {
            &raw mut *self.header
        }
    }

    #[repr(C)]
    struct TestTypedLeaf {
        marker: u64,
        checksum: u64,
    }

    #[repr(C)]
    struct TestTypedParent {
        child: *mut c_void,
        marker: u64,
    }

    fn typed_leaf_descriptor() -> LoomGcObjectDescriptor {
        LoomGcObjectDescriptor {
            abi_version: TYPED_GC_ABI_VERSION,
            flags: 0,
            fixed_size: size_of::<TestTypedLeaf>() as u64,
            object_align: align_of::<TestTypedLeaf>() as u64,
            pointer_count: 0,
            pointer_offsets: ptr::null(),
        }
    }

    unsafe fn typed_allocate(
        descriptor: *const LoomGcObjectDescriptor,
        allocation_size: usize,
    ) -> *mut c_void {
        let mut output = ptr::null_mut();
        assert_eq!(
            unsafe { typed_alloc_v1(descriptor, allocation_size as u64, &raw mut output) },
            GC_OK,
        );
        assert!(!output.is_null());
        output
    }

    fn indirect(pointer: *mut ValueSlot) -> ValueSlot {
        let mut value = ValueSlot::default();
        value.words[VALUE_WORD_TAG] = VALUE_TAG_REFINED;
        value.words[VALUE_WORD_DATA] = pointer as u64;
        value
    }

    fn integer(value: i64) -> ValueSlot {
        let mut slot = ValueSlot::default();
        slot.words[VALUE_WORD_TAG] = loom_runtime_abi::VALUE_TAG_INT;
        slot.words[VALUE_WORD_SCALAR] = value.cast_unsigned();
        slot
    }

    unsafe fn value_chain(values: &[ValueSlot]) -> *mut ValueNode {
        let mut head = ptr::null_mut();
        for value in values.iter().rev() {
            let node = allocate_value_node().cast::<ValueNode>();
            assert!(!node.is_null());
            unsafe {
                (*node).value = *value;
                (*node).next = head;
            }
            head = node;
        }
        head
    }

    unsafe fn chain_values(mut node: *const ValueNode, count: usize) -> Vec<ValueSlot> {
        let mut values = Vec::with_capacity(count);
        for _ in 0..count {
            assert!(!node.is_null());
            values.push(unsafe { (*node).value });
            node = unsafe { (*node).next };
        }
        assert!(node.is_null());
        values
    }

    unsafe extern "C" fn completed_task(_task: *mut LoomTask, _executor: *mut LoomExecutor) -> i32 {
        TASK_COMPLETED
    }

    unsafe fn force_next_safepoint(runtime: *mut LoomRuntime) {
        unsafe { (*runtime).heap.next_gc_threshold = 0 };
    }

    #[test]
    fn typed_graph_moves_rewrites_aliases_and_reclaims_without_an_executor() {
        static IMMORTAL_WORD: u64 = 0xdecaf_bad5eed;
        const TRAILING_BYTES: &[u8] = b"trailing-typed-bytes";

        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let mut frame = TestTypedRootFrame::<3>::all_live();
        let leaf_descriptor = typed_leaf_descriptor();
        let mut parent_offsets = [0_u64];
        let mut parent_descriptor = LoomGcObjectDescriptor {
            abi_version: TYPED_GC_ABI_VERSION,
            flags: 0,
            fixed_size: size_of::<TestTypedParent>() as u64,
            object_align: align_of::<TestTypedParent>() as u64,
            pointer_count: parent_offsets.len() as u64,
            pointer_offsets: parent_offsets.as_ptr(),
        };
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert!((*runtime).attached_executor_pointer().is_null());
            assert_eq!(typed_root_push_v1(frame.pointer()), GC_OK);

            let child_allocation_size = size_of::<TestTypedLeaf>() + TRAILING_BYTES.len();
            let original_child = typed_allocate(&raw const leaf_descriptor, child_allocation_size)
                .cast::<TestTypedLeaf>();
            assert_eq!((*original_child).marker, 0);
            assert_eq!((*original_child).checksum, 0);
            (*original_child).marker = 41;
            (*original_child).checksum = 43;
            ptr::copy_nonoverlapping(
                TRAILING_BYTES.as_ptr(),
                original_child.cast::<u8>().add(size_of::<TestTypedLeaf>()),
                TRAILING_BYTES.len(),
            );
            frame.roots[0] = original_child.cast();

            // Exercise the allocation safepoint: the child is direct-pointer
            // rooted and must be reloaded after the parent allocation.
            (*runtime).heap.collect_before_every_allocation = true;
            let parent = typed_allocate(&raw const parent_descriptor, size_of::<TestTypedParent>())
                .cast::<TestTypedParent>();
            (*runtime).heap.collect_before_every_allocation = false;
            assert_ne!(frame.roots[0], original_child.cast());
            assert_eq!(
                std::slice::from_raw_parts(
                    frame.roots[0].cast::<u8>().add(size_of::<TestTypedLeaf>()),
                    TRAILING_BYTES.len(),
                ),
                TRAILING_BYTES,
            );
            assert!((*parent).child.is_null());
            assert_eq!((*parent).marker, 0);
            (*parent).child = frame.roots[0];
            (*parent).marker = 47;

            frame.roots[0] = parent.cast();
            frame.roots[1] = parent.cast();
            frame.roots[2] = (&raw const IMMORTAL_WORD).cast_mut().cast();

            // Allocation metadata is copied. Mutating caller-owned descriptor
            // storage after allocation must not change the managed shape.
            parent_offsets[0] = size_of::<*mut c_void>() as u64;
            parent_descriptor.pointer_count = 0;
            parent_descriptor.pointer_offsets = ptr::null();
            assert_eq!(parent_offsets[0], size_of::<*mut c_void>() as u64);
            assert_eq!(parent_descriptor.pointer_count, 0);
            assert!(parent_descriptor.pointer_offsets.is_null());

            let aligned_descriptor = LoomGcObjectDescriptor {
                object_align: 64,
                ..leaf_descriptor
            };
            let dead = typed_allocate(&raw const aligned_descriptor, size_of::<TestTypedLeaf>())
                .cast::<TestTypedLeaf>();
            assert_eq!((dead as usize) % 64, 0);
            (*dead).marker = 99;
            let old_parent = frame.roots[0];
            let old_child = (*parent).child;
            let immortal = frame.roots[2];

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_ne!(frame.roots[0], old_parent);
            assert_eq!(frame.roots[0], frame.roots[1]);
            assert_eq!(frame.roots[2], immortal);
            let moved_parent = frame.roots[0].cast::<TestTypedParent>();
            assert_eq!((*moved_parent).marker, 47);
            assert_ne!((*moved_parent).child, old_child);
            let moved_child = (*moved_parent).child.cast::<TestTypedLeaf>();
            assert_eq!((*moved_child).marker, 41);
            assert_eq!((*moved_child).checksum, 43);
            assert_eq!(
                std::slice::from_raw_parts(
                    moved_child.cast::<u8>().add(size_of::<TestTypedLeaf>()),
                    TRAILING_BYTES.len(),
                ),
                TRAILING_BYTES,
            );
            assert_eq!((*runtime).heap.typed_object_count(), 2);
            assert_eq!((*runtime).heap.reclaimed, 1);
            assert!((*runtime).attached_executor_pointer().is_null());

            frame.roots[0] = ptr::null_mut();
            frame.roots[1] = ptr::null_mut();
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.typed_object_count(), 0);
            assert_eq!(frame.roots[2], immortal);

            assert_eq!(typed_root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn typed_cycles_are_traced_and_rewritten() {
        let runtime = runtime_create_v1();
        let mut frame = TestTypedRootFrame::<1>::all_live();
        let pointer_offsets = [0_u64];
        let descriptor = LoomGcObjectDescriptor {
            abi_version: TYPED_GC_ABI_VERSION,
            flags: 0,
            fixed_size: size_of::<TestTypedParent>() as u64,
            object_align: align_of::<TestTypedParent>() as u64,
            pointer_count: 1,
            pointer_offsets: pointer_offsets.as_ptr(),
        };
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(typed_root_push_v1(frame.pointer()), GC_OK);
            let first = typed_allocate(&raw const descriptor, size_of::<TestTypedParent>())
                .cast::<TestTypedParent>();
            frame.roots[0] = first.cast();
            let second = typed_allocate(&raw const descriptor, size_of::<TestTypedParent>())
                .cast::<TestTypedParent>();
            (*first).child = second.cast();
            (*first).marker = 11;
            (*second).child = first.cast();
            (*second).marker = 13;

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            let moved_first = frame.roots[0].cast::<TestTypedParent>();
            let moved_second = (*moved_first).child.cast::<TestTypedParent>();
            assert_ne!(moved_first, first);
            assert_ne!(moved_second, second);
            assert_eq!((*moved_first).marker, 11);
            assert_eq!((*moved_second).marker, 13);
            assert_eq!((*moved_second).child, moved_first.cast());
            assert_eq!((*runtime).heap.typed_object_count(), 2);

            frame.roots[0] = ptr::null_mut();
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.typed_object_count(), 0);
            assert_eq!(typed_root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn typed_root_state_selects_only_live_cells() {
        let runtime = runtime_create_v1();
        let mut frame = TestTypedRootFrame::<2>::new(2, &[0b01, 0b10]);
        let descriptor = typed_leaf_descriptor();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(typed_root_push_v1(frame.pointer()), GC_OK);
            let first = typed_allocate(&raw const descriptor, size_of::<TestTypedLeaf>());
            let initially_dead = typed_allocate(&raw const descriptor, size_of::<TestTypedLeaf>());
            frame.roots[0] = first;
            frame.roots[1] = initially_dead;

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.typed_object_count(), 1);
            assert_ne!(frame.roots[0], first);
            assert_eq!(frame.roots[1], initially_dead);

            let second = typed_allocate(&raw const descriptor, size_of::<TestTypedLeaf>());
            frame.roots[1] = second;
            frame.header.state = 1;
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.typed_object_count(), 1);
            assert_ne!(frame.roots[1], second);

            assert_eq!(typed_root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn legacy_and_typed_roots_coexist_and_are_validated_before_collection() {
        let runtime = runtime_create_v1();
        let mut legacy = TestRootFrame::<1>::all_live();
        let mut typed = TestTypedRootFrame::<1>::all_live();
        let descriptor = typed_leaf_descriptor();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(legacy.pointer()), GC_OK);
            assert_eq!(typed_root_push_v1(typed.pointer()), GC_OK);
            let legacy_before = allocate_value().cast::<ValueSlot>();
            let typed_before = typed_allocate(&raw const descriptor, size_of::<TestTypedLeaf>());
            legacy.roots[0] = indirect(legacy_before);
            typed.roots[0] = typed_before;

            force_next_safepoint(runtime);
            let collections_before = (*runtime).heap.collections;
            typed.header.state = 1;
            assert_eq!(safepoint_v1(), GC_DESCRIPTOR_INVALID);
            assert_eq!((*runtime).heap.collections, collections_before);
            assert_eq!(legacy.roots[0].words[VALUE_WORD_DATA], legacy_before as u64);
            assert_eq!(typed.roots[0], typed_before);

            typed.header.state = 0;
            assert_eq!(safepoint_v1(), GC_OK);
            assert_ne!(legacy.roots[0].words[VALUE_WORD_DATA], legacy_before as u64);
            assert_ne!(typed.roots[0], typed_before);
            assert_eq!((*runtime).heap.values.len(), 1);
            assert_eq!((*runtime).heap.typed_object_count(), 1);

            // The chains are independent; cross-chain pop order is irrelevant.
            assert_eq!(root_pop_v1(legacy.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_ROOT_STACK_NOT_EMPTY);
            assert_eq!(typed_root_pop_v1(typed.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn typed_allocation_descriptors_are_strict() {
        let runtime = runtime_create_v1();
        let base = typed_leaf_descriptor();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            let mut output = ptr::dangling_mut::<c_void>();

            let mut descriptor = base;
            descriptor.abi_version += 1;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_ABI_MISMATCH,
            );
            assert!(output.is_null());

            descriptor = base;
            descriptor.flags = 1;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_DESCRIPTOR_INVALID,
            );
            descriptor = base;
            descriptor.object_align = 3;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_DESCRIPTOR_INVALID,
            );
            descriptor = base;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size - 1,
                    &raw mut output,
                ),
                GC_DESCRIPTOR_INVALID,
            );

            let unaligned_offsets = [1_u64];
            descriptor = base;
            descriptor.pointer_count = 1;
            descriptor.pointer_offsets = unaligned_offsets.as_ptr();
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_DESCRIPTOR_INVALID,
            );
            let descending_offsets = [8_u64, 0];
            descriptor.pointer_count = 2;
            descriptor.pointer_offsets = descending_offsets.as_ptr();
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_DESCRIPTOR_INVALID,
            );
            let outside_offsets = [16_u64];
            descriptor.pointer_count = 1;
            descriptor.pointer_offsets = outside_offsets.as_ptr();
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_DESCRIPTOR_INVALID,
            );

            assert!(output.is_null());
            assert_eq!((*runtime).heap.typed_object_count(), 0);
            assert_eq!((*runtime).heap.collections, 0);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn typed_allocation_resource_limits_and_inactive_runtime_are_rejected() {
        let runtime = runtime_create_v1();
        let base = typed_leaf_descriptor();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            let mut output = ptr::dangling_mut::<c_void>();
            let mut descriptor = base;
            descriptor.pointer_count = GC_MAX_OBJECT_POINTERS + 1;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_RESOURCE_LIMIT,
            );
            descriptor = base;
            descriptor.object_align = GC_MAX_OBJECT_ALIGNMENT * 2;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    descriptor.fixed_size,
                    &raw mut output,
                ),
                GC_RESOURCE_LIMIT,
            );
            descriptor = base;
            assert_eq!(
                typed_alloc_v1(
                    &raw const descriptor,
                    GC_MAX_OBJECT_BYTES + 1,
                    &raw mut output,
                ),
                GC_RESOURCE_LIMIT,
            );
            assert!(output.is_null());
            assert_eq!((*runtime).heap.typed_object_count(), 0);
            assert_eq!((*runtime).heap.collections, 0);

            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            output = ptr::dangling_mut::<c_void>();
            assert_eq!(
                typed_alloc_v1(&raw const base, base.fixed_size, &raw mut output,),
                GC_INVALID_ARGUMENT,
            );
            assert!(output.is_null());
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn universal_and_typed_root_maps_share_total_resource_bounds() {
        let runtime = runtime_create_v1();
        let bitmap = [0_u64];
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);

            let typed_descriptor = LoomGcTypedRootDescriptor {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: GC_MAX_ROOT_SLOTS,
                state_count: GC_MAX_ROOT_BITMAP_WORDS / GC_MAX_ROOT_SLOTS.div_ceil(64) + 1,
                live_bitmap_words: GC_MAX_ROOT_SLOTS.div_ceil(64),
                live_bitmaps: bitmap.as_ptr(),
            };
            let mut typed_frame = LoomGcTypedRootFrame {
                abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 0,
                descriptor: &raw const typed_descriptor,
                slots: ptr::null(),
                previous: ptr::null_mut(),
            };
            assert_eq!(typed_root_push_v1(&raw mut typed_frame), GC_RESOURCE_LIMIT,);

            let legacy_descriptor = LoomGcRootDescriptor {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                slot_count: typed_descriptor.slot_count,
                state_count: typed_descriptor.state_count,
                live_bitmap_words: typed_descriptor.live_bitmap_words,
                live_bitmaps: bitmap.as_ptr(),
            };
            let mut legacy_frame = LoomGcRootFrame {
                abi_version: SHADOW_STACK_ABI_VERSION,
                flags: 0,
                state: 0,
                descriptor: &raw const legacy_descriptor,
                slots: ptr::null(),
                previous: ptr::null_mut(),
            };
            assert_eq!(root_push_v1(&raw mut legacy_frame), GC_RESOURCE_LIMIT);

            let mut malformed = TestTypedRootFrame::<1>::new(1, &[0b10]);
            assert_eq!(
                typed_root_push_v1(malformed.pointer()),
                GC_DESCRIPTOR_INVALID,
            );
            let mut bad_slot = TestTypedRootFrame::<1>::all_live();
            bad_slot.slots[0] = ptr::dangling_mut::<c_void>();
            bad_slot.header.slots = bad_slot.slots.as_ptr();
            assert_eq!(
                typed_root_push_v1(bad_slot.pointer()),
                GC_DESCRIPTOR_INVALID,
            );

            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
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
    fn value_clone_supports_aliasing_and_rejects_an_overlong_chain() {
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<1>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            let head = value_chain(&[integer(10), integer(20)]);
            frame.roots[0] = synthetic_list(head, 2);
            let root = (&raw mut frame.roots[0]).cast::<c_void>();
            assert_eq!(clone_value_v1(root, root.cast_const()), GC_OK);
            let cloned = chain_values(frame.roots[0].words[VALUE_WORD_DATA] as *const ValueNode, 2);
            assert_eq!(cloned[0].words[VALUE_WORD_SCALAR], 10);
            assert_eq!(cloned[1].words[VALUE_WORD_SCALAR], 20);
            assert_eq!((*runtime).sync_root_depth, 1);

            let malformed = value_chain(&[integer(1), integer(2)]);
            frame.roots[0] = synthetic_list(malformed, 1);
            assert_eq!(clone_value_v1(root, root.cast_const()), GC_INVALID_ARGUMENT);
            assert_eq!(
                frame.roots[0].words[VALUE_WORD_TAG],
                loom_runtime_abi::VALUE_TAG_UNIT,
            );
            assert_eq!((*runtime).sync_root_depth, 1);

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn clone_preserves_enum_outcome_and_normalized_dyn_semantics() {
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<2>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);

            let enum_head = value_chain(&[integer(7), integer(9)]);
            frame.roots[0].words[VALUE_WORD_TAG] = VALUE_TAG_ENUM;
            frame.roots[0].words[loom_runtime_abi::VALUE_WORD_NOMINAL] = 41;
            frame.roots[0].words[VALUE_WORD_AUX] = 3;
            frame.roots[0].words[VALUE_WORD_SCALAR] = 2;
            frame.roots[0].words[VALUE_WORD_DATA] = enum_head as u64;
            assert_eq!(
                clone_value_v1(
                    (&raw mut frame.roots[1]).cast(),
                    (&raw const frame.roots[0]).cast(),
                ),
                GC_OK,
            );
            assert_eq!(frame.roots[1].words[VALUE_WORD_TAG], VALUE_TAG_ENUM);
            assert_eq!(frame.roots[1].words[VALUE_WORD_AUX], 3);
            assert_eq!(frame.roots[1].words[VALUE_WORD_SCALAR], 2);
            assert_eq!(
                chain_values(frame.roots[1].words[VALUE_WORD_DATA] as *const ValueNode, 2,)
                    .iter()
                    .map(|value| value.words[VALUE_WORD_SCALAR])
                    .collect::<Vec<_>>(),
                vec![7, 9],
            );

            let dyn_inner = allocate_value().cast::<ValueSlot>();
            assert!(!dyn_inner.is_null());
            dyn_inner.write(integer(88));
            frame.roots[0] = ValueSlot::default();
            frame.roots[0].words[VALUE_WORD_TAG] = VALUE_TAG_DYN;
            frame.roots[0].words[VALUE_WORD_AUX] = DYN_FLAG_MUTABLE | 0x80;
            frame.roots[0].words[VALUE_WORD_SCALAR] = 123;
            frame.roots[0].words[VALUE_WORD_DATA] = dyn_inner as u64;
            assert_eq!(
                clone_value_v1(
                    (&raw mut frame.roots[1]).cast(),
                    (&raw const frame.roots[0]).cast(),
                ),
                GC_OK,
            );
            let cloned_dyn_inner = frame.roots[1].words[VALUE_WORD_DATA] as *const ValueSlot;
            assert!(!cloned_dyn_inner.is_null());
            assert_ne!(cloned_dyn_inner, dyn_inner);
            assert_eq!(frame.roots[1].words[VALUE_WORD_AUX], DYN_FLAG_MUTABLE);
            assert_eq!(frame.roots[1].words[VALUE_WORD_SCALAR], 0);
            assert_eq!((*cloned_dyn_inner).words[VALUE_WORD_SCALAR], 88);

            let outcome_inner = allocate_value().cast::<ValueSlot>();
            assert!(!outcome_inner.is_null());
            outcome_inner.write(integer(55));
            frame.roots[0] = ValueSlot::default();
            frame.roots[0].words[VALUE_WORD_TAG] = VALUE_TAG_TASK_OUTCOME;
            frame.roots[0].words[VALUE_WORD_AUX] = TASK_COMPLETED as u64;
            frame.roots[0].words[VALUE_WORD_DATA] = outcome_inner as u64;
            assert_eq!(
                clone_value_v1(
                    (&raw mut frame.roots[1]).cast(),
                    (&raw const frame.roots[0]).cast(),
                ),
                GC_OK,
            );
            let cloned_outcome_inner = frame.roots[1].words[VALUE_WORD_DATA] as *const ValueSlot;
            assert!(!cloned_outcome_inner.is_null());
            assert_ne!(cloned_outcome_inner, outcome_inner);
            assert_eq!((*cloned_outcome_inner).words[VALUE_WORD_SCALAR], 55);

            frame.roots[0].words[VALUE_WORD_AUX] = 1;
            assert_eq!(
                clone_value_v1(
                    (&raw mut frame.roots[1]).cast(),
                    (&raw const frame.roots[0]).cast(),
                ),
                GC_OK,
            );
            assert_eq!(frame.roots[1].words[VALUE_WORD_DATA], outcome_inner as u64);

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn clone_and_builder_survive_collection_before_every_allocation() {
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<6>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            let inner = allocate_value().cast::<ValueSlot>();
            assert!(!inner.is_null());
            inner.write(integer(101));
            frame.roots[0] = indirect(inner);
            frame.roots[1] = text_value(b"moving text").expect("managed Text");
            frame.roots[2] = integer(303);
            let inputs: [*const c_void; 3] = [
                (&raw const frame.roots[0]).cast(),
                (&raw const frame.roots[1]).cast(),
                (&raw const frame.roots[2]).cast(),
            ];
            let inner_before = frame.roots[0].words[VALUE_WORD_DATA];
            (*runtime).heap.collect_before_every_allocation = true;
            assert_eq!(
                build_value_nodes_v1(
                    (&raw mut frame.roots[3]).cast(),
                    inputs.as_ptr(),
                    inputs.len() as u64,
                ),
                GC_OK,
            );
            assert_ne!(frame.roots[0].words[VALUE_WORD_DATA], inner_before);
            {
                // This snapshot is valid only until the next managed
                // allocation. Later assertions reload the rooted chain after
                // clone_value's forced collection boundaries.
                let built = chain_values(
                    frame.roots[3].words[VALUE_WORD_DATA] as *const ValueNode,
                    inputs.len(),
                );
                assert_eq!(
                    built[0].words[VALUE_WORD_DATA],
                    frame.roots[0].words[VALUE_WORD_DATA]
                );
                assert_eq!(text::text_value_bytes(&built[1]), Some(&b"moving text"[..]),);
                assert_eq!(built[2].words[VALUE_WORD_SCALAR], 303);
            }

            frame.roots[4].words[VALUE_WORD_TAG] = VALUE_TAG_TUPLE;
            frame.roots[4].words[VALUE_WORD_AUX] = inputs.len() as u64;
            frame.roots[4].words[VALUE_WORD_DATA] = frame.roots[3].words[VALUE_WORD_DATA];
            let collections_before = (*runtime).heap.collections;
            assert_eq!(
                clone_value_v1(
                    (&raw mut frame.roots[5]).cast(),
                    (&raw const frame.roots[4]).cast(),
                ),
                GC_OK,
            );
            assert!((*runtime).heap.collections >= collections_before + inputs.len() as u64);
            let source = chain_values(
                frame.roots[3].words[VALUE_WORD_DATA] as *const ValueNode,
                inputs.len(),
            );
            let cloned = chain_values(
                frame.roots[5].words[VALUE_WORD_DATA] as *const ValueNode,
                inputs.len(),
            );
            assert_eq!(
                source[0].words[VALUE_WORD_DATA],
                frame.roots[0].words[VALUE_WORD_DATA],
            );
            assert_eq!(cloned[0].words[VALUE_WORD_TAG], VALUE_TAG_REFINED);
            assert_ne!(
                cloned[0].words[VALUE_WORD_DATA],
                source[0].words[VALUE_WORD_DATA],
            );
            assert_eq!(
                (*(source[0].words[VALUE_WORD_DATA] as *const ValueSlot)).words[VALUE_WORD_SCALAR],
                101,
            );
            assert_eq!(
                (*(cloned[0].words[VALUE_WORD_DATA] as *const ValueSlot)).words[VALUE_WORD_SCALAR],
                101,
            );
            assert_eq!(
                text::text_value_bytes(&source[1]),
                Some(&b"moving text"[..])
            );
            assert_eq!(
                text::text_value_bytes(&cloned[1]),
                Some(&b"moving text"[..])
            );
            assert_eq!(source[2].words[VALUE_WORD_SCALAR], 303);
            assert_eq!(cloned[2].words[VALUE_WORD_SCALAR], 303);
            (*runtime).heap.collect_before_every_allocation = false;

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn list_add_reloads_roots_and_tail_after_allocator_collection() {
        let runtime = runtime_create_v1();
        unsafe {
            assert!(!runtime.is_null());
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            let mut list = ValueSlot::default();
            list.words[VALUE_WORD_TAG] = VALUE_TAG_LIST;
            let roots = RuntimeRootScope::from_values(vec![list, ValueSlot::default()])
                .expect("runtime root scope");
            (*runtime).heap.collect_before_every_allocation = true;

            roots.write(1, text_value(b"first").expect("managed Text"));
            assert_eq!(list_add(roots.pointer(0), roots.pointer(1)), 0);
            let first_head = roots.read(0).words[VALUE_WORD_DATA];
            roots.write(1, text_value("界🙂".as_bytes()).expect("managed Text"));
            assert_ne!(roots.read(0).words[VALUE_WORD_DATA], first_head);
            assert_eq!(list_add(roots.pointer(0), roots.pointer(1)), 0);
            assert_eq!(roots.read(0).words[VALUE_WORD_AUX], 2);

            assert_eq!(list_get(roots.pointer(0), 0, roots.pointer(1)), 1);
            let first_address = roots.read(1).words[VALUE_WORD_DATA];
            let _trigger = text_value(b"trigger").expect("managed Text");
            assert_ne!(roots.read(1).words[VALUE_WORD_DATA], first_address);
            assert_eq!(text::text_value_bytes(&roots.read(1)), Some(&b"first"[..]));

            let mut second = ValueSlot::default();
            assert_eq!(list_get(roots.pointer(0), 1, &raw mut second), 1);
            assert_eq!(text::text_value_bytes(&second), Some("界🙂".as_bytes()));
            assert!((*runtime).heap.collections >= 5);

            (*runtime).heap.collect_before_every_allocation = false;
            drop(roots);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn deeply_nested_clone_and_gc_trace_are_native_stack_bounded() {
        const DEPTH: usize = 8_192;
        let runtime = runtime_create_v1();
        let mut frame = TestRootFrame::<2>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(root_push_v1(frame.pointer()), GC_OK);
            frame.roots[0] = integer(73);
            for _ in 0..DEPTH {
                let inner = allocate_value().cast::<ValueSlot>();
                assert!(!inner.is_null());
                inner.write(frame.roots[0]);
                frame.roots[0] = indirect(inner);
            }
            // The first clone allocation forces a collection while all
            // explicit CloneWork frames and the deeply nested source graph are
            // live. Both clone and trace must remain native-stack bounded.
            force_next_safepoint(runtime);
            assert_eq!(
                clone_value_v1(
                    (&raw mut frame.roots[1]).cast(),
                    (&raw const frame.roots[0]).cast(),
                ),
                GC_OK,
            );
            assert!((*runtime).heap.collections >= 1);
            let mut source = frame.roots[0];
            let mut cloned = frame.roots[1];
            for _ in 0..DEPTH {
                assert_eq!(source.words[VALUE_WORD_TAG], VALUE_TAG_REFINED);
                assert_eq!(cloned.words[VALUE_WORD_TAG], VALUE_TAG_REFINED);
                assert_ne!(source.words[VALUE_WORD_DATA], cloned.words[VALUE_WORD_DATA]);
                source = *(source.words[VALUE_WORD_DATA] as *const ValueSlot);
                cloned = *(cloned.words[VALUE_WORD_DATA] as *const ValueSlot);
            }
            assert_eq!(source.words[VALUE_WORD_SCALAR], 73);
            assert_eq!(cloned.words[VALUE_WORD_SCALAR], 73);
            assert_eq!((*runtime).sync_root_depth, 1);

            assert_eq!(root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
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
