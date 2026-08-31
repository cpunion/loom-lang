//! Precise moving heap for compiler-generated typed Loom objects.
//!
//! Collection runs at compiler-known safepoints, allocation slow paths, or
//! between coroutine resume calls. Generated code publishes direct managed
//! pointers through compiler-described root frames, so relocation never needs
//! a universal tagged value representation or source-visible stable address.

use std::alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::{align_of, size_of};
use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering;

use loom_runtime_abi::{
    GC_ABI_MISMATCH, GC_DESCRIPTOR_INVALID, GC_FRAME_ORDER, GC_INVALID_ARGUMENT,
    GC_MAX_OBJECT_ALIGNMENT, GC_MAX_OBJECT_BYTES, GC_MAX_OBJECT_POINTERS,
    GC_MAX_REPEATED_POINTER_CELLS, GC_MAX_ROOT_BITMAP_WORDS, GC_MAX_ROOT_DEPTH, GC_MAX_ROOT_SLOTS,
    GC_MAX_ROOT_STATES, GC_OK, GC_RESOURCE_LIMIT, GC_ROOT_FRAME_LINKED, GC_ROOT_STACK_NOT_EMPTY,
    LoomGcObjectDescriptor, LoomGcRepeatedObjectDescriptor, LoomGcTypedRootDescriptor,
    LoomGcTypedRootFrame, TYPED_GC_ABI_VERSION, TYPED_GC_REPEATED_ABI_VERSION,
    TYPED_SHADOW_STACK_ABI_VERSION,
};

use crate::reactor::LoomExecutor;
use crate::runtime::LoomRuntime;
use crate::scheduler::{LoomTask, trace_typed_task_roots};

pub(crate) const MIN_GC_THRESHOLD_BYTES: usize = 64 * 1024;

struct TypedAllocation {
    pointer: NonNull<u8>,
    layout: Layout,
    trace: TypedTraceShape,
}

impl TypedAllocation {
    fn new(layout: Layout, trace: TypedTraceShape) -> Self {
        // SAFETY: descriptor validation constructed a nonzero valid layout.
        let pointer = unsafe { alloc_zeroed(layout) };
        let Some(pointer) = NonNull::new(pointer) else {
            handle_alloc_error(layout);
        };
        Self {
            pointer,
            layout,
            trace,
        }
    }

    fn evacuate(&self) -> Self {
        let replacement = Self::new(self.layout, self.trace.clone());
        // SAFETY: both allocations have the same non-overlapping layout.
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
        // SAFETY: copied trace metadata proved that this is an aligned cell
        // wholly inside the allocation.
        unsafe { self.pointer.as_ptr().add(offset).cast::<*mut c_void>() }
    }

    fn visit_pointer_offsets(&self, mut visit: impl FnMut(usize)) {
        for &offset in &self.trace.fixed_pointer_offsets {
            visit(offset);
        }
        let Some(repeated) = &self.trace.repeated else {
            return;
        };
        for element in 0..repeated.element_count {
            let base = repeated
                .start
                .checked_add(
                    element
                        .checked_mul(repeated.stride)
                        .unwrap_or_else(|| unreachable!("validated repeated stride overflowed")),
                )
                .unwrap_or_else(|| unreachable!("validated repeated shape overflowed"));
            for &offset in &repeated.pointer_offsets {
                visit(
                    base.checked_add(offset)
                        .unwrap_or_else(|| unreachable!("validated element offset overflowed")),
                );
            }
        }
    }
}

impl Drop for TypedAllocation {
    fn drop(&mut self) {
        // SAFETY: this entry uniquely owns an allocation made with this layout.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}

#[derive(Clone)]
struct TypedTraceShape {
    fixed_pointer_offsets: Box<[usize]>,
    repeated: Option<RepeatedTraceShape>,
}

#[derive(Clone)]
struct RepeatedTraceShape {
    start: usize,
    stride: usize,
    element_count: usize,
    pointer_offsets: Box<[usize]>,
}

/// Runtime-owned storage for typed managed objects.
#[derive(Default)]
pub(crate) struct LoomHeap {
    typed_objects: Vec<TypedAllocation>,
    pub(crate) collections: u64,
    pub(crate) relocations: u64,
    pub(crate) reclaimed: u64,
    pub(crate) allocation_charge: usize,
    pub(crate) next_gc_threshold: usize,
    #[cfg(test)]
    pub(crate) collect_on_every_poll: bool,
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
    typed_top: *mut LoomGcTypedRootFrame,
    typed_depth: u64,
}

impl RootBaseline {
    unsafe fn capture(runtime: *const LoomRuntime) -> Self {
        // SAFETY: callers hold the active runtime for this interval.
        unsafe {
            Self {
                typed_top: (*runtime).typed_root_top,
                typed_depth: (*runtime).typed_root_depth,
            }
        }
    }

    unsafe fn matches(self, runtime: *const LoomRuntime) -> bool {
        // SAFETY: callers hold the active runtime for this interval.
        unsafe {
            (*runtime).typed_root_top == self.typed_top
                && (*runtime).typed_root_depth == self.typed_depth
        }
    }
}

#[derive(Clone)]
struct ActivationSnapshot {
    active_runtime: *mut LoomRuntime,
    active_depth: u32,
    root_baselines: Vec<RootBaseline>,
    runtime_depth: u32,
    roots: RootBaseline,
}

/// Recoverable activation around a non-suspending cleanup callback.
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
        // SAFETY: the scheduler retains the executor and attached runtime for
        // the complete callback.
        let runtime = NonNull::new(unsafe { (*executor).runtime_pointer() })?;
        let active_runtime = ACTIVE_RUNTIME.with(Cell::get);
        let active_depth = ACTIVE_DEPTH.with(Cell::get);
        let root_baselines = ACTIVE_ROOT_BASELINES.with(|baselines| baselines.borrow().clone());
        // SAFETY: runtime came from the retained executor attachment.
        let runtime_depth = unsafe { runtime.as_ref().active_depth.load(Ordering::Acquire) };
        // SAFETY: the retained runtime remains readable.
        let roots = unsafe { RootBaseline::capture(runtime.as_ptr()) };
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
            // SAFETY: the runtime remains retained by the executor.
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
        if !observed_runtime.is_null()
            && observed_runtime != self.runtime.as_ptr()
            && observed_runtime != self.snapshot.active_runtime
        {
            // SAFETY: an unexpected runtime could become current only through
            // the activation ABI. Clear its leaked thread ownership first.
            unsafe {
                (*observed_runtime).typed_root_top = ptr::null_mut();
                (*observed_runtime).typed_root_depth = 0;
                (*observed_runtime).active_depth.store(0, Ordering::Release);
            }
        }

        // SAFETY: the scheduler retains this runtime through restoration.
        unsafe {
            let runtime = self.runtime.as_mut();
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

fn enter_runtime(runtime: *mut LoomRuntime) -> bool {
    if runtime.is_null() {
        return false;
    }
    let current_runtime = ACTIVE_RUNTIME.with(Cell::get);
    let current_depth = ACTIVE_DEPTH.with(Cell::get);
    if (!current_runtime.is_null() && current_runtime != runtime) || current_depth == u32::MAX {
        return false;
    }
    // SAFETY: active_depth is atomic and lives for the complete runtime.
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
    if current_depth == 0 && unsafe { (*runtime).has_roots() } {
        let restored = runtime_depth.compare_exchange(1, 0, Ordering::Release, Ordering::Relaxed);
        debug_assert!(restored.is_ok());
        return false;
    }
    if current_depth == 0 {
        // SAFETY: this thread owns the outer generated-code interval.
        unsafe { (*runtime).begin_sync_fault_scope() };
        ACTIVE_RUNTIME.with(|active| active.set(runtime));
    }
    // SAFETY: this thread owns the active runtime interval.
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
    // SAFETY: this thread owns the active runtime interval.
    if !unsafe { baseline.matches(runtime) } {
        return GC_ROOT_STACK_NOT_EMPTY;
    }
    // SAFETY: active_depth lives for the complete runtime.
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
    !runtime.is_null()
        // SAFETY: the caller supplies a live runtime pointer.
        && unsafe { &(*runtime).active_depth }.load(Ordering::Acquire) != 0
}

pub(crate) fn active_runtime_pointer() -> *mut LoomRuntime {
    if ACTIVE_DEPTH.with(|depth| depth.get() == 0) {
        ptr::null_mut()
    } else {
        ACTIVE_RUNTIME.with(Cell::get)
    }
}

pub(crate) fn enter_executor(executor: *mut LoomExecutor) {
    debug_assert!(!executor.is_null());
    // SAFETY: the scheduler retains the executor and its runtime attachment.
    if !enter_runtime(unsafe { (*executor).runtime_pointer() }) {
        std::process::abort();
    }
}

pub(crate) fn leave_executor() {
    if leave_runtime() != GC_OK {
        std::process::abort();
    }
}

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
) -> Result<ValidatedRootShape, i32> {
    if abi_version != expected_abi_version {
        return Err(GC_ABI_MISMATCH);
    }
    if flags != 0 || slot_count == 0 || state_count == 0 {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    if slot_count > GC_MAX_ROOT_SLOTS || state_count > GC_MAX_ROOT_STATES {
        return Err(GC_RESOURCE_LIMIT);
    }
    let expected_bitmap_words = slot_count.div_ceil(64);
    if live_bitmap_words != expected_bitmap_words {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    let Some(total_words) = state_count.checked_mul(live_bitmap_words) else {
        return Err(GC_RESOURCE_LIMIT);
    };
    if total_words > GC_MAX_ROOT_BITMAP_WORDS {
        return Err(GC_RESOURCE_LIMIT);
    }
    if !is_aligned_for(live_bitmaps) {
        return Err(GC_DESCRIPTOR_INVALID);
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
            // SAFETY: the bounded immutable table contains this complete row.
            if unsafe { *live_bitmaps.add(index) } & !allowed != 0 {
                return Err(GC_DESCRIPTOR_INVALID);
            }
        }
    }
    Ok(ValidatedRootShape {
        slot_count,
        bitmap_words,
    })
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
        )
    }
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
        // SAFETY: validated shape and state bound this bitmap row.
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
            // SAFETY: the bounded slot array is immutable while linked.
            let slot = unsafe { *frame.slots.add(index) };
            if !is_aligned_for(slot.cast::<*mut c_void>()) {
                return GC_DESCRIPTOR_INVALID;
            }
        }
    }
    GC_OK
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
        // SAFETY: validation established this frame and its metadata.
        let frame_ref = unsafe { &*frame };
        let descriptor = unsafe { &*frame_ref.descriptor };
        let slot_count = usize::try_from(descriptor.slot_count).unwrap_or_else(|_| unreachable!());
        let bitmap_words =
            usize::try_from(descriptor.live_bitmap_words).unwrap_or_else(|_| unreachable!());
        let state = usize::try_from(frame_ref.state).unwrap_or_else(|_| unreachable!());
        let bitmap_row = state * bitmap_words;
        if let Some(visitor) = visitor {
            for index in 0..slot_count {
                // SAFETY: validation bounded the bitmap row and slot array.
                let word = unsafe { *descriptor.live_bitmaps.add(bitmap_row + index / 64) };
                if word & (1_u64 << (index % 64)) != 0 {
                    let root = unsafe { *frame_ref.slots.add(index) };
                    unsafe { visitor(root.cast(), context) };
                }
            }
        }
        // SAFETY: a linked frame's previous field is runtime-owned.
        frame = unsafe { (*frame).previous };
    }
    if frame.is_null() {
        GC_OK
    } else {
        GC_FRAME_ORDER
    }
}

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
    // SAFETY: activation serializes this root chain.
    let runtime = unsafe { &mut *runtime };
    let Some(depth) = runtime.typed_root_depth.checked_add(1) else {
        return GC_RESOURCE_LIMIT;
    };
    if depth > GC_MAX_ROOT_DEPTH {
        return GC_RESOURCE_LIMIT;
    }
    // SAFETY: the validated unlinked frame remains live until pop.
    unsafe {
        (*frame).previous = runtime.typed_root_top;
        (*frame).flags = GC_ROOT_FRAME_LINKED;
    }
    runtime.typed_root_top = frame;
    runtime.typed_root_depth = depth;
    GC_OK
}

#[unsafe(export_name = "loom_gc_typed_root_pop_v1")]
pub unsafe extern "C" fn typed_root_pop_v1(frame: *mut LoomGcTypedRootFrame) -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() || frame.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: activation serializes this root chain.
    let runtime = unsafe { &mut *runtime };
    if runtime.typed_root_top != frame || runtime.typed_root_depth == 0 {
        return GC_FRAME_ORDER;
    }
    let status = unsafe { validate_typed_root_frame(frame, true) };
    if status != GC_OK {
        return status;
    }
    // SAFETY: top identity and validation establish ownership of these fields.
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

fn managed_allocation_slowpath(runtime: *mut LoomRuntime, incoming: usize) -> i32 {
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: the current generated-code interval exclusively owns this heap.
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
    // The projected charge selected this boundary, so force collection even
    // when the current charge has not reached the threshold yet.
    unsafe { collect_active_runtime(runtime, true) }
}

struct ValidatedObjectShape {
    layout: Layout,
    trace: TypedTraceShape,
}

unsafe fn copy_validated_pointer_offsets(
    pointer_offsets: *const u64,
    pointer_count: u64,
    region_size: usize,
) -> Result<Box<[usize]>, i32> {
    if pointer_count > GC_MAX_OBJECT_POINTERS {
        return Err(GC_RESOURCE_LIMIT);
    }
    if (pointer_count == 0) != pointer_offsets.is_null() {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    if pointer_count != 0 && !is_aligned_for(pointer_offsets) {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    let Ok(pointer_count) = usize::try_from(pointer_count) else {
        return Err(GC_RESOURCE_LIMIT);
    };
    let mut copied = Vec::with_capacity(pointer_count);
    let mut previous = None;
    for index in 0..pointer_count {
        // SAFETY: the ABI supplies the bounded immutable descriptor table.
        let raw_offset = unsafe { *pointer_offsets.add(index) };
        let Ok(offset) = usize::try_from(raw_offset) else {
            return Err(GC_DESCRIPTOR_INVALID);
        };
        let Some(end) = offset.checked_add(size_of::<*mut c_void>()) else {
            return Err(GC_DESCRIPTOR_INVALID);
        };
        if !offset.is_multiple_of(align_of::<*mut c_void>())
            || end > region_size
            || previous.is_some_and(|previous| offset <= previous)
        {
            return Err(GC_DESCRIPTOR_INVALID);
        }
        copied.push(offset);
        previous = Some(offset);
    }
    Ok(copied.into_boxed_slice())
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
    if allocation_size > GC_MAX_OBJECT_BYTES || descriptor.object_align > GC_MAX_OBJECT_ALIGNMENT {
        return Err(GC_RESOURCE_LIMIT);
    }
    if descriptor.pointer_count != 0 && descriptor.object_align < align_of::<*mut c_void>() as u64 {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    let (Ok(allocation_size), Ok(object_align), Ok(fixed_size)) = (
        usize::try_from(allocation_size),
        usize::try_from(descriptor.object_align),
        usize::try_from(descriptor.fixed_size),
    ) else {
        return Err(GC_RESOURCE_LIMIT);
    };
    let Ok(layout) = Layout::from_size_align(allocation_size, object_align) else {
        return Err(GC_DESCRIPTOR_INVALID);
    };
    let pointer_offsets = unsafe {
        copy_validated_pointer_offsets(
            descriptor.pointer_offsets,
            descriptor.pointer_count,
            fixed_size,
        )?
    };
    Ok(ValidatedObjectShape {
        layout,
        trace: TypedTraceShape {
            fixed_pointer_offsets: pointer_offsets,
            repeated: None,
        },
    })
}

unsafe fn validate_repeated_object_descriptor(
    descriptor: *const LoomGcRepeatedObjectDescriptor,
    capacity: u64,
) -> Result<ValidatedObjectShape, i32> {
    if !is_aligned_for(descriptor) {
        return Err(GC_INVALID_ARGUMENT);
    }
    // SAFETY: the ABI requires a readable descriptor at this aligned pointer.
    let descriptor = unsafe { &*descriptor };
    if descriptor.abi_version != TYPED_GC_REPEATED_ABI_VERSION {
        return Err(GC_ABI_MISMATCH);
    }
    if descriptor.flags != 0
        || descriptor.fixed_size == 0
        || descriptor.element_stride == 0
        || descriptor.object_align == 0
        || !descriptor.object_align.is_power_of_two()
    {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    if descriptor.object_align > GC_MAX_OBJECT_ALIGNMENT
        || descriptor.fixed_pointer_count > GC_MAX_OBJECT_POINTERS
        || descriptor.element_pointer_count > GC_MAX_OBJECT_POINTERS
    {
        return Err(GC_RESOURCE_LIMIT);
    }
    let has_pointers = descriptor.fixed_pointer_count != 0 || descriptor.element_pointer_count != 0;
    if has_pointers && descriptor.object_align < align_of::<*mut c_void>() as u64 {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    let repeated_pointer_cells = descriptor
        .element_pointer_count
        .checked_mul(capacity)
        .and_then(|count| count.checked_add(descriptor.fixed_pointer_count))
        .ok_or(GC_RESOURCE_LIMIT)?;
    if repeated_pointer_cells > GC_MAX_REPEATED_POINTER_CELLS {
        return Err(GC_RESOURCE_LIMIT);
    }
    let allocation_size = descriptor
        .element_stride
        .checked_mul(capacity)
        .and_then(|bytes| bytes.checked_add(descriptor.fixed_size))
        .ok_or(GC_RESOURCE_LIMIT)?;
    if allocation_size > GC_MAX_OBJECT_BYTES {
        return Err(GC_RESOURCE_LIMIT);
    }
    let (
        Ok(allocation_size),
        Ok(object_align),
        Ok(fixed_size),
        Ok(element_stride),
        Ok(element_count),
    ) = (
        usize::try_from(allocation_size),
        usize::try_from(descriptor.object_align),
        usize::try_from(descriptor.fixed_size),
        usize::try_from(descriptor.element_stride),
        usize::try_from(capacity),
    )
    else {
        return Err(GC_RESOURCE_LIMIT);
    };
    let Ok(layout) = Layout::from_size_align(allocation_size, object_align) else {
        return Err(GC_DESCRIPTOR_INVALID);
    };
    let fixed_pointer_offsets = unsafe {
        copy_validated_pointer_offsets(
            descriptor.fixed_pointer_offsets,
            descriptor.fixed_pointer_count,
            fixed_size,
        )?
    };
    let pointer_align = align_of::<*mut c_void>();
    if descriptor.element_pointer_count != 0
        && (!fixed_size.is_multiple_of(pointer_align)
            || !element_stride.is_multiple_of(pointer_align))
    {
        return Err(GC_DESCRIPTOR_INVALID);
    }
    let element_pointer_offsets = unsafe {
        copy_validated_pointer_offsets(
            descriptor.element_pointer_offsets,
            descriptor.element_pointer_count,
            element_stride,
        )?
    };
    Ok(ValidatedObjectShape {
        layout,
        trace: TypedTraceShape {
            fixed_pointer_offsets,
            repeated: Some(RepeatedTraceShape {
                start: fixed_size,
                stride: element_stride,
                element_count,
                pointer_offsets: element_pointer_offsets,
            }),
        },
    })
}

pub(crate) unsafe fn allocate_typed_object(
    descriptor: *const LoomGcObjectDescriptor,
    allocation_size: u64,
    output: *mut *mut c_void,
) -> i32 {
    if !is_aligned_for(output) {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: output is aligned writable pointer-sized storage.
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
    let allocation = TypedAllocation::new(shape.layout, shape.trace);
    let pointer = allocation.pointer();
    // SAFETY: activation serializes heap access; publish only after ownership
    // and copied trace metadata have entered the heap.
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

pub(crate) unsafe fn allocate_typed_repeated_object(
    descriptor: *const LoomGcRepeatedObjectDescriptor,
    capacity: u64,
    output: *mut *mut c_void,
) -> i32 {
    if !is_aligned_for(output) {
        return GC_INVALID_ARGUMENT;
    }
    // SAFETY: output is aligned writable pointer-sized storage.
    unsafe { output.write(ptr::null_mut()) };
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    let shape = match unsafe { validate_repeated_object_descriptor(descriptor, capacity) } {
        Ok(shape) => shape,
        Err(status) => return status,
    };
    let status = managed_allocation_slowpath(runtime, shape.layout.size());
    if status != GC_OK {
        return status;
    }
    let allocation = TypedAllocation::new(shape.layout, shape.trace);
    let pointer = allocation.pointer();
    // SAFETY: activation serializes heap access; publish only after ownership
    // and copied trace metadata have entered the heap.
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

#[unsafe(export_name = "loom_gc_typed_repeated_alloc_v1")]
pub unsafe extern "C" fn typed_repeated_alloc_v1(
    descriptor: *const LoomGcRepeatedObjectDescriptor,
    capacity: u64,
    output: *mut *mut c_void,
) -> i32 {
    unsafe { allocate_typed_repeated_object(descriptor, capacity, output) }
}

#[unsafe(export_name = "loom_gc_typed_alloc_v1")]
pub unsafe extern "C" fn typed_alloc_v1(
    descriptor: *const LoomGcObjectDescriptor,
    allocation_size: u64,
    output: *mut *mut c_void,
) -> i32 {
    unsafe { allocate_typed_object(descriptor, allocation_size, output) }
}

struct HeapIndex {
    typed_objects: HashMap<usize, *const TypedAllocation>,
}

impl HeapIndex {
    fn new(heap: &LoomHeap) -> Self {
        Self {
            typed_objects: heap
                .typed_objects
                .iter()
                .map(|object| (object.address(), &raw const *object))
                .collect(),
        }
    }
}

struct TypedTraceContext {
    index: *const HeapIndex,
    marks: *mut HashSet<usize>,
    work: Vec<usize>,
}

unsafe extern "C" fn trace_typed_slot(slot: *mut *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    // SAFETY: collection owns the context for the complete visit.
    let context = unsafe { &mut *context.cast::<TypedTraceContext>() };
    let index = unsafe { &*context.index };
    let marks = unsafe { &mut *context.marks };
    // SAFETY: root validation established writable pointer-sized storage.
    let pointer = unsafe { slot.read() };
    trace_typed_pointer(pointer, index, marks, &mut context.work);
}

fn trace_typed_pointer(
    pointer: *mut c_void,
    index: &HeapIndex,
    marks: &mut HashSet<usize>,
    work: &mut Vec<usize>,
) {
    debug_assert!(work.is_empty());
    let address = pointer as usize;
    if pointer.is_null() || !index.typed_objects.contains_key(&address) || !marks.insert(address) {
        return;
    }
    work.push(address);
    while let Some(address) = work.pop() {
        let allocation_pointer = index
            .typed_objects
            .get(&address)
            .copied()
            .unwrap_or_else(|| unreachable!());
        // SAFETY: the heap is immutable throughout tracing.
        let allocation = unsafe { &*allocation_pointer };
        allocation.visit_pointer_offsets(|offset| {
            // SAFETY: copied metadata proved this aligned cell is in bounds.
            let child = unsafe { allocation.pointer_cell(offset).read() };
            let child_address = child as usize;
            if !child.is_null()
                && index.typed_objects.contains_key(&child_address)
                && marks.insert(child_address)
            {
                work.push(child_address);
            }
        });
    }
}

struct TypedRewriteContext<'moves> {
    typed_objects: &'moves HashMap<usize, *mut c_void>,
}

unsafe extern "C" fn rewrite_typed_slot(slot: *mut *mut c_void, context: *mut c_void) {
    if slot.is_null() || context.is_null() {
        return;
    }
    // SAFETY: relocation owns the context and validated slot for this visit.
    let context = unsafe { &*context.cast::<TypedRewriteContext<'_>>() };
    let address = unsafe { slot.read() } as usize;
    if let Some(pointer) = context.typed_objects.get(&address) {
        unsafe { slot.write(*pointer) };
    }
}

fn rewrite_typed_object(
    allocation: &mut TypedAllocation,
    typed_objects: &HashMap<usize, *mut c_void>,
) {
    allocation.visit_pointer_offsets(|offset| {
        // SAFETY: copied metadata proved this aligned cell is in bounds.
        let slot = unsafe { allocation.pointer_cell(offset) };
        let address = unsafe { slot.read() } as usize;
        if let Some(pointer) = typed_objects.get(&address) {
            unsafe { slot.write(*pointer) };
        }
    });
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
    // SAFETY: the attached runtime is stable and the scheduler owns both at
    // this safepoint.
    let runtime_ref = unsafe { &mut *runtime };
    let status = unsafe {
        collect_heap(
            &mut runtime_ref.heap,
            &mut executor.tasks,
            runtime_ref.typed_root_top,
            runtime_ref.typed_root_depth,
            force,
        )
    };
    if status != GC_OK {
        std::process::abort();
    }
}

#[unsafe(export_name = "loom_gc_safepoint_v1")]
pub unsafe extern "C" fn safepoint_v1() -> i32 {
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return GC_INVALID_ARGUMENT;
    }
    unsafe { collect_active_runtime(runtime, false) }
}

unsafe fn collect_active_runtime(runtime: *mut LoomRuntime, force: bool) -> i32 {
    // SAFETY: the caller owns the active generated-code interval.
    let runtime_ref = unsafe { &mut *runtime };
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
                typed_root_top,
                typed_root_depth,
                force,
            )
        };
    }
    // SAFETY: a runtime has at most one stable executor attachment.
    let executor_ref = unsafe { &mut *executor };
    if executor_ref.runtime_pointer() != runtime {
        return GC_INVALID_ARGUMENT;
    }
    unsafe {
        collect_heap(
            &mut runtime_ref.heap,
            &mut executor_ref.tasks,
            typed_root_top,
            typed_root_depth,
            force,
        )
    }
}

unsafe fn trace_scheduler_roots(tasks: &[Box<LoomTask>], context: &mut TypedTraceContext) {
    for task in tasks {
        let task = (&raw const **task).cast_mut();
        unsafe {
            trace_typed_task_roots(task, Some(trace_typed_slot), ptr::from_mut(context).cast());
        }
    }
}

unsafe fn collect_heap(
    heap: &mut LoomHeap,
    tasks: &mut [Box<LoomTask>],
    typed_root_top: *mut LoomGcTypedRootFrame,
    typed_root_depth: u64,
    force: bool,
) -> i32 {
    let root_status =
        unsafe { visit_typed_roots(typed_root_top, typed_root_depth, None, ptr::null_mut()) };
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
    if heap.typed_objects.is_empty() {
        heap.allocation_charge = 0;
        heap.next_gc_threshold = MIN_GC_THRESHOLD_BYTES;
        return GC_OK;
    }

    let index = HeapIndex::new(heap);
    let mut marks = HashSet::new();
    let mut trace_context = TypedTraceContext {
        index: &raw const index,
        marks: &raw mut marks,
        work: Vec::new(),
    };
    unsafe { trace_scheduler_roots(tasks, &mut trace_context) };
    let root_status = unsafe {
        visit_typed_roots(
            typed_root_top,
            typed_root_depth,
            Some(trace_typed_slot),
            (&raw mut trace_context).cast(),
        )
    };
    if root_status != GC_OK {
        return root_status;
    }

    let before = heap.typed_objects.len();
    heap.typed_objects
        .retain(|object| marks.contains(&object.address()));
    let after = heap.typed_objects.len();
    heap.reclaimed = heap
        .reclaimed
        .saturating_add((before.saturating_sub(after)) as u64);
    unsafe { relocate_marked_heap(heap, tasks, typed_root_top, typed_root_depth) }
}

fn evacuate_marked_heap(
    heap: &mut LoomHeap,
) -> (Vec<TypedAllocation>, HashMap<usize, *mut c_void>) {
    // Keep from-space alive until every reference has been rewritten.
    let from_space = std::mem::take(&mut heap.typed_objects);
    let mut replacements = Vec::with_capacity(from_space.len());
    let mut moves = HashMap::with_capacity(from_space.len());
    for object in &from_space {
        let replacement = object.evacuate();
        moves.insert(object.address(), replacement.pointer());
        replacements.push(replacement);
    }
    heap.typed_objects = replacements;
    debug_assert!(
        moves
            .values()
            .all(|pointer| !moves.contains_key(&(*pointer as usize)))
    );
    (from_space, moves)
}

unsafe fn relocate_marked_heap(
    heap: &mut LoomHeap,
    tasks: &mut [Box<LoomTask>],
    typed_root_top: *mut LoomGcTypedRootFrame,
    typed_root_depth: u64,
) -> i32 {
    let (from_space, moves) = evacuate_marked_heap(heap);
    heap.relocations = heap.relocations.saturating_add(moves.len() as u64);
    let mut rewrite_context = TypedRewriteContext {
        typed_objects: &moves,
    };
    for task in tasks.iter_mut() {
        unsafe {
            trace_typed_task_roots(
                &raw mut **task,
                Some(rewrite_typed_slot),
                (&raw mut rewrite_context).cast(),
            );
        }
    }
    let root_status = unsafe {
        visit_typed_roots(
            typed_root_top,
            typed_root_depth,
            Some(rewrite_typed_slot),
            (&raw mut rewrite_context).cast(),
        )
    };
    if root_status != GC_OK {
        return root_status;
    }
    for object in &mut heap.typed_objects {
        rewrite_typed_object(object, &moves);
    }
    drop(from_space);
    let live_bytes = heap
        .typed_objects
        .iter()
        .map(TypedAllocation::allocation_bytes)
        .fold(0_usize, usize::saturating_add);
    heap.allocation_charge = live_bytes;
    heap.next_gc_threshold = live_bytes.saturating_mul(2).max(MIN_GC_THRESHOLD_BYTES);
    GC_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};

    struct TestTypedRootFrame<const ROOTS: usize> {
        roots: Box<[*mut c_void; ROOTS]>,
        _slots: Box<[*mut c_void; ROOTS]>,
        _live_bitmaps: Box<[u64]>,
        descriptor: Box<LoomGcTypedRootDescriptor>,
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
                _slots: slots,
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

    #[repr(C)]
    struct TestRepeatedHeader {
        fixed_child: *mut c_void,
        length: u64,
        capacity: u64,
    }

    #[repr(C)]
    struct TestRepeatedElement {
        first: *mut c_void,
        marker: u64,
        second: *mut c_void,
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

    fn repeated_descriptor(offsets: &[u64]) -> LoomGcRepeatedObjectDescriptor {
        LoomGcRepeatedObjectDescriptor {
            abi_version: TYPED_GC_REPEATED_ABI_VERSION,
            flags: 0,
            fixed_size: 16,
            object_align: align_of::<*mut c_void>() as u64,
            fixed_pointer_count: 0,
            fixed_pointer_offsets: ptr::null(),
            element_stride: 16,
            element_pointer_count: offsets.len() as u64,
            element_pointer_offsets: offsets.as_ptr(),
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

    unsafe fn typed_repeated_allocate(
        descriptor: *const LoomGcRepeatedObjectDescriptor,
        capacity: usize,
    ) -> *mut c_void {
        let mut output = ptr::null_mut();
        assert_eq!(
            unsafe { typed_repeated_alloc_v1(descriptor, capacity as u64, &raw mut output) },
            GC_OK,
        );
        assert!(!output.is_null());
        output
    }

    unsafe fn repeated_element(object: *mut c_void, index: usize) -> *mut TestRepeatedElement {
        unsafe {
            object
                .cast::<u8>()
                .add(size_of::<TestRepeatedHeader>() + index * size_of::<TestRepeatedElement>())
                .cast()
        }
    }

    unsafe fn expect_typed_alloc_failure(
        descriptor: &LoomGcObjectDescriptor,
        allocation_size: u64,
        expected: i32,
    ) {
        let mut output = ptr::dangling_mut::<c_void>();
        assert_eq!(
            unsafe { typed_alloc_v1(descriptor, allocation_size, &raw mut output) },
            expected,
        );
        assert!(output.is_null());
    }

    unsafe fn expect_typed_repeated_alloc_failure(
        descriptor: &LoomGcRepeatedObjectDescriptor,
        capacity: u64,
        expected: i32,
    ) {
        let mut output = ptr::dangling_mut::<c_void>();
        assert_eq!(
            unsafe { typed_repeated_alloc_v1(descriptor, capacity, &raw mut output) },
            expected,
        );
        assert!(output.is_null());
    }

    unsafe fn force_next_safepoint(runtime: *mut LoomRuntime) {
        unsafe { (*runtime).heap.next_gc_threshold = 0 };
    }

    #[test]
    fn standalone_safepoint_moves_typed_graph_rewrites_aliases_and_reclaims() {
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
            (*original_child).marker = 41;
            (*original_child).checksum = 43;
            ptr::copy_nonoverlapping(
                TRAILING_BYTES.as_ptr(),
                original_child.cast::<u8>().add(size_of::<TestTypedLeaf>()),
                TRAILING_BYTES.len(),
            );
            frame.roots[0] = original_child.cast();

            // Allocation is itself a safepoint; generated code must reload the
            // direct child root before initializing the parent.
            (*runtime).heap.collect_before_every_allocation = true;
            let parent = typed_allocate(&raw const parent_descriptor, size_of::<TestTypedParent>())
                .cast::<TestTypedParent>();
            (*runtime).heap.collect_before_every_allocation = false;
            assert_ne!(frame.roots[0], original_child.cast());
            (*parent).child = frame.roots[0];
            (*parent).marker = 47;
            frame.roots[0] = parent.cast();
            frame.roots[1] = parent.cast();
            frame.roots[2] = (&raw const IMMORTAL_WORD).cast_mut().cast();

            // Descriptor metadata is runtime-owned after publication.
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
    fn repeated_typed_graph_rewrites_fixed_and_element_pointer_cells() {
        const CAPACITY: usize = 5;
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let mut frame = TestTypedRootFrame::<4>::all_live();
        let leaf_descriptor = typed_leaf_descriptor();
        let mut fixed_offsets = [0_u64];
        let mut element_offsets = [0_u64, 16_u64];
        let mut descriptor = LoomGcRepeatedObjectDescriptor {
            abi_version: TYPED_GC_REPEATED_ABI_VERSION,
            flags: 0,
            fixed_size: size_of::<TestRepeatedHeader>() as u64,
            object_align: align_of::<TestRepeatedElement>() as u64,
            fixed_pointer_count: fixed_offsets.len() as u64,
            fixed_pointer_offsets: fixed_offsets.as_ptr(),
            element_stride: size_of::<TestRepeatedElement>() as u64,
            element_pointer_count: element_offsets.len() as u64,
            element_pointer_offsets: element_offsets.as_ptr(),
        };
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);
            assert_eq!(typed_root_push_v1(frame.pointer()), GC_OK);
            for root in &mut frame.roots[1..] {
                *root = typed_allocate(&raw const leaf_descriptor, size_of::<TestTypedLeaf>());
            }
            for (index, root) in frame.roots[1..].iter().enumerate() {
                (*root.cast::<TestTypedLeaf>()).marker = 100 + index as u64;
            }

            (*runtime).heap.collect_before_every_allocation = true;
            let object = typed_repeated_allocate(&raw const descriptor, CAPACITY);
            (*runtime).heap.collect_before_every_allocation = false;
            frame.roots[0] = object;
            let header = object.cast::<TestRepeatedHeader>();
            (*header).fixed_child = frame.roots[1];
            (*header).length = 2;
            (*header).capacity = CAPACITY as u64;
            let first = repeated_element(object, 0);
            let second = repeated_element(object, 1);
            (*first).first = frame.roots[2];
            (*first).marker = 7;
            (*first).second = frame.roots[1];
            (*second).first = object;
            (*second).marker = 11;
            (*second).second = frame.roots[3];
            for index in 2..CAPACITY {
                let unused = repeated_element(object, index);
                assert!((*unused).first.is_null());
                assert_eq!((*unused).marker, 0);
                assert!((*unused).second.is_null());
            }

            // Fixed and repeated pointer tables are copied before publication.
            fixed_offsets[0] = 8;
            element_offsets.fill(8);
            descriptor.fixed_pointer_count = 0;
            descriptor.fixed_pointer_offsets = ptr::null();
            descriptor.element_pointer_count = 0;
            descriptor.element_pointer_offsets = ptr::null();
            assert_eq!(fixed_offsets, [8]);
            assert_eq!(element_offsets, [8, 8]);
            assert_eq!(descriptor.fixed_pointer_count, 0);
            assert!(descriptor.fixed_pointer_offsets.is_null());
            assert_eq!(descriptor.element_pointer_count, 0);
            assert!(descriptor.element_pointer_offsets.is_null());

            let old_object = object;
            let old_children = [frame.roots[1], frame.roots[2], frame.roots[3]];
            frame.roots[1..].fill(ptr::null_mut());
            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);

            let moved_object = frame.roots[0];
            assert_ne!(moved_object, old_object);
            let moved_header = moved_object.cast::<TestRepeatedHeader>();
            assert_eq!((*moved_header).length, 2);
            assert_eq!((*moved_header).capacity, CAPACITY as u64);
            assert_ne!((*moved_header).fixed_child, old_children[0]);
            let moved_first = repeated_element(moved_object, 0);
            let moved_second = repeated_element(moved_object, 1);
            assert_eq!((*moved_first).marker, 7);
            assert_eq!((*moved_second).marker, 11);
            assert_eq!((*moved_first).second, (*moved_header).fixed_child);
            assert_ne!((*moved_first).first, old_children[1]);
            assert_eq!((*moved_second).first, moved_object);
            assert_ne!((*moved_second).second, old_children[2]);
            assert_eq!((*runtime).heap.typed_object_count(), 4);
            for index in 2..CAPACITY {
                let unused = repeated_element(moved_object, index);
                assert!((*unused).first.is_null());
                assert_eq!((*unused).marker, 0);
                assert!((*unused).second.is_null());
            }

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
    fn typed_cycles_are_rewritten_and_reclaimed() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
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
            assert_eq!((*runtime).heap.reclaimed, 2);
            assert_eq!(typed_root_pop_v1(frame.pointer()), GC_OK);
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn typed_root_state_selects_only_live_cells() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
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
    fn typed_root_frames_are_traced_and_popped_in_strict_lifo_order() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let descriptor = typed_leaf_descriptor();
        let mut outer = TestTypedRootFrame::<1>::all_live();
        let mut inner = TestTypedRootFrame::<1>::all_live();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);

            let mut bad_descriptor_frame = TestTypedRootFrame::<1>::all_live();
            bad_descriptor_frame.descriptor.abi_version = TYPED_SHADOW_STACK_ABI_VERSION + 1;
            assert_eq!(
                typed_root_push_v1(bad_descriptor_frame.pointer()),
                GC_ABI_MISMATCH,
            );
            let mut bad_frame = TestTypedRootFrame::<1>::all_live();
            bad_frame.header.abi_version = TYPED_SHADOW_STACK_ABI_VERSION + 1;
            assert_eq!(typed_root_push_v1(bad_frame.pointer()), GC_ABI_MISMATCH);

            assert_eq!(typed_root_push_v1(outer.pointer()), GC_OK);
            outer.roots[0] = typed_allocate(&raw const descriptor, size_of::<TestTypedLeaf>());
            assert_eq!(typed_root_push_v1(inner.pointer()), GC_OK);
            inner.roots[0] = typed_allocate(&raw const descriptor, size_of::<TestTypedLeaf>());
            assert_eq!(typed_root_pop_v1(outer.pointer()), GC_FRAME_ORDER);
            assert_eq!(deactivate_runtime_v1(runtime), GC_ROOT_STACK_NOT_EMPTY);
            assert_eq!(runtime_destroy_v1(runtime), GC_INVALID_ARGUMENT);

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.typed_object_count(), 2);
            let outer_after_first = outer.roots[0];
            assert_eq!(typed_root_pop_v1(inner.pointer()), GC_OK);
            assert_eq!(inner.header.flags, 0);
            assert!(inner.header.previous.is_null());

            force_next_safepoint(runtime);
            assert_eq!(safepoint_v1(), GC_OK);
            assert_eq!((*runtime).heap.typed_object_count(), 1);
            assert_ne!(outer.roots[0], outer_after_first);

            assert_eq!(typed_root_pop_v1(outer.pointer()), GC_OK);
            assert_eq!(outer.header.flags, 0);
            assert!(outer.header.previous.is_null());
            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn typed_object_descriptor_validation_is_fail_closed_and_bounded() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let base = typed_leaf_descriptor();
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);

            expect_typed_alloc_failure(
                &LoomGcObjectDescriptor {
                    abi_version: TYPED_GC_ABI_VERSION + 1,
                    ..base
                },
                base.fixed_size,
                GC_ABI_MISMATCH,
            );
            for malformed in [
                LoomGcObjectDescriptor { flags: 1, ..base },
                LoomGcObjectDescriptor {
                    fixed_size: 0,
                    ..base
                },
                LoomGcObjectDescriptor {
                    object_align: 3,
                    ..base
                },
            ] {
                expect_typed_alloc_failure(
                    &malformed,
                    malformed.fixed_size.max(1),
                    GC_DESCRIPTOR_INVALID,
                );
            }
            expect_typed_alloc_failure(&base, base.fixed_size - 1, GC_DESCRIPTOR_INVALID);

            let unaligned_offsets = [1_u64];
            let descending_offsets = [8_u64, 0];
            let outside_offsets = [16_u64];
            for (offsets, count) in [
                (unaligned_offsets.as_slice(), 1_u64),
                (descending_offsets.as_slice(), 2_u64),
                (outside_offsets.as_slice(), 1_u64),
            ] {
                expect_typed_alloc_failure(
                    &LoomGcObjectDescriptor {
                        pointer_count: count,
                        pointer_offsets: offsets.as_ptr(),
                        ..base
                    },
                    base.fixed_size,
                    GC_DESCRIPTOR_INVALID,
                );
            }

            expect_typed_alloc_failure(
                &LoomGcObjectDescriptor {
                    pointer_count: GC_MAX_OBJECT_POINTERS + 1,
                    ..base
                },
                base.fixed_size,
                GC_RESOURCE_LIMIT,
            );
            expect_typed_alloc_failure(
                &LoomGcObjectDescriptor {
                    object_align: GC_MAX_OBJECT_ALIGNMENT * 2,
                    ..base
                },
                base.fixed_size,
                GC_RESOURCE_LIMIT,
            );
            expect_typed_alloc_failure(&base, GC_MAX_OBJECT_BYTES + 1, GC_RESOURCE_LIMIT);
            assert_eq!((*runtime).heap.typed_object_count(), 0);
            assert_eq!((*runtime).heap.collections, 0);

            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            expect_typed_alloc_failure(&base, base.fixed_size, GC_INVALID_ARGUMENT);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }

    #[test]
    fn repeated_typed_descriptor_validation_is_fail_closed_and_bounded() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let offsets = [0_u64];
        let base = repeated_descriptor(&offsets);
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), GC_OK);

            expect_typed_repeated_alloc_failure(
                &LoomGcRepeatedObjectDescriptor {
                    abi_version: TYPED_GC_REPEATED_ABI_VERSION + 1,
                    ..base
                },
                1,
                GC_ABI_MISMATCH,
            );
            for malformed in [
                LoomGcRepeatedObjectDescriptor { flags: 1, ..base },
                LoomGcRepeatedObjectDescriptor {
                    fixed_size: 0,
                    ..base
                },
                LoomGcRepeatedObjectDescriptor {
                    object_align: 3,
                    ..base
                },
                LoomGcRepeatedObjectDescriptor {
                    element_stride: 0,
                    ..base
                },
                LoomGcRepeatedObjectDescriptor {
                    element_pointer_count: 0,
                    ..base
                },
            ] {
                expect_typed_repeated_alloc_failure(&malformed, 1, GC_DESCRIPTOR_INVALID);
            }

            let unaligned = [1_u64];
            let outside = [16_u64];
            let descending = [8_u64, 0_u64];
            for (table, count) in [
                (unaligned.as_slice(), 1_u64),
                (outside.as_slice(), 1_u64),
                (descending.as_slice(), 2_u64),
            ] {
                expect_typed_repeated_alloc_failure(
                    &LoomGcRepeatedObjectDescriptor {
                        element_pointer_count: count,
                        element_pointer_offsets: table.as_ptr(),
                        ..base
                    },
                    1,
                    GC_DESCRIPTOR_INVALID,
                );
            }

            expect_typed_repeated_alloc_failure(
                &LoomGcRepeatedObjectDescriptor {
                    element_pointer_count: GC_MAX_OBJECT_POINTERS + 1,
                    ..base
                },
                1,
                GC_RESOURCE_LIMIT,
            );
            expect_typed_repeated_alloc_failure(
                &base,
                GC_MAX_REPEATED_POINTER_CELLS + 1,
                GC_RESOURCE_LIMIT,
            );
            expect_typed_repeated_alloc_failure(
                &LoomGcRepeatedObjectDescriptor {
                    element_stride: GC_MAX_OBJECT_BYTES,
                    element_pointer_count: 0,
                    element_pointer_offsets: ptr::null(),
                    ..base
                },
                2,
                GC_RESOURCE_LIMIT,
            );
            assert_eq!((*runtime).heap.typed_object_count(), 0);
            assert_eq!((*runtime).heap.collections, 0);

            assert_eq!(deactivate_runtime_v1(runtime), GC_OK);
            expect_typed_repeated_alloc_failure(&base, 1, GC_INVALID_ARGUMENT);
            assert_eq!(runtime_destroy_v1(runtime), GC_OK);
        }
    }
}
