use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::{align_of, size_of};
use std::net::{TcpStream, ToSocketAddrs};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use loom_runtime_abi::{
    FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX, GC_MAX_OBJECT_ALIGNMENT,
    GC_MAX_OBJECT_BYTES, GC_MAX_ROOT_BITMAP_WORDS, GC_MAX_ROOT_SLOTS, GC_MAX_ROOT_STATES, GC_OK,
    LoomByteView, LoomTypedCoroutineDescriptor, LoomTypedTaskCallback, LoomTypedTaskFaultView,
    LoomWitnessInstance, TYPED_RESOURCE_CLOSE_FAILED, TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT,
    TYPED_RESOURCE_CLOSE_OK, TYPED_RESOURCE_KIND_FILE, TYPED_RESOURCE_KIND_SOCKET,
    TYPED_TASK_ABI_VERSION, TYPED_TASK_CLEANUP_FAULTED, TYPED_TASK_INVALID_ARGUMENT,
    TYPED_TASK_MAX_FAULT_TEXT_BYTES, TYPED_TASK_NO_MEMORY, TYPED_TASK_OK,
    TYPED_TASK_STATUS_INVALID, VALUE_SLOT_WORDS, VALUE_TAG_ENUM, VALUE_TAG_LIST, VALUE_TAG_RECORD,
    VALUE_TAG_TASK, VALUE_TAG_TUPLE,
};

use crate::gc::{
    NodeStream, RecoverableExecutorActivation, RuntimeRootScope, active_runtime_pointer,
    enter_executor, leave_executor, poll,
};
use crate::platform::{
    INVALID_HANDLE, OwnedResource, close_untracked, duplicate_file, duplicate_socket,
    socket_handle_bits,
};
use crate::reactor::{
    LoomExecutor, LoomReadyNotification, LoomRegistration, LoomWaitSource, cancel_for_task,
    has_registrations, pop_for_scheduler, register_for_task, wait_for_scheduler,
};
use crate::runtime::LoomRuntime;
use crate::witness::{WitnessArena, clone_witnesses};
use crate::{
    COROUTINE_ABI_VERSION, TASK_CANCELLED, TASK_COMPLETED, TASK_FAULTED, TASK_JOIN_ALL,
    TASK_JOIN_ANY, TASK_JOIN_RACE, TASK_JOIN_SETTLED, TASK_PENDING, WAIT_ABI_VERSION,
    WAIT_INVALID_ARGUMENT, WAIT_NO_MEMORY, WAIT_OK, WAIT_SOURCE_IO, WAIT_SOURCE_TIMER,
    WAIT_UNSUPPORTED,
};

const NO_JOIN_WINNER: u64 = u64::MAX;
const TASK_VALUE_DIRECT: u64 = 0;

// Compiler-private synthetic prelude ids. Keep these synchronized with
// loom-lowering until the native value ABI becomes self-describing.
const TASK_FAULT_TYPE: u64 = 6;
const TASK_OUTCOME_TYPE: u64 = 7;
const TASK_OUTCOME_COMPLETED: u64 = 0;
const TASK_OUTCOME_FAULTED: u64 = 1;
const TASK_OUTCOME_CANCELLED: u64 = 2;
const TASK_FAULT_CODE: &str = "TaskFault";
const TASK_FAULT_MESSAGE: &str = "task execution failed";
const FILE_TYPE: u64 = 9;
const SOCKET_TYPE: u64 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IoResourceKind {
    File,
    Socket,
}

impl IoResourceKind {
    const fn from_nominal(nominal: u64) -> Option<Self> {
        match nominal {
            FILE_TYPE => Some(Self::File),
            SOCKET_TYPE => Some(Self::Socket),
            _ => None,
        }
    }

    const fn is_file(self) -> bool {
        matches!(self, Self::File)
    }

    const fn from_typed_kind(kind: u32) -> Option<Self> {
        match kind {
            TYPED_RESOURCE_KIND_FILE => Some(Self::File),
            TYPED_RESOURCE_KIND_SOCKET => Some(Self::Socket),
            _ => None,
        }
    }
}
const RESULT_TYPE: u64 = 1;
const IO_ERROR_TYPE: u64 = 18;
const IO_ERROR_KIND_TYPE: u64 = 19;

const JOIN_RESULT_TUPLE: u32 = 1;
const JOIN_RESULT_LIST: u32 = 2;
const JOIN_RESULT_OUTCOME: u32 = 3;
const JOIN_RESULT_OUTCOME_TUPLE: u32 = 4;
const JOIN_RESULT_OUTCOME_LIST: u32 = 5;

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            control if control <= '\u{1f}' => {
                write!(output, "\\u{:04x}", u32::from(control))
                    .expect("writing into String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn default_failure_json(code: &str, message: &str) -> String {
    let is_defect = code.starts_with("LOOM_RUNTIME_");
    let code = json_string(code);
    let message = json_string(message);
    let span = r#"{"file":0,"range":{"start":0,"end":0}}"#;
    if is_defect {
        format!(
            r#"{{"channel":"defect","defect":{{"code":"InterpreterDefect","message":{message},"span":{span}}}}}"#
        )
    } else {
        format!(
            r#"{{"channel":"runtime","fault":{{"code":{code},"message":{message},"span":{span}}}}}"#
        )
    }
}

fn machine_faults_requested() -> bool {
    std::env::var(FAULT_FORMAT_ENV).is_ok_and(|value| value == FAULT_FORMAT_JSON)
}

fn report_fault(detail: &str, code: &str, message: &str, human: &str) {
    if machine_faults_requested() {
        let detail = if detail.is_empty() {
            default_failure_json(code, message)
        } else {
            detail.to_owned()
        };
        eprintln!("{FAULT_JSON_PREFIX}{detail}");
    } else {
        println!("{human}");
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct ValueSlot {
    pub(crate) words: [u64; VALUE_SLOT_WORDS],
}

#[repr(C)]
pub(crate) struct ValueNode {
    pub(crate) value: ValueSlot,
    pub(crate) next: *mut ValueNode,
}

pub type LoomTaskResume = unsafe extern "C" fn(*mut LoomTask, *mut LoomExecutor) -> i32;
pub type LoomTaskCancel = unsafe extern "C" fn(*mut LoomTask, *mut LoomExecutor) -> i32;
pub type LoomTraceVisitor = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type LoomTaskTrace = unsafe extern "C" fn(*mut LoomTask, Option<LoomTraceVisitor>, *mut c_void);
pub(crate) type LoomTypedTraceVisitor = unsafe extern "C" fn(*mut *mut c_void, *mut c_void);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoomCoroutineDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub resume: Option<LoomTaskResume>,
    pub cancel: Option<LoomTaskCancel>,
    pub trace: Option<LoomTaskTrace>,
    pub slot_count: u64,
    pub witness_count: u64,
    pub result_slot: u64,
    pub state_count: u64,
    pub live_bitmap_words: u64,
    pub live_bitmaps: *const u64,
}

/// Owned, validated metadata and stable storage for one typed coroutine.
///
/// The source descriptor's variable-length arrays are copied during Task
/// creation. No pointer into an untrusted or short-lived descriptor is kept.
struct TypedTaskStorage {
    resume: LoomTypedTaskCallback,
    cancel: LoomTypedTaskCallback,
    dispose_result: Option<LoomTypedTaskCallback>,
    frame: NonNull<u8>,
    frame_layout: Layout,
    result_offset: usize,
    result_size: usize,
    result_align: usize,
    root_offsets: Box<[usize]>,
    root_state_count: usize,
    root_bitmap_words: usize,
    live_bitmaps: Box<[u64]>,
    completed_root_state: usize,
    root_state: usize,
    initialized: bool,
    published: bool,
    result_initialized: bool,
    result_taken: bool,
    result_disposed: bool,
    cancel_invoked: bool,
    dispose_invoked: bool,
    join_completion_pending: bool,
    join_cancel_authorized: bool,
    join_winner_finalized: bool,
}

impl TypedTaskStorage {
    fn frame_pointer(&self) -> *mut c_void {
        self.frame.as_ptr().cast()
    }

    fn result_pointer(&self) -> *mut u8 {
        // SAFETY: descriptor validation proved the complete result range lies
        // in the stable frame allocation.
        unsafe { self.frame.as_ptr().add(self.result_offset) }
    }

    fn root_is_live(&self, state: usize, index: usize) -> bool {
        let word = self.live_bitmaps[state * self.root_bitmap_words + index / 64];
        word & (1_u64 << (index % 64)) != 0
    }

    #[allow(clippy::cast_ptr_alignment)]
    unsafe fn root_cell(&self, offset: usize) -> *mut *mut c_void {
        // SAFETY: descriptor validation proved both the frame base alignment
        // and every copied root offset before this storage was constructed.
        unsafe { self.frame.as_ptr().add(offset).cast() }
    }
}

impl Drop for TypedTaskStorage {
    fn drop(&mut self) {
        // SAFETY: `frame` came from alloc_zeroed with this exact Layout and is
        // owned solely by this storage object.
        unsafe { dealloc(self.frame.as_ptr(), self.frame_layout) };
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TaskStatus {
    Unpublished,
    Runnable,
    Running,
    Waiting,
    Draining,
    Completed,
    Faulted,
    Cancelled,
}

enum IoOperation {
    FileOpen {
        path: String,
        create: bool,
    },
    FileRead {
        file: File,
    },
    FileWrite {
        file: File,
        bytes: Vec<u8>,
    },
    SocketConnect {
        host: String,
        port: u16,
    },
    SocketRead {
        socket: TcpStream,
        bytes: Vec<u8>,
    },
    SocketWrite {
        socket: TcpStream,
        bytes: Vec<u8>,
        offset: usize,
    },
}

pub(crate) enum BlockingResult {
    Resource {
        nominal: u64,
        resource: OwnedResource,
    },
    Text {
        bytes: Vec<u8>,
        code: &'static str,
    },
    Unit,
    Fault {
        code: &'static str,
        kind: u32,
        message: String,
    },
}

pub(crate) struct WorkerCompletion {
    pub(crate) task: usize,
    pub(crate) registration: LoomRegistration,
    pub(crate) result: BlockingResult,
}

type BlockingJob = Box<dyn FnOnce() + Send + 'static>;

fn blocking_pool() -> &'static mpsc::SyncSender<BlockingJob> {
    static POOL: OnceLock<mpsc::SyncSender<BlockingJob>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<BlockingJob>(256);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..4 {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("loom-blocking-{index}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let Ok(receiver) = receiver.lock() else {
                                return;
                            };
                            receiver.recv()
                        };
                        let Ok(job) = job else {
                            return;
                        };
                        job();
                    }
                })
                .expect("create bounded Loom blocking worker");
        }
        sender
    })
}

pub struct LoomTask {
    descriptor: LoomCoroutineDescriptor,
    pub(crate) slots: Box<[ValueSlot]>,
    result_slot: usize,
    state: u64,
    executor: *mut LoomExecutor,
    owner: *mut LoomTask,
    owned_children: Vec<*mut LoomTask>,
    join_children: Vec<*mut LoomTask>,
    waits: Vec<LoomRegistration>,
    status: TaskStatus,
    deferred_terminal: i32,
    join_mode: u32,
    join_winner: u64,
    join_step: i32,
    queued: bool,
    cancel_requested: bool,
    join_active: bool,
    wait_leaf: bool,
    wait_source: LoomWaitSource,
    composite_spec: *mut LoomJoinSpec,
    io_operation: Option<IoOperation>,
    blocking_result: Option<BlockingResult>,
    io_fallible: bool,
    owned_result_resources: Vec<OwnedResource>,
    primary_fault_recorded: bool,
    fault_code: String,
    fault_message: String,
    fault_detail: String,
    witness_slots: Box<[*const LoomWitnessInstance]>,
    witness_arena: WitnessArena,
    witnesses_captured: bool,
    typed: Option<TypedTaskStorage>,
    pending_owner: *mut LoomTask,
}

pub struct LoomJoinSpec {
    executor: *mut LoomExecutor,
    owner: *mut LoomTask,
    task: *mut LoomTask,
    mode: u32,
    shape: u32,
    tasks: Vec<*mut LoomTask>,
}

fn terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Faulted | TaskStatus::Cancelled
    )
}

fn task_slot_is_live(task: &LoomTask, index: usize) -> bool {
    match task.status {
        TaskStatus::Completed => return index == task.result_slot,
        TaskStatus::Unpublished | TaskStatus::Faulted | TaskStatus::Cancelled => return false,
        TaskStatus::Runnable | TaskStatus::Running | TaskStatus::Waiting | TaskStatus::Draining => {
        }
    }
    let descriptor = task.descriptor;
    if descriptor.live_bitmaps.is_null() {
        return true;
    }
    let Ok(state) = usize::try_from(task.state) else {
        return true;
    };
    let Ok(state_count) = usize::try_from(descriptor.state_count) else {
        return true;
    };
    let Ok(words) = usize::try_from(descriptor.live_bitmap_words) else {
        return true;
    };
    if state >= state_count {
        return true;
    }
    let Some(offset) = state
        .checked_mul(words)
        .and_then(|row| row.checked_add(index / 64))
    else {
        return true;
    };
    let word = unsafe { *descriptor.live_bitmaps.add(offset) };
    word & (1_u64 << (index % 64)) != 0
}

#[unsafe(export_name = "loom_task_trace_live_slots")]
pub unsafe extern "C" fn task_trace_live_slots(
    task: *mut LoomTask,
    visitor: Option<LoomTraceVisitor>,
    context: *mut c_void,
) {
    let (Some(task), Some(visitor)) = (unsafe { task.as_mut() }, visitor) else {
        return;
    };
    for index in 0..task.slots.len() {
        if task_slot_is_live(task, index) {
            unsafe { visitor((&raw mut task.slots[index]).cast(), context) };
        }
    }
}

pub(crate) unsafe fn trace_task_roots(
    task: *mut LoomTask,
    visitor: Option<LoomTraceVisitor>,
    context: *mut c_void,
) {
    if task.is_null() || unsafe { (*task).typed.is_some() } {
        return;
    }
    if let Some(trace) = unsafe { (*task).descriptor.trace } {
        unsafe { trace(task, visitor, context) };
    }
}

/// Visits the exact direct-pointer root cells in a typed coroutine frame.
///
/// Runnable, running, suspended, and draining tasks use their published MIR
/// state row. A completed task uses the independently validated result row;
/// faulted, cancelled, consumed, and disposed results retain no roots.
pub(crate) unsafe fn trace_typed_task_roots(
    task: *mut LoomTask,
    visitor: Option<LoomTypedTraceVisitor>,
    context: *mut c_void,
) {
    let (Some(task), Some(visitor)) = (unsafe { task.as_mut() }, visitor) else {
        return;
    };
    let Some(typed) = task.typed.as_ref() else {
        return;
    };
    if !typed.initialized {
        return;
    }
    let state = match task.status {
        TaskStatus::Unpublished
        | TaskStatus::Runnable
        | TaskStatus::Running
        | TaskStatus::Waiting
        | TaskStatus::Draining => typed.root_state,
        TaskStatus::Completed
            if typed.result_initialized && !typed.result_taken && !typed.result_disposed =>
        {
            typed.completed_root_state
        }
        TaskStatus::Completed | TaskStatus::Faulted | TaskStatus::Cancelled => return,
    };
    for (index, offset) in typed.root_offsets.iter().copied().enumerate() {
        if typed.root_is_live(state, index) {
            // SAFETY: copied descriptor validation proved every live root is
            // an aligned pointer-sized cell fully inside this stable frame.
            let slot = unsafe { typed.root_cell(offset) };
            unsafe { visitor(slot, context) };
        }
    }
}

fn terminal_step(task: *const LoomTask) -> i32 {
    if task.is_null() {
        return TASK_FAULTED;
    }
    // SAFETY: callers pass pointers owned by their live executor.
    match unsafe { (*task).status } {
        TaskStatus::Completed => TASK_COMPLETED,
        TaskStatus::Cancelled => TASK_CANCELLED,
        _ => TASK_FAULTED,
    }
}

fn executor_owns(executor: &LoomExecutor, task: *const LoomTask) -> bool {
    !task.is_null()
        && executor
            .tasks
            .iter()
            .any(|candidate| ptr::eq::<LoomTask>(&raw const **candidate, task))
}

unsafe fn enqueue_task(executor: &mut LoomExecutor, task: *mut LoomTask) {
    if !executor_owns(executor, task) {
        return;
    }
    // SAFETY: ownership was established immediately above.
    let task_ref = unsafe { &mut *task };
    if task_ref.queued || task_ref.status != TaskStatus::Runnable {
        return;
    }
    task_ref.queued = true;
    executor.runnable.push_back(task);
}

fn all_terminal(tasks: &[*mut LoomTask]) -> bool {
    tasks.iter().all(|task| {
        !task.is_null() && {
            // SAFETY: join/ownership lists contain live executor-owned tasks.
            terminal(unsafe { (**task).status })
        }
    })
}

unsafe fn make_join_runnable(executor: &mut LoomExecutor, parent: *mut LoomTask) {
    // SAFETY: caller established that parent belongs to executor.
    unsafe {
        if let Some(typed) = (*parent).typed.as_mut() {
            typed.join_completion_pending = true;
            typed.join_cancel_authorized = false;
        }
        (*parent).join_active = false;
        (*parent).status = TaskStatus::Runnable;
        enqueue_task(executor, parent);
    }
}

unsafe fn request_cancel(executor: &mut LoomExecutor, task: *mut LoomTask) {
    if !executor_owns(executor, task) {
        return;
    }
    // SAFETY: task is owned by executor; copies release the borrow before
    // recursive scheduler operations.
    if terminal(unsafe { (*task).status })
        || unsafe { (*task).status } == TaskStatus::Unpublished
        || unsafe { (*task).cancel_requested }
    {
        return;
    }
    unsafe { (*task).cancel_requested = true };
    let registrations = unsafe { std::mem::take(&mut (*task).waits) };
    for registration in registrations {
        // SAFETY: each registration was created by this executor for task.
        let _ = unsafe { cancel_for_task(executor, &raw const registration) };
    }
    if unsafe { (*task).status } != TaskStatus::Running {
        // Cancellation is strict reverse creation order. Reinsert the owner at
        // the front first, then visit children oldest-to-newest; every newer
        // child (and its descendants) is placed ahead of older work.
        executor.runnable.retain(|candidate| *candidate != task);
        unsafe {
            (*task).queued = false;
            (*task).status = TaskStatus::Runnable;
            (*task).queued = true;
        }
        executor.runnable.push_front(task);
    }
    let children = unsafe { (*task).owned_children.clone() };
    for child in children {
        // SAFETY: structured child pointers remain live until executor drop.
        unsafe { request_cancel(executor, child) };
    }
}

#[allow(clippy::too_many_lines)]
unsafe fn update_join(executor: &mut LoomExecutor, parent: *mut LoomTask) {
    if !executor_owns(executor, parent)
        || !unsafe { (*parent).join_active }
        || unsafe { (*parent).status } != TaskStatus::Waiting
    {
        return;
    }
    let children = unsafe { (*parent).join_children.clone() };
    let finished = all_terminal(&children);
    match unsafe { (*parent).join_mode } {
        TASK_JOIN_ALL => {
            let failure = children.iter().copied().find(|child| {
                terminal(unsafe { (**child).status })
                    && unsafe { (**child).status != TaskStatus::Completed }
            });
            if let Some(failure) = failure {
                unsafe {
                    inherit_primary_task_fault(&mut *parent, &*failure);
                    (*parent).join_step = terminal_step(failure);
                }
                for child in &children {
                    if !terminal(unsafe { (**child).status }) {
                        unsafe { request_cancel(executor, *child) };
                    }
                }
                if all_terminal(&children) {
                    unsafe { make_join_runnable(executor, parent) };
                }
            } else if finished {
                unsafe {
                    (*parent).join_step = TASK_COMPLETED;
                    make_join_runnable(executor, parent);
                }
            }
        }
        TASK_JOIN_SETTLED => {
            if finished {
                unsafe {
                    (*parent).join_step = TASK_COMPLETED;
                    make_join_runnable(executor, parent);
                }
            }
        }
        TASK_JOIN_ANY => {
            if unsafe { (*parent).join_winner } == NO_JOIN_WINNER
                && let Some(index) = children
                    .iter()
                    .position(|child| unsafe { (**child).status } == TaskStatus::Completed)
            {
                let Ok(index) = u64::try_from(index) else {
                    unsafe {
                        (*parent).join_step = TASK_FAULTED;
                        make_join_runnable(executor, parent);
                    }
                    return;
                };
                unsafe {
                    (*parent).join_winner = index;
                    (*parent).join_step = TASK_COMPLETED;
                }
            }
            if unsafe { (*parent).join_winner } != NO_JOIN_WINNER {
                let Ok(winner) = usize::try_from(unsafe { (*parent).join_winner }) else {
                    unsafe {
                        (*parent).join_step = TASK_FAULTED;
                        make_join_runnable(executor, parent);
                    }
                    return;
                };
                for (index, child) in children.iter().enumerate() {
                    if index != winner && !terminal(unsafe { (**child).status }) {
                        unsafe { request_cancel(executor, *child) };
                    }
                }
                if all_terminal(&children) {
                    unsafe { make_join_runnable(executor, parent) };
                }
            } else if finished {
                unsafe {
                    (*parent).join_step = TASK_FAULTED;
                    make_join_runnable(executor, parent);
                }
            }
        }
        TASK_JOIN_RACE => {
            if unsafe { (*parent).join_winner } == NO_JOIN_WINNER
                && let Some(index) = children
                    .iter()
                    .position(|child| terminal(unsafe { (**child).status }))
            {
                let Ok(index) = u64::try_from(index) else {
                    unsafe {
                        (*parent).join_step = TASK_FAULTED;
                        make_join_runnable(executor, parent);
                    }
                    return;
                };
                unsafe {
                    (*parent).join_winner = index;
                    (*parent).join_step = TASK_COMPLETED;
                }
            }
            if unsafe { (*parent).join_winner } != NO_JOIN_WINNER {
                let Ok(winner) = usize::try_from(unsafe { (*parent).join_winner }) else {
                    unsafe {
                        (*parent).join_step = TASK_FAULTED;
                        make_join_runnable(executor, parent);
                    }
                    return;
                };
                for (index, child) in children.iter().enumerate() {
                    if index != winner && !terminal(unsafe { (**child).status }) {
                        unsafe { request_cancel(executor, *child) };
                    }
                }
                if all_terminal(&children) {
                    unsafe { make_join_runnable(executor, parent) };
                }
            }
        }
        _ => unsafe {
            (*parent).join_step = TASK_FAULTED;
            make_join_runnable(executor, parent);
        },
    }
}

unsafe fn child_became_terminal(executor: &mut LoomExecutor, child: *mut LoomTask) {
    let owner = unsafe { (*child).owner };
    if !executor_owns(executor, owner) {
        return;
    }
    if unsafe { (*owner).status } == TaskStatus::Waiting && unsafe { (*owner).join_active } {
        unsafe { update_join(executor, owner) };
    } else if unsafe { (*owner).status } == TaskStatus::Draining {
        let children = unsafe { (*owner).owned_children.clone() };
        if all_terminal(&children) {
            let typed_cancel_pending = unsafe {
                (*owner)
                    .typed
                    .as_ref()
                    .is_some_and(|typed| (*owner).cancel_requested && !typed.cancel_invoked)
            };
            if typed_cancel_pending {
                unsafe {
                    (*owner).status = TaskStatus::Runnable;
                    enqueue_task(executor, owner);
                }
            } else {
                let deferred = unsafe { (*owner).deferred_terminal };
                unsafe {
                    (*owner).deferred_terminal = TASK_PENDING;
                    complete_terminal(executor, owner, deferred);
                }
            }
        }
    }
}

unsafe fn record_typed_runtime_defect(task: *mut LoomTask, code: &str, message: &str) {
    if !task.is_null() {
        unsafe {
            record_primary_task_fault(
                &mut *task,
                code.to_owned(),
                message.to_owned(),
                String::new(),
            );
        }
    }
}

unsafe fn suppress_new_typed_cleanup_fault(task: *mut LoomTask, fault_before: bool) {
    if task.is_null() || fault_before || !unsafe { (*task).primary_fault_recorded } {
        return;
    }
    unsafe {
        (*task).primary_fault_recorded = false;
        (*task).fault_code.clear();
        (*task).fault_message.clear();
        (*task).fault_detail.clear();
    }
}

/// Marks one nested non-suspending cleanup activation. The raw pointer keeps
/// the guard from borrowing the executor across a generated callback, while
/// Drop guarantees depth restoration on every Rust return path.
struct CleanupPhaseGuard {
    executor: NonNull<LoomExecutor>,
}

impl CleanupPhaseGuard {
    fn enter(executor: &mut LoomExecutor) -> Option<Self> {
        if !executor.enter_cleanup() {
            return None;
        }
        Some(Self {
            executor: NonNull::from(executor),
        })
    }
}

impl Drop for CleanupPhaseGuard {
    fn drop(&mut self) {
        // SAFETY: the guard never outlives the callback site, which retains
        // ownership of this live executor for the complete activation.
        unsafe { self.executor.as_mut().leave_cleanup() };
    }
}

/// Scheduler topology which a cleanup callback is forbidden to change.
/// Public mutation APIs fail while `cleanup_depth` is nonzero; restoring this
/// snapshot is a final defense before the callback's frame may be reclaimed.
struct CleanupTopologySnapshot {
    status: TaskStatus,
    owner: *mut LoomTask,
    pending_owner: *mut LoomTask,
    waits: Vec<LoomRegistration>,
    owned_children: Vec<*mut LoomTask>,
    join_children: Vec<*mut LoomTask>,
    join_active: bool,
    queued: bool,
    cancel_requested: bool,
    runnable: Vec<*mut LoomTask>,
    retired_tasks: Vec<*mut LoomTask>,
    tasks: Vec<*mut LoomTask>,
    join_specs: Vec<*mut LoomJoinSpec>,
}

impl CleanupTopologySnapshot {
    unsafe fn capture(executor: &LoomExecutor, task: *const LoomTask) -> Self {
        let task = unsafe { &*task };
        Self {
            status: task.status,
            owner: task.owner,
            pending_owner: task.pending_owner,
            waits: task.waits.clone(),
            owned_children: task.owned_children.clone(),
            join_children: task.join_children.clone(),
            join_active: task.join_active,
            queued: task.queued,
            cancel_requested: task.cancel_requested,
            runnable: executor.runnable.iter().copied().collect(),
            retired_tasks: executor.retired_tasks.clone(),
            tasks: executor
                .tasks
                .iter()
                .map(|candidate| (&raw const **candidate).cast_mut())
                .collect(),
            join_specs: executor
                .join_specs
                .iter()
                .map(|join| (&raw const **join).cast_mut())
                .collect(),
        }
    }

    unsafe fn validate_and_restore(self, executor: &mut LoomExecutor, task: *mut LoomTask) -> bool {
        let current_tasks = executor
            .tasks
            .iter()
            .map(|candidate| (&raw const **candidate).cast_mut())
            .collect::<Vec<_>>();
        let current_join_specs = executor
            .join_specs
            .iter()
            .map(|join| (&raw const **join).cast_mut())
            .collect::<Vec<_>>();
        let task_ref = unsafe { &mut *task };
        let intact = task_ref.status == self.status
            && task_ref.owner == self.owner
            && task_ref.pending_owner == self.pending_owner
            && task_ref.waits == self.waits
            && task_ref.owned_children == self.owned_children
            && task_ref.join_children == self.join_children
            && task_ref.join_active == self.join_active
            && task_ref.queued == self.queued
            && task_ref.cancel_requested == self.cancel_requested
            && executor
                .runnable
                .iter()
                .copied()
                .eq(self.runnable.iter().copied())
            && executor.retired_tasks == self.retired_tasks
            && current_tasks == self.tasks
            && current_join_specs == self.join_specs
            && executor.active_task == task;

        // Any registration appended by a malicious callback is cancelled
        // after leaving cleanup phase, before the frame can be reclaimed.
        for registration in task_ref
            .waits
            .iter()
            .copied()
            .filter(|registration| !self.waits.contains(registration))
        {
            let _ = unsafe { cancel_for_task(executor, &raw const registration) };
        }
        task_ref.status = self.status;
        task_ref.owner = self.owner;
        task_ref.pending_owner = self.pending_owner;
        task_ref.waits = self.waits;
        task_ref.owned_children = self.owned_children;
        task_ref.join_children = self.join_children;
        task_ref.join_active = self.join_active;
        task_ref.queued = self.queued;
        task_ref.cancel_requested = self.cancel_requested;
        executor.runnable = self.runnable.into();
        executor.retired_tasks = self.retired_tasks;
        intact
    }
}

#[derive(Clone, Copy)]
struct CleanupCallbackInvocation {
    step: i32,
    topology_intact: bool,
    activation_intact: bool,
    cleanup_phase_entered: bool,
}

impl CleanupCallbackInvocation {
    const fn protocol_intact(self) -> bool {
        self.topology_intact && self.activation_intact && self.cleanup_phase_entered
    }
}

/// Invokes one non-suspending typed cleanup callback behind recoverable
/// scheduler-topology and runtime-activation boundaries. A callback may return
/// malformed state, but it cannot strand a root chain or nested activation in
/// the scheduler thread before the defect is converted into a Task fault.
unsafe fn invoke_typed_cleanup_callback(
    executor: &mut LoomExecutor,
    task: *mut LoomTask,
    callback: LoomTypedTaskCallback,
    frame: *mut c_void,
) -> CleanupCallbackInvocation {
    let Some(cleanup) = CleanupPhaseGuard::enter(executor) else {
        return CleanupCallbackInvocation {
            step: TASK_FAULTED,
            topology_intact: true,
            activation_intact: true,
            cleanup_phase_entered: false,
        };
    };
    let topology = unsafe { CleanupTopologySnapshot::capture(executor, task) };
    let Some(activation) = RecoverableExecutorActivation::enter(ptr::from_mut(executor)) else {
        drop(cleanup);
        return CleanupCallbackInvocation {
            step: TASK_FAULTED,
            topology_intact: unsafe { topology.validate_and_restore(executor, task) },
            activation_intact: false,
            cleanup_phase_entered: true,
        };
    };
    let step = unsafe { callback(task.cast(), ptr::from_mut(executor).cast(), frame) };
    let activation_intact = activation.finish();
    drop(cleanup);
    CleanupCallbackInvocation {
        step,
        topology_intact: unsafe { topology.validate_and_restore(executor, task) },
        activation_intact,
        cleanup_phase_entered: true,
    }
}

/// Disposes one runtime-owned initialized result. This is structured Task
/// cleanup, not a GC finalizer: it runs at a deterministic scheduler boundary
/// while the stable frame and attached Runtime are still alive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CleanupOutcome {
    Clean,
    Faulted,
    Defect,
}

impl CleanupOutcome {
    const fn is_clean(self) -> bool {
        matches!(self, Self::Clean)
    }

    const fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Defect, _) | (_, Self::Defect) => Self::Defect,
            (Self::Faulted, _) | (_, Self::Faulted) => Self::Faulted,
            (Self::Clean, Self::Clean) => Self::Clean,
        }
    }
}

#[allow(clippy::too_many_lines)]
unsafe fn dispose_typed_result(executor: &mut LoomExecutor, task: *mut LoomTask) -> CleanupOutcome {
    if !executor_owns(executor, task) {
        return CleanupOutcome::Defect;
    }
    let (callback, frame, fault_before, cancellation_primary) = {
        let task_ref = unsafe { &mut *task };
        let cancellation_primary =
            task_ref.cancel_requested || task_ref.status == TaskStatus::Cancelled;
        let Some(typed) = task_ref.typed.as_mut() else {
            return CleanupOutcome::Clean;
        };
        if !typed.result_initialized || typed.result_taken || typed.result_disposed {
            return CleanupOutcome::Clean;
        }
        if typed.dispose_invoked {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_DISPOSE_TWICE",
                    "typed Task result disposal was attempted more than once",
                );
            }
            return CleanupOutcome::Defect;
        }
        typed.dispose_invoked = true;
        (
            typed.dispose_result,
            typed.frame_pointer(),
            task_ref.primary_fault_recorded,
            cancellation_primary,
        )
    };

    let previous_active = executor.active_task;
    executor.active_task = task;
    let invocation = if let Some(callback) = callback {
        unsafe { invoke_typed_cleanup_callback(executor, task, callback, frame) }
    } else {
        CleanupCallbackInvocation {
            step: TASK_COMPLETED,
            topology_intact: true,
            activation_intact: true,
            cleanup_phase_entered: true,
        }
    };
    if !invocation.cleanup_phase_entered {
        unsafe {
            record_typed_runtime_defect(
                task,
                "LOOM_RUNTIME_TYPED_CLEANUP_DEPTH",
                "typed result disposal exceeded the cleanup nesting limit",
            );
        }
    }
    if !invocation.activation_intact {
        unsafe {
            record_typed_runtime_defect(
                task,
                "LOOM_RUNTIME_TYPED_DISPOSE_ACTIVATION",
                "typed Task result disposal leaked runtime activation or root state",
            );
        }
    }
    if !invocation.topology_intact {
        unsafe {
            record_typed_runtime_defect(
                task,
                "LOOM_RUNTIME_TYPED_DISPOSE_TOPOLOGY",
                "typed Task result disposal changed scheduler topology",
            );
        }
    }
    executor.active_task = previous_active;

    // A result disposer may explicitly close one or more published built-in
    // resources. Whatever remains in the compiler-private ledger is the
    // runtime fallback for an unconsumed result and must be released at this
    // structured disposal boundary, even when the callback reports a fault or
    // protocol defect. Reaping the retired Task is only memory reclamation.
    unsafe { (*task).owned_result_resources.clear() };
    let typed = unsafe { (*task).typed.as_mut().expect("typed result owner") };
    if typed.result_size != 0 {
        unsafe { ptr::write_bytes(typed.result_pointer(), 0, typed.result_size) };
    }
    typed.result_initialized = false;
    typed.result_disposed = true;

    let step = invocation.step;
    let cleanup_recorded_fault = !fault_before && unsafe { (*task).primary_fault_recorded };
    if cancellation_primary && !fault_before && invocation.protocol_intact() {
        match step {
            TASK_COMPLETED if !cleanup_recorded_fault => return CleanupOutcome::Clean,
            TASK_FAULTED if cleanup_recorded_fault => {
                // This is a legitimate cleanup RuntimeFault after an already
                // established cancellation, so cancellation remains primary.
                unsafe { suppress_new_typed_cleanup_fault(task, fault_before) };
                return CleanupOutcome::Clean;
            }
            _ => {}
        }
    }
    if !invocation.protocol_intact() {
        return CleanupOutcome::Defect;
    }
    match step {
        TASK_COMPLETED if !cleanup_recorded_fault => CleanupOutcome::Clean,
        TASK_COMPLETED => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_DISPOSE_STATUS",
                    "typed Task result disposal recorded a fault but returned completed",
                );
            }
            CleanupOutcome::Defect
        }
        TASK_PENDING => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_DISPOSE_PENDING",
                    "typed Task result disposal must not suspend",
                );
            }
            CleanupOutcome::Defect
        }
        TASK_FAULTED => {
            if unsafe { (*task).primary_fault_recorded } {
                CleanupOutcome::Faulted
            } else {
                unsafe {
                    record_typed_runtime_defect(
                        task,
                        "LOOM_RUNTIME_TYPED_DISPOSE_FAULTED",
                        "typed Task result disposal failed without recording a fault",
                    );
                }
                CleanupOutcome::Defect
            }
        }
        TASK_CANCELLED => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_DISPOSE_CANCELLED",
                    "typed Task result disposal returned cancellation",
                );
            }
            CleanupOutcome::Defect
        }
        _ => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_DISPOSE_STATUS",
                    "typed Task result disposal returned an invalid status",
                );
            }
            CleanupOutcome::Defect
        }
    }
}

unsafe fn retire_terminal_typed_children(
    executor: &mut LoomExecutor,
    owner: *mut LoomTask,
) -> Option<(*mut LoomTask, CleanupOutcome, Option<*mut LoomTask>)> {
    let children = unsafe { (*owner).owned_children.clone() };
    let mut first_failure = None;
    let mut first_defect = None;
    for child in children.into_iter().rev() {
        if unsafe { (*child).typed.is_none() } || !terminal(unsafe { (*child).status }) {
            continue;
        }
        if unsafe { (*child).status } == TaskStatus::Completed {
            let outcome = unsafe { dispose_typed_result(executor, child) };
            if !outcome.is_clean() {
                unsafe { (*child).status = TaskStatus::Faulted };
                first_failure.get_or_insert((child, outcome));
                if outcome == CleanupOutcome::Defect {
                    first_defect.get_or_insert(child);
                }
            }
        }
        unsafe { retire_typed_child(executor, owner, child) };
    }
    first_failure.map(|(task, outcome)| (task, outcome, first_defect))
}

#[allow(clippy::too_many_lines)]
unsafe fn retire_typed_frame(executor: &mut LoomExecutor, task: *mut LoomTask) -> CleanupOutcome {
    if !executor_owns(executor, task) || unsafe { (*task).typed.is_none() } {
        return CleanupOutcome::Defect;
    }
    let needs_cancel = {
        let task_ref = unsafe { &*task };
        let typed = task_ref.typed.as_ref().expect("typed retirement branch");
        typed.initialized && !terminal(task_ref.status)
    };
    let mut outcome = CleanupOutcome::Clean;
    if needs_cancel {
        let (callback, frame, fault_before, cancellation_primary) = {
            let task_ref = unsafe { &mut *task };
            let cancellation_primary = task_ref.cancel_requested;
            let typed = task_ref.typed.as_mut().expect("typed retirement branch");
            if typed.cancel_invoked {
                unsafe {
                    record_typed_runtime_defect(
                        task,
                        "LOOM_RUNTIME_TYPED_CANCEL_TWICE",
                        "typed coroutine frame retirement attempted cancellation twice",
                    );
                }
                return CleanupOutcome::Defect;
            }
            typed.cancel_invoked = true;
            task_ref.cancel_requested = true;
            task_ref.status = TaskStatus::Running;
            (
                typed.cancel,
                typed.frame_pointer(),
                task_ref.primary_fault_recorded,
                cancellation_primary,
            )
        };
        let previous_active = executor.active_task;
        executor.active_task = task;
        let invocation = unsafe { invoke_typed_cleanup_callback(executor, task, callback, frame) };
        if !invocation.cleanup_phase_entered {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CLEANUP_DEPTH",
                    "typed frame cancellation exceeded the cleanup nesting limit",
                );
            }
        }
        if !invocation.activation_intact {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CANCEL_ACTIVATION",
                    "typed coroutine cancellation leaked runtime activation or root state",
                );
            }
        }
        if !invocation.topology_intact {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CANCEL_TOPOLOGY",
                    "typed coroutine cancellation changed scheduler topology",
                );
            }
        }
        executor.active_task = previous_active;

        // A malicious cleanup callback cannot leave a registration referring
        // to a frame that retirement is about to free.
        let registrations = unsafe { std::mem::take(&mut (*task).waits) };
        for registration in registrations {
            let _ = unsafe { cancel_for_task(executor, &raw const registration) };
        }
        let step = invocation.step;
        let cleanup_recorded_fault = !fault_before && unsafe { (*task).primary_fault_recorded };
        let cancel_outcome = if cancellation_primary
            && !fault_before
            && invocation.protocol_intact()
            && step == TASK_FAULTED
            && cleanup_recorded_fault
        {
            unsafe {
                suppress_new_typed_cleanup_fault(task, fault_before);
                (*task).status = TaskStatus::Cancelled;
            }
            CleanupOutcome::Clean
        } else if !invocation.protocol_intact() {
            unsafe { (*task).status = TaskStatus::Faulted };
            CleanupOutcome::Defect
        } else {
            match step {
                TASK_CANCELLED if !cleanup_recorded_fault => {
                    unsafe { (*task).status = TaskStatus::Cancelled };
                    CleanupOutcome::Clean
                }
                TASK_CANCELLED => {
                    unsafe {
                        record_typed_runtime_defect(
                            task,
                            "LOOM_RUNTIME_TYPED_CANCEL_STATUS",
                            "typed coroutine cancellation recorded a fault but returned cancelled",
                        );
                        (*task).status = TaskStatus::Faulted;
                    }
                    CleanupOutcome::Defect
                }
                TASK_FAULTED => {
                    if unsafe { (*task).primary_fault_recorded } {
                        unsafe { (*task).status = TaskStatus::Faulted };
                        CleanupOutcome::Faulted
                    } else {
                        unsafe {
                            record_typed_runtime_defect(
                                task,
                                "LOOM_RUNTIME_TYPED_CANCEL_FAULTED",
                                "typed coroutine cancellation failed without recording a fault",
                            );
                        }
                        unsafe { (*task).status = TaskStatus::Faulted };
                        CleanupOutcome::Defect
                    }
                }
                TASK_PENDING => {
                    unsafe {
                        record_typed_runtime_defect(
                            task,
                            "LOOM_RUNTIME_TYPED_CANCEL_PENDING",
                            "typed coroutine frame retirement must not suspend",
                        );
                        (*task).status = TaskStatus::Faulted;
                    }
                    CleanupOutcome::Defect
                }
                _ => {
                    unsafe {
                        record_typed_runtime_defect(
                            task,
                            "LOOM_RUNTIME_TYPED_CANCEL_STATUS",
                            "typed coroutine frame retirement returned an invalid status",
                        );
                        (*task).status = TaskStatus::Faulted;
                    }
                    CleanupOutcome::Defect
                }
            }
        };
        outcome = outcome.combine(cancel_outcome);
    }
    if unsafe { (*task).typed.as_ref().unwrap().result_initialized } {
        let dispose_outcome = unsafe { dispose_typed_result(executor, task) };
        if !dispose_outcome.is_clean() {
            unsafe { (*task).status = TaskStatus::Faulted };
        }
        outcome = outcome.combine(dispose_outcome);
    }
    outcome
}

pub(crate) unsafe fn retire_typed_frames_before_executor_drop(executor: &mut LoomExecutor) {
    // Child frames are created after their owners, so reverse creation order
    // preserves the same inner-before-outer cleanup order as normal structured
    // completion. Initialized nonterminal frames are cancelled first; only a
    // genuinely published result is subsequently passed to dispose_result.
    let tasks = executor
        .tasks
        .iter_mut()
        .rev()
        .map(|task| &raw mut **task)
        .collect::<Vec<_>>();
    for task in tasks {
        if unsafe { (*task).typed.is_some() } {
            let _ = unsafe { retire_typed_frame(executor, task) };
        }
    }
}

unsafe fn complete_terminal(executor: &mut LoomExecutor, task: *mut LoomTask, step: i32) {
    if !executor_owns(executor, task) || terminal(unsafe { (*task).status }) {
        return;
    }
    let children = unsafe { (*task).owned_children.clone() };
    for child in &children {
        if !terminal(unsafe { (**child).status }) {
            unsafe { request_cancel(executor, *child) };
        }
    }
    if !all_terminal(&children) {
        unsafe {
            (*task).deferred_terminal = step;
            (*task).status = TaskStatus::Draining;
        }
        return;
    }
    let mut step = step;
    if let Some((first_failure, _outcome, first_defect)) =
        unsafe { retire_terminal_typed_children(executor, task) }
    {
        let failure = if step == TASK_CANCELLED {
            first_defect.unwrap_or(first_failure)
        } else {
            first_failure
        };
        let replaces_outcome = step == TASK_COMPLETED || first_defect.is_some();
        if replaces_outcome {
            unsafe {
                inherit_primary_task_fault(&mut *task, &*failure);
                if !(*task).primary_fault_recorded {
                    record_typed_runtime_defect(
                        task,
                        "LOOM_RUNTIME_TYPED_CHILD_DISPOSE",
                        "a typed child result could not be disposed",
                    );
                }
            }
            step = TASK_FAULTED;
        }
    }
    if unsafe { (*task).typed.is_some() } {
        if step == TASK_COMPLETED && !unsafe { (*task).typed.as_ref().unwrap().result_initialized }
        {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_RESULT_MISSING",
                    "typed Task completed without publishing its result",
                );
            }
            step = TASK_FAULTED;
        }
        if step != TASK_COMPLETED
            && unsafe { (*task).typed.as_ref().unwrap().result_initialized }
            && !unsafe { dispose_typed_result(executor, task) }.is_clean()
        {
            // Legitimate cleanup faults after cancellation were normalized to
            // Clean above. A remaining Faulted/Defect outcome is an earlier
            // fault or cleanup protocol defect and must not be laundered into
            // cancellation.
            step = TASK_FAULTED;
        }
        if step == TASK_FAULTED && !unsafe { (*task).primary_fault_recorded } {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_TASK_FAULTED",
                    "typed Task returned faulted without recording a fault",
                );
            }
        }
    }
    if step != TASK_COMPLETED {
        // A cancelled or faulted I/O task cannot publish a resource result.
        // Drop both a readiness operation's private descriptor and any worker
        // result that raced with cancellation before making the task terminal.
        let registrations = unsafe { std::mem::take(&mut (*task).waits) };
        for registration in registrations {
            let _ = unsafe { cancel_for_task(executor, &raw const registration) };
        }
        unsafe {
            (*task).io_operation = None;
            (*task).blocking_result = None;
            (*task).owned_result_resources.clear();
        }
    }
    unsafe {
        (*task).status = match step {
            TASK_COMPLETED => TaskStatus::Completed,
            TASK_CANCELLED => TaskStatus::Cancelled,
            _ => TaskStatus::Faulted,
        };
        child_became_terminal(executor, task);
    }
}

unsafe fn consume_notifications(executor: *mut LoomExecutor) {
    loop {
        let mut notification = LoomReadyNotification::default();
        // SAFETY: scheduler owns executor and the stack out pointer.
        if unsafe { pop_for_scheduler(executor, &raw mut notification) } != 1 {
            break;
        }
        let task = notification.frame.cast::<LoomTask>();
        // SAFETY: executor is live for the entire scheduler run.
        let executor_ref = unsafe { &mut *executor };
        if !executor_owns(executor_ref, task) || terminal(unsafe { (*task).status }) {
            continue;
        }
        let Some(index) = unsafe { (*task).waits.iter() }
            .position(|candidate| *candidate == notification.registration)
        else {
            continue;
        };
        unsafe { (*task).waits.remove(index) };
        if unsafe { (*task).waits.is_empty() } && !unsafe { (*task).cancel_requested } {
            unsafe {
                (*task).status = TaskStatus::Runnable;
                enqueue_task(executor_ref, task);
            }
        }
    }
}

unsafe fn drain_worker_completions(executor: *mut LoomExecutor) {
    loop {
        let Some(worker) = (unsafe { (*executor).worker.as_ref() }) else {
            return;
        };
        let completion = match worker.receiver.try_recv() {
            Ok(completion) => completion,
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return,
        };
        let task = completion.task as *mut LoomTask;
        let executor_ref = unsafe { &mut *executor };
        if !executor_owns(executor_ref, task)
            || terminal(unsafe { (*task).status })
            || !unsafe { (*task).waits.contains(&completion.registration) }
        {
            continue;
        }
        unsafe { (*task).blocking_result = Some(completion.result) };
        let _ = unsafe {
            crate::reactor::executor_notify_completion(
                executor,
                &raw const completion.registration,
                0,
                0,
            )
        };
    }
}

fn move_task_frames(executor: &mut LoomExecutor) {
    let mut relocations = 0_u64;
    for task in &mut executor.tasks {
        let pointer = (&raw mut **task).cast::<LoomTask>();
        if task.typed.is_some() || pointer == executor.active_task || task.slots.is_empty() {
            continue;
        }
        task.slots = task.slots.to_vec().into_boxed_slice();
        relocations = relocations.saturating_add(1);
    }
    if relocations != 0 {
        let heap = executor.heap_mut();
        heap.relocations = heap.relocations.saturating_add(relocations);
    }
}

unsafe extern "C" fn resume_wait_leaf(task: *mut LoomTask, executor: *mut LoomExecutor) -> i32 {
    if executor.is_null() || task.is_null() || !unsafe { (*task).wait_leaf } {
        return TASK_FAULTED;
    }
    if unsafe { (*task).cancel_requested } {
        return TASK_CANCELLED;
    }
    match unsafe { (*task).state } {
        0 => {
            let source = unsafe { (*task).wait_source };
            // SAFETY: task and source are live and owned by executor.
            if unsafe { task_suspend_wait(executor, task, &raw const source) } != WAIT_OK {
                return TASK_FAULTED;
            }
            unsafe { (*task).state = 1 };
            TASK_PENDING
        }
        1 => {
            let result = unsafe { &mut (*task).slots[(*task).result_slot] };
            *result = ValueSlot::default();
            TASK_COMPLETED
        }
        _ => TASK_FAULTED,
    }
}

const TYPED_TIMER_REGISTRATION_FAULT_CODE: &str = "TimerRegistrationFault";
const TYPED_TIMER_REGISTRATION_FAULT_MESSAGE: &str = "could not register timer wait";

/// Runtime-owned callback for the narrow typed `Task[Unit]` timer factory.
/// The absolute deadline lives in the scheduler's existing copied `WaitSource`;
/// the one-byte typed frame is intentionally rootless and carries no source
/// value or legacy universal envelope.
unsafe extern "C" fn resume_typed_timer(
    task: *mut c_void,
    executor: *mut c_void,
    _frame: *mut c_void,
) -> i32 {
    let task = task.cast::<LoomTask>();
    let executor = executor.cast::<LoomExecutor>();
    if task.is_null()
        || executor.is_null()
        || unsafe { (*task).typed.is_none() }
        || !unsafe { (*task).wait_leaf }
        || unsafe { (*task).wait_source.kind } != WAIT_SOURCE_TIMER
    {
        return unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_TIMER_STATE",
                "typed timer Task has invalid runtime state",
            )
        };
    }
    match unsafe { (*task).state } {
        0 => {
            let source = unsafe { (*task).wait_source };
            let status = unsafe { task_suspend_wait(executor, task, &raw const source) };
            if status != WAIT_OK {
                return unsafe {
                    fail_message(
                        task,
                        TYPED_TIMER_REGISTRATION_FAULT_CODE,
                        TYPED_TIMER_REGISTRATION_FAULT_MESSAGE,
                    )
                };
            }
            unsafe { (*task).state = 1 };
            TASK_PENDING
        }
        1 => {
            let status = unsafe { typed_task_publish_result_v1(task) };
            if status != TYPED_TASK_OK {
                return unsafe {
                    fail_message(
                        task,
                        "LOOM_RUNTIME_TYPED_TIMER_RESULT",
                        "typed timer Task could not publish its Unit result",
                    )
                };
            }
            TASK_COMPLETED
        }
        _ => unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_TIMER_STATE",
                "typed timer Task resumed from an invalid state",
            )
        },
    }
}

unsafe extern "C" fn cancel_typed_timer(
    task: *mut c_void,
    executor: *mut c_void,
    _frame: *mut c_void,
) -> i32 {
    if task.is_null() || executor.is_null() {
        return TASK_FAULTED;
    }
    TASK_CANCELLED
}

unsafe extern "C" fn resume_composite(task: *mut LoomTask, executor: *mut LoomExecutor) -> i32 {
    if executor.is_null() || task.is_null() || unsafe { (*task).composite_spec.is_null() } {
        return TASK_FAULTED;
    }
    if unsafe { (*task).cancel_requested } {
        return TASK_CANCELLED;
    }
    let spec = unsafe { (*task).composite_spec };
    if unsafe { (*spec).executor } != executor || unsafe { (*spec).task } != task {
        return TASK_FAULTED;
    }
    if unsafe { (*task).state } == 0 {
        let prepare = unsafe { task_prepare_join(executor, task, (*spec).mode) };
        if prepare != WAIT_OK {
            return TASK_FAULTED;
        }
        for child in unsafe { (*spec).tasks.clone() } {
            if unsafe { task_add_join_child(executor, task, child) } != WAIT_OK {
                unsafe { (*task).join_active = false };
                return TASK_FAULTED;
            }
        }
        let suspend = unsafe { task_suspend_join(executor, task) };
        if suspend < 0 {
            return TASK_FAULTED;
        }
        unsafe { (*task).state = 1 };
        if suspend != 0 {
            return TASK_PENDING;
        }
    } else if unsafe { (*task).state } != 1 {
        return TASK_FAULTED;
    }
    let step = unsafe { (*task).join_step };
    if step != TASK_COMPLETED {
        return step;
    }
    let result = unsafe { task_result(task) };
    let write = unsafe { task_write_join_result(task, result, (*spec).shape) };
    if write == WAIT_OK {
        TASK_COMPLETED
    } else {
        TASK_FAULTED
    }
}

unsafe extern "C" fn resume_io(task: *mut LoomTask, executor: *mut LoomExecutor) -> i32 {
    if task.is_null() || executor.is_null() {
        return TASK_FAULTED;
    }
    if unsafe { (*task).cancel_requested } {
        return TASK_CANCELLED;
    }
    if let Some(result) = unsafe { (*task).blocking_result.take() } {
        return unsafe { finish_blocking_result(task, executor, result) };
    }
    let Some(operation) = (unsafe { (*task).io_operation.take() }) else {
        return unsafe {
            fail_message(
                task,
                "IoCompletionFault",
                "I/O task resumed without a result",
            )
        };
    };
    match operation {
        IoOperation::FileOpen { path, create } => unsafe {
            suspend_blocking(task, executor, move || blocking_file_open(path, create))
        },
        IoOperation::FileRead { file } => unsafe {
            suspend_blocking(task, executor, move || blocking_file_read(file))
        },
        IoOperation::FileWrite { file, bytes } => unsafe {
            suspend_blocking(task, executor, move || blocking_file_write(file, &bytes))
        },
        IoOperation::SocketConnect { host, port } => unsafe {
            suspend_blocking(task, executor, move || blocking_socket_connect(&host, port))
        },
        IoOperation::SocketRead { socket, bytes } => unsafe {
            resume_socket_read(task, executor, socket, bytes)
        },
        IoOperation::SocketWrite {
            socket,
            bytes,
            offset,
        } => unsafe { resume_socket_write(task, executor, socket, bytes, offset) },
    }
}

unsafe fn suspend_blocking<F>(task: *mut LoomTask, executor: *mut LoomExecutor, work: F) -> i32
where
    F: FnOnce() -> BlockingResult + Send + 'static,
{
    let source = LoomWaitSource {
        abi_version: WAIT_ABI_VERSION,
        kind: crate::WAIT_SOURCE_COMPLETION,
        handle: -1,
        interests: 0,
        reserved: 0,
        deadline_ns: 0,
    };
    if unsafe { task_suspend_wait(executor, task, &raw const source) } != WAIT_OK {
        return unsafe {
            fail_message(task, "IoWaitFault", "could not register worker completion")
        };
    }
    let registration = unsafe { *(*task).waits.last().expect("completion was registered") };
    let executor_ref = unsafe { &mut *executor };
    let sender = executor_ref.ensure_worker().sender.clone();
    let poller = Arc::clone(
        &executor_ref
            .reactor
            .as_ref()
            .expect("completion registration initialized the reactor")
            .poller,
    );
    let task_address = task as usize;
    let job: BlockingJob = Box::new(move || {
        let result = work();
        let _ = sender.send(WorkerCompletion {
            task: task_address,
            registration,
            result,
        });
        let _ = poller.notify();
    });
    match blocking_pool().try_send(job) {
        Ok(()) => TASK_PENDING,
        Err(mpsc::TrySendError::Full(_) | mpsc::TrySendError::Disconnected(_)) => {
            let _ = unsafe { cancel_for_task(&raw mut *executor, &raw const registration) };
            unsafe {
                (*task).waits.retain(|candidate| *candidate != registration);
                (*task).status = TaskStatus::Running;
            }
            unsafe {
                fail_message(
                    task,
                    "BlockingPoolSaturated",
                    "blocking I/O pool is saturated",
                )
            }
        }
    }
}

fn blocking_file_open(path: String, create: bool) -> BlockingResult {
    let opened = if create {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
    } else {
        File::open(&path)
    };
    match opened {
        Ok(file) => BlockingResult::Resource {
            nominal: FILE_TYPE,
            resource: file.into(),
        },
        Err(error) => BlockingResult::Fault {
            code: if create {
                "FileCreateFault"
            } else {
                "FileOpenFault"
            },
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

fn blocking_file_read(mut file: File) -> BlockingResult {
    let mut bytes = Vec::new();
    match file.read_to_end(&mut bytes) {
        Ok(_) => BlockingResult::Text {
            bytes,
            code: "FileReadFault",
        },
        Err(error) => BlockingResult::Fault {
            code: "FileReadFault",
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

fn blocking_file_write(mut file: File, bytes: &[u8]) -> BlockingResult {
    match file.write_all(bytes) {
        Ok(()) => BlockingResult::Unit,
        Err(error) => BlockingResult::Fault {
            code: "FileWriteFault",
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

fn blocking_socket_connect(host: &str, port: u16) -> BlockingResult {
    let mut addresses = match (host, port).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(error) => {
            return BlockingResult::Fault {
                code: "SocketResolveFault",
                kind: io_error_kind(&error),
                message: error.to_string(),
            };
        }
    };
    let address = addresses.next();
    let Some(address) = address else {
        return BlockingResult::Fault {
            code: "SocketResolveFault",
            kind: 9,
            message: "host resolved to no addresses".into(),
        };
    };
    match TcpStream::connect(address) {
        Ok(socket) => {
            if let Err(error) = socket.set_nonblocking(true) {
                return BlockingResult::Fault {
                    code: "SocketConnectFault",
                    kind: io_error_kind(&error),
                    message: error.to_string(),
                };
            }
            BlockingResult::Resource {
                nominal: SOCKET_TYPE,
                resource: socket.into(),
            }
        }
        Err(error) => BlockingResult::Fault {
            code: "SocketConnectFault",
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

unsafe fn finish_blocking_result(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
    result: BlockingResult,
) -> i32 {
    match result {
        BlockingResult::Resource { nominal, resource } => unsafe {
            store_resource_result(task, nominal, resource)
        },
        BlockingResult::Text { bytes, code } => unsafe {
            store_text_result(task, executor, &bytes, code)
        },
        BlockingResult::Unit => unsafe { store_unit_result(task) },
        BlockingResult::Fault {
            code,
            kind,
            message,
        } => unsafe { complete_io_error(task, kind, code, &message) },
    }
}

unsafe fn resume_socket_read(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
    mut socket: TcpStream,
    mut bytes: Vec<u8>,
) -> i32 {
    let handle = socket_handle_bits(&socket);
    let mut chunk = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        match socket.read(&mut chunk) {
            Ok(0) => {
                return unsafe { store_text_result(task, executor, &bytes, "SocketReadFault") };
            }
            Ok(length) => bytes.extend_from_slice(&chunk[..length]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => unsafe {
                return suspend_io(
                    task,
                    executor,
                    IoOperation::SocketRead { socket, bytes },
                    handle,
                    crate::WAIT_READABLE,
                );
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => unsafe { return fail_io(task, "SocketReadFault", &error) },
        }
    }
}

unsafe fn resume_socket_write(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
    mut socket: TcpStream,
    bytes: Vec<u8>,
    mut offset: usize,
) -> i32 {
    if offset == bytes.len() {
        return unsafe { store_unit_result(task) };
    }
    let handle = socket_handle_bits(&socket);
    loop {
        if offset == bytes.len() {
            return unsafe { store_unit_result(task) };
        }
        match socket.write(&bytes[offset..]) {
            Ok(0) => unsafe {
                return complete_io_error(
                    task,
                    9,
                    "SocketWriteFault",
                    "socket accepted zero bytes",
                );
            },
            Ok(written) => {
                offset += written;
                if offset == bytes.len() {
                    return unsafe { store_unit_result(task) };
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => unsafe {
                return suspend_io(
                    task,
                    executor,
                    IoOperation::SocketWrite {
                        socket,
                        bytes,
                        offset,
                    },
                    handle,
                    crate::WAIT_WRITABLE,
                );
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => unsafe { return fail_io(task, "SocketWriteFault", &error) },
        }
    }
}

fn build_runtime_aggregate(mut aggregate: ValueSlot, values: Vec<ValueSlot>) -> Option<ValueSlot> {
    let count_word = if aggregate.words[0] == VALUE_TAG_ENUM {
        3
    } else {
        2
    };
    aggregate.words[count_word] = 0;
    aggregate.words[4] = 0;
    if values.is_empty() {
        return Some(aggregate);
    }
    let mut initial = Vec::with_capacity(values.len() + 1);
    initial.push(aggregate);
    initial.extend(values);
    let roots = RuntimeRootScope::from_values(initial).ok()?;
    let stream = NodeStream::new(&roots, 0, aggregate);
    for index in (1..roots.len()).rev() {
        if stream.prepend(index) != crate::GC_OK {
            return None;
        }
    }
    Some(roots.read(0))
}

fn record_value(nominal: u64, fields: Vec<ValueSlot>) -> Option<ValueSlot> {
    let mut record = ValueSlot::default();
    record.words[0] = VALUE_TAG_RECORD;
    record.words[1] = nominal;
    build_runtime_aggregate(record, fields)
}

fn enum_value(nominal: u64, variant: u64, payload: Vec<ValueSlot>) -> Option<ValueSlot> {
    let mut value = ValueSlot::default();
    value.words[0] = VALUE_TAG_ENUM;
    value.words[1] = nominal;
    value.words[2] = variant;
    build_runtime_aggregate(value, payload)
}

unsafe fn store_resource_result(task: *mut LoomTask, nominal: u64, resource: OwnedResource) -> i32 {
    debug_assert_eq!(resource.is_file(), nominal == FILE_TYPE);
    let mut raw = ValueSlot::default();
    raw.words[0] = 2;
    raw.words[3] = resource.handle_bits().cast_unsigned();
    let Some(result) = record_value(nominal, vec![raw]) else {
        return unsafe { fail_message(task, "OutOfMemory", "resource result allocation failed") };
    };
    unsafe { (*task).owned_result_resources.push(resource) };
    unsafe { store_io_success(task, result) }
}

unsafe fn store_text_result(
    task: *mut LoomTask,
    _executor: *mut LoomExecutor,
    bytes: &[u8],
    code: &str,
) -> i32 {
    if std::str::from_utf8(bytes).is_err() {
        return unsafe { complete_io_error(task, 3, code, "I/O bytes are not valid UTF-8 Text") };
    }
    let Some(result) = crate::gc::text_value(bytes) else {
        return unsafe { fail_message(task, "OutOfMemory", "Text allocation failed") };
    };
    unsafe { store_io_success(task, result) }
}

unsafe fn store_unit_result(task: *mut LoomTask) -> i32 {
    unsafe { store_io_success(task, ValueSlot::default()) }
}

unsafe fn store_io_success(task: *mut LoomTask, value: ValueSlot) -> i32 {
    let result = if unsafe { (*task).io_fallible } {
        let Some(result) = enum_value(RESULT_TYPE, 0, vec![value]) else {
            return unsafe { fail_message(task, "OutOfMemory", "Result allocation failed") };
        };
        result
    } else {
        value
    };
    let slot = unsafe { &mut (*task).slots[(*task).result_slot] };
    *slot = result;
    TASK_COMPLETED
}

unsafe fn suspend_io(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
    operation: IoOperation,
    handle: i64,
    interests: u32,
) -> i32 {
    let source = LoomWaitSource {
        abi_version: WAIT_ABI_VERSION,
        kind: WAIT_SOURCE_IO,
        handle,
        interests,
        reserved: 0,
        deadline_ns: 0,
    };
    unsafe { (*task).io_operation = Some(operation) };
    if unsafe { task_suspend_wait(executor, task, &raw const source) } == WAIT_OK {
        TASK_PENDING
    } else {
        unsafe { fail_message(task, "IoWaitFault", "could not register I/O readiness") }
    }
}

unsafe fn fail_io(task: *mut LoomTask, code: &str, error: &io::Error) -> i32 {
    unsafe { complete_io_error(task, io_error_kind(error), code, &error.to_string()) }
}

fn io_error_kind(error: &io::Error) -> u32 {
    match error.kind() {
        io::ErrorKind::NotFound => 0,
        io::ErrorKind::PermissionDenied => 1,
        io::ErrorKind::AlreadyExists => 2,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData | io::ErrorKind::Unsupported => 3,
        io::ErrorKind::ConnectionRefused => 4,
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => 5,
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => 6,
        io::ErrorKind::UnexpectedEof => 7,
        io::ErrorKind::NotConnected => 8,
        _ => 9,
    }
}

unsafe fn complete_io_error(task: *mut LoomTask, kind: u32, code: &str, message: &str) -> i32 {
    if task.is_null() || !unsafe { (*task).io_fallible } {
        return unsafe { fail_message(task, code, message) };
    }
    let mut kind_value = ValueSlot::default();
    kind_value.words[0] = VALUE_TAG_ENUM;
    kind_value.words[1] = IO_ERROR_KIND_TYPE;
    kind_value.words[2] = u64::from(kind);
    let message = text_value(message.as_bytes());
    let Some(error) = record_value(IO_ERROR_TYPE, vec![kind_value, message]) else {
        return unsafe { fail_message(task, "OutOfMemory", "I/O error allocation failed") };
    };
    let Some(result) = enum_value(RESULT_TYPE, 1, vec![error]) else {
        return unsafe { fail_message(task, "OutOfMemory", "Result error allocation failed") };
    };
    let slot = unsafe { &mut (*task).slots[(*task).result_slot] };
    *slot = result;
    TASK_COMPLETED
}

unsafe fn fail_message(task: *mut LoomTask, code: &str, message: &str) -> i32 {
    if !task.is_null() {
        // SAFETY: the caller supplied a live task owned by its executor.
        unsafe {
            record_primary_task_fault(&mut *task, code.into(), message.into(), String::new());
        }
    }
    TASK_FAULTED
}

fn empty_legacy_descriptor() -> LoomCoroutineDescriptor {
    LoomCoroutineDescriptor {
        abi_version: COROUTINE_ABI_VERSION,
        flags: 0,
        resume: None,
        cancel: None,
        trace: None,
        slot_count: 0,
        witness_count: 0,
        result_slot: 0,
        state_count: 0,
        live_bitmap_words: 0,
        live_bitmaps: ptr::null(),
    }
}

fn is_aligned_for<T>(pointer: *const T) -> bool {
    !pointer.is_null() && (pointer as usize).is_multiple_of(align_of::<T>())
}

struct CopiedTypedRoots {
    offsets: Box<[usize]>,
    state_count: usize,
    bitmap_words: usize,
    live_bitmaps: Box<[u64]>,
    completed_state: usize,
}

unsafe fn copy_typed_roots(
    descriptor: &LoomTypedCoroutineDescriptor,
    frame_size: usize,
    frame_align: usize,
    result_offset: usize,
    result_size: usize,
) -> Option<CopiedTypedRoots> {
    let (Ok(slot_count), Ok(state_count), Ok(bitmap_words)) = (
        usize::try_from(descriptor.root_slot_count),
        usize::try_from(descriptor.root_state_count),
        usize::try_from(descriptor.root_bitmap_words),
    ) else {
        return None;
    };
    let total_bitmap_words = state_count.checked_mul(bitmap_words)?;
    if descriptor.root_slot_count > GC_MAX_ROOT_SLOTS
        || state_count == 0
        || descriptor.root_state_count > GC_MAX_ROOT_STATES
        || bitmap_words != slot_count.div_ceil(64)
        || u64::try_from(total_bitmap_words).map_or(true, |words| words > GC_MAX_ROOT_BITMAP_WORDS)
        || usize::try_from(descriptor.completed_root_state)
            .map_or(true, |state| state >= state_count)
        || (slot_count == 0) != descriptor.root_offsets.is_null()
        || (total_bitmap_words == 0) != descriptor.live_bitmaps.is_null()
        || (slot_count != 0
            && (!is_aligned_for(descriptor.root_offsets)
                || frame_align < align_of::<*mut c_void>()))
        || (total_bitmap_words != 0 && !is_aligned_for(descriptor.live_bitmaps))
    {
        return None;
    }

    let source_offsets = if slot_count == 0 {
        &[][..]
    } else {
        // SAFETY: the ABI requires this bounded, aligned array to remain
        // readable for the call. Canonical null/count shape was checked.
        unsafe { slice::from_raw_parts(descriptor.root_offsets, slot_count) }
    };
    let mut offsets = Vec::new();
    offsets.try_reserve_exact(slot_count).ok()?;
    let pointer_size = size_of::<*mut c_void>();
    for (index, offset) in source_offsets.iter().copied().enumerate() {
        let offset = usize::try_from(offset).ok()?;
        if !offset.is_multiple_of(align_of::<*mut c_void>())
            || offset
                .checked_add(pointer_size)
                .is_none_or(|end| end > frame_size)
            || index > 0 && offsets[index - 1] >= offset
        {
            return None;
        }
        offsets.push(offset);
    }

    let source_bitmaps = if total_bitmap_words == 0 {
        &[][..]
    } else {
        // SAFETY: same bounded copied-metadata contract as root_offsets.
        unsafe { slice::from_raw_parts(descriptor.live_bitmaps, total_bitmap_words) }
    };
    let mut live_bitmaps = Vec::new();
    live_bitmaps.try_reserve_exact(total_bitmap_words).ok()?;
    live_bitmaps.extend_from_slice(source_bitmaps);
    if let Some(remainder) = slot_count.checked_rem(64).filter(|value| *value != 0) {
        let allowed = (1_u64 << remainder) - 1;
        for state in 0..state_count {
            if live_bitmaps[state * bitmap_words + bitmap_words - 1] & !allowed != 0 {
                return None;
            }
        }
    }
    let completed_state = usize::try_from(descriptor.completed_root_state).ok()?;
    let result_end = result_offset + result_size;
    for index in 0..slot_count {
        if bitmap_words != 0
            && live_bitmaps[completed_state * bitmap_words + index / 64] & (1_u64 << (index % 64))
                != 0
        {
            let offset = offsets[index];
            if offset < result_offset || offset + pointer_size > result_end {
                return None;
            }
        }
    }
    Some(CopiedTypedRoots {
        offsets: offsets.into_boxed_slice(),
        state_count,
        bitmap_words,
        live_bitmaps: live_bitmaps.into_boxed_slice(),
        completed_state,
    })
}

unsafe fn copy_typed_task_storage(
    descriptor: *const LoomTypedCoroutineDescriptor,
) -> Option<TypedTaskStorage> {
    if !is_aligned_for(descriptor) {
        return None;
    }
    // SAFETY: the ABI requires a readable descriptor at this aligned pointer.
    let descriptor = unsafe { &*descriptor };
    let (Some(resume), Some(cancel)) = (descriptor.resume, descriptor.cancel) else {
        return None;
    };
    if descriptor.abi_version != TYPED_TASK_ABI_VERSION || descriptor.flags != 0 {
        return None;
    }
    let (Ok(frame_size), Ok(frame_align), Ok(result_offset), Ok(result_size), Ok(result_align)) = (
        usize::try_from(descriptor.frame_size),
        usize::try_from(descriptor.frame_align),
        usize::try_from(descriptor.result_offset),
        usize::try_from(descriptor.result_size),
        usize::try_from(descriptor.result_align),
    ) else {
        return None;
    };
    if frame_size == 0
        || descriptor.frame_size > GC_MAX_OBJECT_BYTES
        || frame_align == 0
        || !frame_align.is_power_of_two()
        || descriptor.frame_align > GC_MAX_OBJECT_ALIGNMENT
        || result_align == 0
        || !result_align.is_power_of_two()
        || result_align > frame_align
        || result_offset % result_align != 0
        || result_offset
            .checked_add(result_size)
            .is_none_or(|end| end > frame_size)
    {
        return None;
    }
    let frame_layout = Layout::from_size_align(frame_size, frame_align).ok()?;

    let roots = unsafe {
        copy_typed_roots(
            descriptor,
            frame_size,
            frame_align,
            result_offset,
            result_size,
        )
    }?;

    // SAFETY: the validated nonzero Layout is bounded by the shared ABI
    // resource limits. Null reports allocation failure without publishing a
    // partially constructed Task.
    let frame = NonNull::new(unsafe { alloc_zeroed(frame_layout) })?;
    Some(TypedTaskStorage {
        resume,
        cancel,
        dispose_result: descriptor.dispose_result,
        frame,
        frame_layout,
        result_offset,
        result_size,
        result_align,
        root_offsets: roots.offsets,
        root_state_count: roots.state_count,
        root_bitmap_words: roots.bitmap_words,
        live_bitmaps: roots.live_bitmaps,
        completed_root_state: roots.completed_state,
        root_state: 0,
        initialized: false,
        published: false,
        result_initialized: false,
        result_taken: false,
        result_disposed: false,
        cancel_invoked: false,
        dispose_invoked: false,
        join_completion_pending: false,
        join_cancel_authorized: false,
        join_winner_finalized: false,
    })
}

/// Allocates an unpublished typed Task and its zeroed stable coroutine frame.
///
/// The descriptor and its variable-length metadata are fully validated and
/// copied. The Task is not linked into its structured owner or ready queue
/// until `loom_typed_task_publish_v1` succeeds.
#[unsafe(export_name = "loom_typed_task_create_v1")]
pub unsafe extern "C" fn typed_task_create_v1(
    executor: *mut LoomExecutor,
    descriptor: *const LoomTypedCoroutineDescriptor,
) -> *mut LoomTask {
    if executor.is_null() || unsafe { (*executor).cleanup_active() } {
        return ptr::null_mut();
    }
    let Some(typed) = (unsafe { copy_typed_task_storage(descriptor) }) else {
        return ptr::null_mut();
    };
    let executor_ref = unsafe { &mut *executor };
    let pending_owner = executor_ref.active_task;
    if !pending_owner.is_null()
        && (!executor_owns(executor_ref, pending_owner)
            || unsafe { (*pending_owner).status } != TaskStatus::Running
            || unsafe { (*pending_owner).cancel_requested })
    {
        return ptr::null_mut();
    }
    let mut task = Box::new(LoomTask {
        descriptor: empty_legacy_descriptor(),
        slots: Box::new([]),
        result_slot: 0,
        state: 0,
        executor,
        owner: ptr::null_mut(),
        owned_children: Vec::new(),
        join_children: Vec::new(),
        waits: Vec::new(),
        status: TaskStatus::Unpublished,
        deferred_terminal: TASK_PENDING,
        join_mode: TASK_JOIN_ALL,
        join_winner: NO_JOIN_WINNER,
        join_step: TASK_COMPLETED,
        queued: false,
        cancel_requested: false,
        join_active: false,
        wait_leaf: false,
        wait_source: LoomWaitSource::default(),
        composite_spec: ptr::null_mut(),
        io_operation: None,
        blocking_result: None,
        io_fallible: false,
        owned_result_resources: Vec::new(),
        primary_fault_recorded: false,
        fault_code: String::new(),
        fault_message: String::new(),
        fault_detail: String::new(),
        witness_slots: Box::new([]),
        witness_arena: WitnessArena::default(),
        witnesses_captured: true,
        typed: Some(typed),
        pending_owner,
    });
    let pointer = &raw mut *task;
    executor_ref.tasks.push(task);
    pointer
}

#[unsafe(export_name = "loom_typed_task_frame_v1")]
pub unsafe extern "C" fn typed_task_frame_v1(task: *mut LoomTask) -> *mut c_void {
    if task.is_null() {
        return ptr::null_mut();
    }
    let task = unsafe { &*task };
    if task.status != TaskStatus::Unpublished
        || task.executor.is_null()
        || unsafe { (*task.executor).cleanup_active() }
    {
        return ptr::null_mut();
    }
    task.typed
        .as_ref()
        .map_or(ptr::null_mut(), TypedTaskStorage::frame_pointer)
}

#[unsafe(export_name = "loom_typed_task_initialize_v1")]
pub unsafe extern "C" fn typed_task_initialize_v1(task: *mut LoomTask, root_state: u64) -> i32 {
    let Some(task) = (unsafe { task.as_mut() }) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let Some(typed) = task.typed.as_mut() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let Ok(root_state) = usize::try_from(root_state) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if task.executor.is_null()
        || unsafe { (*task.executor).cleanup_active() }
        || task.status != TaskStatus::Unpublished
        || typed.initialized
        || root_state >= typed.root_state_count
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    typed.root_state = root_state;
    typed.initialized = true;
    TYPED_TASK_OK
}

#[unsafe(export_name = "loom_typed_task_publish_v1")]
pub unsafe extern "C" fn typed_task_publish_v1(
    executor: *mut LoomExecutor,
    task: *mut LoomTask,
) -> i32 {
    if executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, task)
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let task_ref = unsafe { &mut *task };
    let Some(typed) = task_ref.typed.as_mut() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if task_ref.status != TaskStatus::Unpublished || !typed.initialized || typed.published {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let executor_ref = unsafe { &mut *executor };
    let owner = task_ref.pending_owner;
    if !owner.is_null()
        && (executor_ref.active_task != owner
            || !executor_owns(executor_ref, owner)
            || unsafe { (*owner).status } != TaskStatus::Running
            || unsafe { (*owner).cancel_requested })
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    task_ref.pending_owner = ptr::null_mut();
    task_ref.owner = owner;
    typed.published = true;
    task_ref.status = TaskStatus::Runnable;
    if !owner.is_null() {
        unsafe { (*owner).owned_children.push(task) };
    }
    unsafe { enqueue_task(executor_ref, task) };
    TYPED_TASK_OK
}

/// Atomically publishes an initialized typed composite after transferring
/// typed children from the currently running structured parent.
///
/// Every pointer and ownership edge is validated before storage is reserved.
/// All fallible reservations then finish before the first topology mutation,
/// so any error leaves Task fields, the ready queue, and executor Task order
/// unchanged. The source array is copied and never retained.
#[unsafe(export_name = "loom_typed_task_publish_adopting_v1")]
#[allow(clippy::too_many_lines)]
pub unsafe extern "C" fn typed_task_publish_adopting_v1(
    executor: *mut LoomExecutor,
    composite: *mut LoomTask,
    children: *const *mut LoomTask,
    count: u64,
) -> i32 {
    if executor.is_null() || unsafe { (*executor).cleanup_active() } {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let executor_ref = unsafe { &mut *executor };
    let Ok(count) = usize::try_from(count) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if count == 0
        || count > executor_ref.tasks.len()
        || !is_aligned_for(children)
        || !executor_owns(executor_ref, composite)
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }

    let parent = executor_ref.active_task;
    if parent.is_null()
        || parent == composite
        || !executor_owns(executor_ref, parent)
        || unsafe { (*parent).executor } != executor
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let parent_ref = unsafe { &*parent };
    let Some(parent_typed) = parent_ref.typed.as_ref() else {
        // This ABI is deliberately unavailable to the universal Task path.
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if parent_ref.status != TaskStatus::Running
        || parent_ref.cancel_requested
        || !parent_ref.pending_owner.is_null()
        || parent_ref.join_active
        || !parent_ref.join_children.is_empty()
        || !parent_typed.initialized
        || !parent_typed.published
        || parent_typed.join_completion_pending
        || parent_typed.join_cancel_authorized
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }

    let composite_ref = unsafe { &*composite };
    let Some(composite_typed) = composite_ref.typed.as_ref() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if composite_ref.executor != executor
        || composite_ref.status != TaskStatus::Unpublished
        || !composite_ref.owner.is_null()
        || composite_ref.pending_owner != parent
        || composite_ref.queued
        || composite_ref.cancel_requested
        || composite_ref.join_active
        || !composite_ref.owned_children.is_empty()
        || !composite_ref.join_children.is_empty()
        || !composite_ref.waits.is_empty()
        || executor_ref
            .runnable
            .iter()
            .any(|candidate| *candidate == composite)
        || executor_ref.retired_tasks.contains(&composite)
        || !composite_typed.initialized
        || composite_typed.published
        || composite_typed.result_initialized
        || composite_typed.result_taken
        || composite_typed.result_disposed
        || composite_typed.cancel_invoked
        || composite_typed.dispose_invoked
        || composite_typed.join_completion_pending
        || composite_typed.join_cancel_authorized
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }

    // SAFETY: this unsafe ABI requires the bounded, aligned pointer array to
    // remain readable for the call. Count is additionally bounded by the
    // executor's live Task count above.
    let source_children = unsafe { slice::from_raw_parts(children, count) };
    if count > parent_ref.owned_children.len() {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    for (index, child) in source_children.iter().copied().enumerate() {
        if child.is_null()
            || child == parent
            || child == composite
            || source_children[..index].contains(&child)
            || !executor_owns(executor_ref, child)
        {
            return TYPED_TASK_INVALID_ARGUMENT;
        }
        let child_ref = unsafe { &*child };
        let Some(child_typed) = child_ref.typed.as_ref() else {
            return TYPED_TASK_INVALID_ARGUMENT;
        };
        let owned_memberships = parent_ref
            .owned_children
            .iter()
            .filter(|candidate| **candidate == child)
            .count();
        if child_ref.executor != executor
            || child_ref.owner != parent
            || !child_ref.pending_owner.is_null()
            || matches!(
                child_ref.status,
                TaskStatus::Unpublished | TaskStatus::Running
            )
            || owned_memberships != 1
            || !child_typed.initialized
            || !child_typed.published
            || child_typed.result_taken
            || child_typed.result_disposed
            || child_typed.dispose_invoked
            || (child_ref.status == TaskStatus::Completed && !child_typed.result_initialized)
        {
            return TYPED_TASK_INVALID_ARGUMENT;
        }
    }

    let Some(parent_count) = parent_ref
        .owned_children
        .len()
        .checked_sub(count)
        .and_then(|remaining| remaining.checked_add(1))
    else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let mut next_parent_children = Vec::new();
    if next_parent_children
        .try_reserve_exact(parent_count)
        .is_err()
    {
        return TYPED_TASK_NO_MEMORY;
    }
    for child in parent_ref.owned_children.iter().copied() {
        if !source_children.contains(&child) {
            next_parent_children.push(child);
        }
    }
    next_parent_children.push(composite);

    let mut adopted_children = Vec::new();
    if adopted_children.try_reserve_exact(count).is_err() {
        return TYPED_TASK_NO_MEMORY;
    }
    adopted_children.extend_from_slice(source_children);

    // Stage the complete transferred subtrees in input order. The Box handles
    // are reordered during commit so shutdown's reverse walk agrees with the
    // structured reverse-input cleanup order even when callers reorder Task
    // values relative to their original creation order.
    let mut adopted_subtree = Vec::new();
    if adopted_subtree
        .try_reserve_exact(executor_ref.tasks.len())
        .is_err()
    {
        return TYPED_TASK_NO_MEMORY;
    }
    for child in &adopted_children {
        for candidate in &executor_ref.tasks {
            let pointer = (&raw const **candidate).cast_mut();
            let mut ancestor = pointer;
            let mut belongs = false;
            for _ in 0..executor_ref.tasks.len() {
                if ancestor == *child {
                    belongs = true;
                    break;
                }
                if ancestor.is_null() || !executor_owns(executor_ref, ancestor) {
                    break;
                }
                ancestor = unsafe { (*ancestor).owner };
            }
            if belongs {
                adopted_subtree.push(pointer);
            }
        }
    }
    let mut next_task_order = Vec::new();
    if next_task_order
        .try_reserve_exact(executor_ref.tasks.len())
        .is_err()
        || executor_ref.runnable.try_reserve(1).is_err()
    {
        return TYPED_TASK_NO_MEMORY;
    }
    for task in &executor_ref.tasks {
        let pointer = (&raw const **task).cast_mut();
        if pointer != composite && !adopted_subtree.contains(&pointer) {
            next_task_order.push(pointer);
        }
    }
    next_task_order.push(composite);
    next_task_order.extend_from_slice(&adopted_subtree);
    if next_task_order.len() != executor_ref.tasks.len()
        || executor_ref.tasks.iter().any(|task| {
            let pointer = (&raw const **task).cast_mut();
            next_task_order
                .iter()
                .filter(|candidate| **candidate == pointer)
                .count()
                != 1
        })
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }

    // Commit begins here. Every remaining operation is allocation-free and
    // infallible. Preserve the supplied child order in the new structured
    // owner, while replacing all selected parent edges with one composite.
    unsafe {
        (*parent).owned_children = next_parent_children;
        (*composite).owned_children = adopted_children;
        for child in &(*composite).owned_children {
            (**child).owner = composite;
        }
        (*composite).pending_owner = ptr::null_mut();
        (*composite).owner = parent;
        if let Some(typed) = (*composite).typed.as_mut() {
            typed.published = true;
        }
        (*composite).status = TaskStatus::Runnable;
    }

    // Task frames normally appear before their children in `tasks`, so the
    // shutdown reverse walk destroys children first. A first-class composite
    // is constructed after its operands. Keep unrelated Tasks in their stable
    // order, then place the composite before complete adopted subtrees grouped
    // in caller-supplied order. Moving Box handles never moves LoomTask frames.
    for (target, pointer) in next_task_order.into_iter().enumerate() {
        let mut source = target;
        for index in target..executor_ref.tasks.len() {
            if ptr::eq::<LoomTask>(&raw const *executor_ref.tasks[index], pointer) {
                source = index;
                break;
            }
        }
        executor_ref.tasks.swap(target, source);
    }
    unsafe { enqueue_task(executor_ref, composite) };
    TYPED_TASK_OK
}

/// Retires and removes an unpublished frame. An initialized frame first runs
/// its non-suspending cancellation cleanup exactly once; cleanup failure is
/// reported as `TYPED_TASK_CLEANUP_FAULTED`, but the frame is still removed.
#[unsafe(export_name = "loom_typed_task_abort_unpublished_v1")]
pub unsafe extern "C" fn typed_task_abort_unpublished_v1(
    executor: *mut LoomExecutor,
    task: *mut LoomTask,
) -> i32 {
    if executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, task)
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let task_ref = unsafe { &*task };
    let Some(typed) = task_ref.typed.as_ref() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if task_ref.status != TaskStatus::Unpublished || typed.published {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let executor_ref = unsafe { &mut *executor };
    let outcome = if typed.initialized {
        unsafe { retire_typed_frame(executor_ref, task) }
    } else {
        CleanupOutcome::Clean
    };
    let Some(index) = executor_ref
        .tasks
        .iter()
        .position(|candidate| ptr::eq::<LoomTask>(&raw const **candidate, task))
    else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    // Abort is not a hot path. Preserve creation order so executor shutdown's
    // reverse iteration remains a true child/newest-before-parent LIFO.
    executor_ref.tasks.remove(index);
    if outcome.is_clean() {
        TYPED_TASK_OK
    } else {
        TYPED_TASK_CLEANUP_FAULTED
    }
}

/// Creates and publishes a zero-root typed `Task[Unit]` backed by the existing
/// platform-neutral timer `WaitSource` and executor reactor.
#[unsafe(export_name = "loom_typed_timer_task_create_v1")]
pub unsafe extern "C" fn typed_timer_task_create_v1(
    executor: *mut LoomExecutor,
    deadline_ns: u64,
) -> *mut LoomTask {
    let descriptor = LoomTypedCoroutineDescriptor {
        abi_version: TYPED_TASK_ABI_VERSION,
        flags: 0,
        resume: Some(resume_typed_timer),
        cancel: Some(cancel_typed_timer),
        dispose_result: None,
        frame_size: 1,
        frame_align: 1,
        result_offset: 0,
        result_size: 0,
        result_align: 1,
        root_slot_count: 0,
        root_state_count: 1,
        root_bitmap_words: 0,
        root_offsets: ptr::null(),
        live_bitmaps: ptr::null(),
        completed_root_state: 0,
    };
    let task = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
    if task.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (*task).wait_leaf = true;
        (*task).wait_source = LoomWaitSource {
            abi_version: WAIT_ABI_VERSION,
            kind: WAIT_SOURCE_TIMER,
            handle: -1,
            interests: 0,
            reserved: 0,
            deadline_ns,
        };
    }
    if unsafe { typed_task_initialize_v1(task, 0) } != TYPED_TASK_OK {
        let _ = unsafe { typed_task_abort_unpublished_v1(executor, task) };
        return ptr::null_mut();
    }
    if unsafe { typed_task_publish_v1(executor, task) } != TYPED_TASK_OK {
        let _ = unsafe { typed_task_abort_unpublished_v1(executor, task) };
        return ptr::null_mut();
    }
    task
}

#[unsafe(export_name = "loom_typed_task_set_root_state_v1")]
pub unsafe extern "C" fn typed_task_set_root_state_v1(task: *mut LoomTask, root_state: u64) -> i32 {
    let task_pointer = task;
    let Some(task) = (unsafe { task.as_mut() }) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let Some(typed) = task.typed.as_mut() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let Ok(root_state) = usize::try_from(root_state) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let executor = task.executor;
    if !typed.initialized
        || !typed.published
        || typed.result_initialized
        || executor.is_null()
        || unsafe { (*executor).active_task } != task_pointer
        || task.status != TaskStatus::Running
        || root_state >= typed.root_state_count
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    typed.root_state = root_state;
    TYPED_TASK_OK
}

#[unsafe(export_name = "loom_typed_task_publish_result_v1")]
pub unsafe extern "C" fn typed_task_publish_result_v1(task: *mut LoomTask) -> i32 {
    let task_pointer = task;
    let Some(task) = (unsafe { task.as_mut() }) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let Some(typed) = task.typed.as_mut() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let executor = task.executor;
    if task.status != TaskStatus::Running
        || !typed.initialized
        || !typed.published
        || typed.result_initialized
        || task.cancel_requested
        || executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || unsafe { (*executor).active_task } != task_pointer
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    typed.result_initialized = true;
    typed.root_state = typed.completed_root_state;
    TYPED_TASK_OK
}

#[unsafe(export_name = "loom_typed_task_status_v1")]
pub unsafe extern "C" fn typed_task_status_v1(task: *const LoomTask) -> i32 {
    if task.is_null() || unsafe { (*task).typed.is_none() } {
        return TYPED_TASK_STATUS_INVALID;
    }
    match unsafe { (*task).status } {
        TaskStatus::Completed => TASK_COMPLETED,
        TaskStatus::Faulted => TASK_FAULTED,
        TaskStatus::Cancelled => TASK_CANCELLED,
        TaskStatus::Unpublished
        | TaskStatus::Runnable
        | TaskStatus::Running
        | TaskStatus::Waiting
        | TaskStatus::Draining => TASK_PENDING,
    }
}

#[unsafe(export_name = "loom_typed_task_is_cancel_requested_v1")]
pub unsafe extern "C" fn typed_task_is_cancel_requested_v1(task: *const LoomTask) -> i32 {
    i32::from(
        !task.is_null()
            && unsafe { (*task).typed.is_some() }
            && unsafe { (*task).cancel_requested },
    )
}

unsafe fn retire_typed_child(
    executor: &mut LoomExecutor,
    owner: *mut LoomTask,
    child: *mut LoomTask,
) {
    if owner.is_null() || !executor_owns(executor, owner) || !executor_owns(executor, child) {
        return;
    }
    unsafe {
        (*owner)
            .owned_children
            .retain(|candidate| *candidate != child);
        (*owner)
            .join_children
            .retain(|candidate| *candidate != child);
        (*child).owner = ptr::null_mut();
    }
    if !executor.retired_tasks.contains(&child) {
        executor.retired_tasks.push(child);
    }
}

unsafe fn transfer_result_resources(owner: *mut LoomTask, child: *mut LoomTask) {
    let resources = unsafe { std::mem::take(&mut (*child).owned_result_resources) };
    unsafe { (*owner).owned_result_resources.extend(resources) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedTaskTakeKind {
    Result,
    Outcome,
}

unsafe fn typed_child_take_ready(
    executor: &LoomExecutor,
    owner: *mut LoomTask,
    child: *mut LoomTask,
    take_kind: TypedTaskTakeKind,
) -> bool {
    !owner.is_null()
        && executor.active_task == owner
        && executor_owns(executor, owner)
        && executor_owns(executor, child)
        && unsafe { (*owner).status } == TaskStatus::Running
        && !unsafe { (*owner).join_active }
        && unsafe { (*owner).join_step } == TASK_COMPLETED
        && unsafe {
            (*owner)
                .owned_children
                .iter()
                .filter(|candidate| **candidate == child)
                .count()
                == 1
        }
        && unsafe {
            (*owner)
                .join_children
                .iter()
                .filter(|candidate| **candidate == child)
                .count()
                == 1
        }
        && match take_kind {
            TypedTaskTakeKind::Result => {
                matches!(unsafe { (*owner).join_mode }, TASK_JOIN_ALL | TASK_JOIN_ANY)
            }
            TypedTaskTakeKind::Outcome => matches!(
                unsafe { (*owner).join_mode },
                TASK_JOIN_SETTLED | TASK_JOIN_RACE
            ),
        }
        && (!matches!(
            unsafe { (*owner).join_mode },
            TASK_JOIN_ANY | TASK_JOIN_RACE
        ) || unsafe {
            (*owner)
                .typed
                .as_ref()
                .is_some_and(|typed| typed.join_winner_finalized)
        })
}

/// Moves a completed result out of its stable Task frame and consumes the
/// structured child handle. Size and alignment are repeated by the caller so
/// an ABI/layout disagreement fails before touching either storage location.
/// A non-root child must belong exactly once to a successfully settled
/// ALL/ANY join; ANY winner finalization must already be complete.
/// A child transfers its result-resource ledger to the active owner before
/// retirement. A root has no owner, so its ledger remains discoverable through
/// the executor-owned Task registry until explicit close or executor teardown.
#[unsafe(export_name = "loom_typed_task_take_result_v1")]
pub unsafe extern "C" fn typed_task_take_result_v1(
    task: *mut LoomTask,
    output: *mut c_void,
    output_size: u64,
    output_align: u64,
) -> i32 {
    let Some(task_ref) = (unsafe { task.as_mut() }) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let Some(typed) = task_ref.typed.as_mut() else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let (Ok(output_size), Ok(output_align)) =
        (usize::try_from(output_size), usize::try_from(output_align))
    else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    if task_ref.status != TaskStatus::Completed
        || !typed.result_initialized
        || typed.result_taken
        || typed.result_disposed
        || typed.dispose_invoked
        || output_size != typed.result_size
        || output_align != typed.result_align
        || (output_size != 0
            && (output.is_null() || !(output as usize).is_multiple_of(output_align)))
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    if output_size != 0 {
        let output_start = output as usize;
        let source_start = typed.result_pointer() as usize;
        let (Some(output_end), Some(source_end)) = (
            output_start.checked_add(output_size),
            source_start.checked_add(output_size),
        ) else {
            return TYPED_TASK_INVALID_ARGUMENT;
        };
        if output_start < source_end && source_start < output_end {
            return TYPED_TASK_INVALID_ARGUMENT;
        }
    }
    let executor = task_ref.executor;
    if executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, task)
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let owner = task_ref.owner;
    if !owner.is_null()
        && !unsafe { typed_child_take_ready(&*executor, owner, task, TypedTaskTakeKind::Result) }
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    if output_size != 0 {
        // SAFETY: both nonoverlapping ranges have the exact validated size and
        // alignment. Task frames are non-GC storage, and `output` is borrowed
        // by this call only.
        unsafe {
            ptr::copy_nonoverlapping(typed.result_pointer(), output.cast(), output_size);
            ptr::write_bytes(typed.result_pointer(), 0, output_size);
        }
    }
    typed.result_initialized = false;
    typed.result_taken = true;
    if !owner.is_null() {
        unsafe { transfer_result_resources(owner, task) };
        unsafe { retire_typed_child(&mut *executor, owner, task) };
    }
    TYPED_TASK_OK
}

/// Consumes one terminal typed child and publishes the payload for its exact
/// `TaskOutcome[T]` case. Completed values are moved byte-for-byte from the
/// descriptor-pinned result slot. Fault text is copied into two independently
/// managed `Text` values while the first allocation is precisely rooted.
///
/// The caller must keep all output cells in stable, non-GC storage for the
/// complete call. Contract validation is a preflight and does not modify caller
/// storage. On success the child is detached from its active join and retired;
/// the child must belong to a settled SETTLED/RACE join, with RACE winner
/// finalization complete;
/// only a Completed outcome first transfers its result-resource ledger to the
/// active owner.
#[unsafe(export_name = "loom_typed_task_take_outcome_v1")]
#[allow(clippy::too_many_lines)]
pub unsafe extern "C" fn typed_task_take_outcome_v1(
    task: *mut LoomTask,
    value_output: *mut c_void,
    value_size: u64,
    value_align: u64,
    code_output: *mut *mut c_void,
    message_output: *mut *mut c_void,
) -> i32 {
    let Some(task_ref) = (unsafe { task.as_ref() }) else {
        return TYPED_TASK_STATUS_INVALID;
    };
    let Some(typed) = task_ref.typed.as_ref() else {
        return TYPED_TASK_STATUS_INVALID;
    };
    let (Ok(value_size), Ok(value_align)) =
        (usize::try_from(value_size), usize::try_from(value_align))
    else {
        return TYPED_TASK_STATUS_INVALID;
    };
    let pointer_size = size_of::<*mut c_void>();
    let pointer_align = align_of::<*mut c_void>();
    if !terminal(task_ref.status)
        || !typed.initialized
        || !typed.published
        || typed.result_taken
        || typed.result_disposed
        || typed.dispose_invoked
        || value_size != typed.result_size
        || value_align != typed.result_align
        || (value_size != 0
            && (value_output.is_null() || !(value_output as usize).is_multiple_of(value_align)))
        || code_output.is_null()
        || message_output.is_null()
        || !(code_output as usize).is_multiple_of(pointer_align)
        || !(message_output as usize).is_multiple_of(pointer_align)
        || code_output == message_output
        || (task_ref.status == TaskStatus::Completed && !typed.result_initialized)
        || (task_ref.status != TaskStatus::Completed && typed.result_initialized)
        || (task_ref.status == TaskStatus::Faulted && !task_ref.primary_fault_recorded)
    {
        return TYPED_TASK_STATUS_INVALID;
    }

    let checked_overlap = |left_start: usize,
                           left_size: usize,
                           right_start: usize,
                           right_size: usize|
     -> Option<bool> {
        let left_end = left_start.checked_add(left_size)?;
        let right_end = right_start.checked_add(right_size)?;
        Some(left_start < right_end && right_start < left_end)
    };
    let value_start = value_output as usize;
    let source_start = typed.result_pointer() as usize;
    let frame_start = typed.frame.as_ptr() as usize;
    let frame_size = typed.frame_layout.size();
    let code_start = code_output as usize;
    let message_start = message_output as usize;
    let ranges_valid = checked_overlap(code_start, pointer_size, message_start, pointer_size)
        == Some(false)
        && checked_overlap(code_start, pointer_size, frame_start, frame_size) == Some(false)
        && checked_overlap(message_start, pointer_size, frame_start, frame_size) == Some(false)
        && (value_size == 0
            || (checked_overlap(value_start, value_size, source_start, value_size) == Some(false)
                && checked_overlap(value_start, value_size, frame_start, frame_size)
                    == Some(false)
                && checked_overlap(value_start, value_size, code_start, pointer_size)
                    == Some(false)
                && checked_overlap(value_start, value_size, message_start, pointer_size)
                    == Some(false)));
    if !ranges_valid {
        return TYPED_TASK_STATUS_INVALID;
    }

    let executor = task_ref.executor;
    let owner = task_ref.owner;
    if executor.is_null()
        || owner.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !unsafe { typed_child_take_ready(&*executor, owner, task, TypedTaskTakeKind::Outcome) }
    {
        return TYPED_TASK_STATUS_INVALID;
    }

    // Invalid calls never clear caller storage. Once the complete contract is
    // known-valid, every non-faulted outcome publishes null Text payloads.
    unsafe {
        code_output.write(ptr::null_mut());
        message_output.write(ptr::null_mut());
    }
    let outcome = match task_ref.status {
        TaskStatus::Completed => {
            if value_size != 0 {
                // SAFETY: descriptor equality and the non-overlap checks above
                // establish exact, disjoint source and destination ranges.
                unsafe {
                    ptr::copy_nonoverlapping(
                        typed.result_pointer(),
                        value_output.cast(),
                        value_size,
                    );
                    ptr::write_bytes(typed.result_pointer(), 0, value_size);
                }
            }
            let Some(typed) = (unsafe { (*task).typed.as_mut() }) else {
                std::process::abort();
            };
            typed.result_initialized = false;
            typed.result_taken = true;
            TASK_COMPLETED
        }
        TaskStatus::Faulted => {
            let allocation_status = unsafe {
                crate::text::allocate_typed_text_pair(
                    task_ref.fault_code.as_bytes(),
                    task_ref.fault_message.as_bytes(),
                    code_output,
                    message_output,
                )
            };
            if allocation_status != GC_OK {
                // A validated generated-code call has an active runtime and
                // bounded scheduler-owned UTF-8. No recoverable GC status can
                // arise here without violating that internal contract.
                std::process::abort();
            }
            TASK_FAULTED
        }
        TaskStatus::Cancelled => TASK_CANCELLED,
        TaskStatus::Unpublished
        | TaskStatus::Runnable
        | TaskStatus::Running
        | TaskStatus::Waiting
        | TaskStatus::Draining => unreachable!("terminal status was validated above"),
    };
    if outcome == TASK_COMPLETED {
        unsafe { transfer_result_resources(owner, task) };
    }
    unsafe { retire_typed_child(&mut *executor, owner, task) };
    outcome
}

#[unsafe(export_name = "loom_typed_task_request_cancel_v1")]
pub unsafe extern "C" fn typed_task_request_cancel_v1(
    executor: *mut LoomExecutor,
    task: *mut LoomTask,
) -> i32 {
    if executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, task)
        || unsafe { (*task).typed.as_ref() }.is_none_or(|typed| !typed.published)
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    unsafe { request_cancel(&mut *executor, task) };
    TYPED_TASK_OK
}

unsafe fn copy_typed_fault_text(data: *const u8, length: u64) -> Result<String, i32> {
    if length > TYPED_TASK_MAX_FAULT_TEXT_BYTES {
        return Err(TYPED_TASK_INVALID_ARGUMENT);
    }
    let length = usize::try_from(length).map_err(|_| TYPED_TASK_INVALID_ARGUMENT)?;
    if data.is_null() && length != 0 {
        return Err(TYPED_TASK_INVALID_ARGUMENT);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| TYPED_TASK_NO_MEMORY)?;
    if length != 0 {
        // SAFETY: the unsafe ABI requires this bounded byte range to remain
        // readable for the call. A u8 pointer has no stronger alignment.
        bytes.extend_from_slice(unsafe { slice::from_raw_parts(data, length) });
    }
    String::from_utf8(bytes).map_err(|_| TYPED_TASK_INVALID_ARGUMENT)
}

/// Copies one primary typed fault. Later cleanup failures never overwrite the
/// original code/message/detail triplet.
#[unsafe(export_name = "loom_typed_task_record_fault_v1")]
pub unsafe extern "C" fn typed_task_record_fault_v1(
    task: *mut LoomTask,
    code: *const u8,
    code_length: u64,
    message: *const u8,
    message_length: u64,
    detail: *const u8,
    detail_length: u64,
) -> i32 {
    let Some(task_ref) = (unsafe { task.as_mut() }) else {
        return TYPED_TASK_INVALID_ARGUMENT;
    };
    let executor = task_ref.executor;
    if task_ref.typed.is_none() || executor.is_null() || unsafe { (*executor).active_task } != task
    {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let code = match unsafe { copy_typed_fault_text(code, code_length) } {
        Ok(code) => code,
        Err(status) => return status,
    };
    let message = match unsafe { copy_typed_fault_text(message, message_length) } {
        Ok(message) => message,
        Err(status) => return status,
    };
    let detail = match unsafe { copy_typed_fault_text(detail, detail_length) } {
        Ok(detail) => detail,
        Err(status) => return status,
    };
    record_primary_task_fault(task_ref, code, message, detail);
    TYPED_TASK_OK
}

fn byte_view(value: &str) -> LoomByteView {
    LoomByteView {
        data: if value.is_empty() {
            ptr::null()
        } else {
            value.as_ptr()
        },
        length: value.len() as u64,
    }
}

#[unsafe(export_name = "loom_typed_task_fault_view_v1")]
pub unsafe extern "C" fn typed_task_fault_view_v1(
    task: *const LoomTask,
    output: *mut LoomTypedTaskFaultView,
) -> i32 {
    if task.is_null() || !is_aligned_for(output) {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    let task = unsafe { &*task };
    if task.typed.is_none() || !task.primary_fault_recorded {
        return TYPED_TASK_INVALID_ARGUMENT;
    }
    unsafe {
        output.write(LoomTypedTaskFaultView {
            code: byte_view(&task.fault_code),
            message: byte_view(&task.fault_message),
            detail: byte_view(&task.fault_detail),
        });
    }
    TYPED_TASK_OK
}

#[unsafe(export_name = "loom_task_spawn")]
pub unsafe extern "C" fn task_spawn(
    executor: *mut LoomExecutor,
    resume: Option<LoomTaskResume>,
    slot_count: u64,
    result_slot: u64,
) -> *mut LoomTask {
    let descriptor = LoomCoroutineDescriptor {
        abi_version: COROUTINE_ABI_VERSION,
        flags: 0,
        resume,
        cancel: None,
        trace: None,
        slot_count,
        witness_count: 0,
        result_slot,
        state_count: 0,
        live_bitmap_words: 0,
        live_bitmaps: ptr::null(),
    };
    unsafe { task_spawn_descriptor(executor, &raw const descriptor) }
}

#[unsafe(export_name = "loom_task_spawn_descriptor")]
pub unsafe extern "C" fn task_spawn_descriptor(
    executor: *mut LoomExecutor,
    descriptor: *const LoomCoroutineDescriptor,
) -> *mut LoomTask {
    if executor.is_null() || unsafe { (*executor).cleanup_active() } {
        return ptr::null_mut();
    }
    let Some(mut descriptor) = (unsafe { descriptor.as_ref() }).copied() else {
        return ptr::null_mut();
    };
    let (Ok(slot_count), Ok(witness_count), Ok(result_slot), Some(_)) = (
        usize::try_from(descriptor.slot_count),
        usize::try_from(descriptor.witness_count),
        usize::try_from(descriptor.result_slot),
        descriptor.resume,
    ) else {
        return ptr::null_mut();
    };
    let bitmap_layout_valid = descriptor.live_bitmaps.is_null()
        || (descriptor.state_count > 0
            && descriptor.live_bitmap_words >= descriptor.slot_count.div_ceil(64)
            && descriptor
                .state_count
                .checked_mul(descriptor.live_bitmap_words)
                .is_some());
    if descriptor.abi_version != COROUTINE_ABI_VERSION
        || slot_count == 0
        || result_slot >= slot_count
        || !bitmap_layout_valid
    {
        return ptr::null_mut();
    }
    if descriptor.cancel.is_none() {
        descriptor.cancel = descriptor.resume;
    }
    if descriptor.trace.is_none() {
        descriptor.trace = Some(task_trace_live_slots);
    }
    // SAFETY: non-null executor is uniquely driven on this thread.
    let executor_ref = unsafe { &mut *executor };
    let owner = executor_ref.active_task;
    if !owner.is_null()
        && (!executor_owns(executor_ref, owner)
            || unsafe { (*owner).status } != TaskStatus::Running
            || unsafe { (*owner).cancel_requested })
    {
        return ptr::null_mut();
    }
    let mut task = Box::new(LoomTask {
        descriptor,
        slots: vec![ValueSlot::default(); slot_count].into_boxed_slice(),
        result_slot,
        state: 0,
        executor,
        owner,
        owned_children: Vec::new(),
        join_children: Vec::new(),
        waits: Vec::new(),
        status: TaskStatus::Runnable,
        deferred_terminal: TASK_PENDING,
        join_mode: TASK_JOIN_ALL,
        join_winner: NO_JOIN_WINNER,
        join_step: TASK_COMPLETED,
        queued: false,
        cancel_requested: false,
        join_active: false,
        wait_leaf: false,
        wait_source: LoomWaitSource::default(),
        composite_spec: ptr::null_mut(),
        io_operation: None,
        blocking_result: None,
        io_fallible: false,
        owned_result_resources: Vec::new(),
        primary_fault_recorded: false,
        fault_code: "TaskFault".into(),
        fault_message: "task execution failed".into(),
        fault_detail: String::new(),
        witness_slots: vec![ptr::null(); witness_count].into_boxed_slice(),
        witness_arena: WitnessArena::default(),
        witnesses_captured: witness_count == 0,
        typed: None,
        pending_owner: ptr::null_mut(),
    });
    let pointer = &raw mut *task;
    executor_ref.tasks.push(task);
    if !owner.is_null() {
        unsafe { (*owner).owned_children.push(pointer) };
    }
    unsafe { enqueue_task(executor_ref, pointer) };
    pointer
}

/// Atomically captures every hidden proof parameter into Task-owned,
/// non-moving storage.
///
/// `count` must equal the coroutine descriptor's `witness_count`. The source
/// array and every proof reachable from it only need to remain live for this
/// call; no source address is retained as a cache key afterwards.
#[unsafe(export_name = "loom_task_capture_witnesses_v1")]
pub unsafe extern "C" fn task_capture_witnesses_v1(
    task: *mut LoomTask,
    sources: *const *const LoomWitnessInstance,
    count: u64,
) -> i32 {
    let Some(task) = (unsafe { task.as_mut() }) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let Ok(count) = usize::try_from(count) else {
        return WAIT_INVALID_ARGUMENT;
    };
    if task.executor.is_null()
        || unsafe { (*task.executor).cleanup_active() }
        || task.witnesses_captured
        || task.status != TaskStatus::Runnable
        || task.state != 0
        || count != task.witness_slots.len()
        || (count != 0 && sources.is_null())
    {
        return WAIT_INVALID_ARGUMENT;
    }
    let sources = if count == 0 {
        &[][..]
    } else {
        unsafe { slice::from_raw_parts(sources, count) }
    };
    let Some(staged) = (unsafe { clone_witnesses(sources) }) else {
        return WAIT_INVALID_ARGUMENT;
    };
    task.witness_slots = task.witness_arena.adopt(staged);
    task.witnesses_captured = true;
    WAIT_OK
}

/// Returns one Task-owned hidden proof parameter by its checked dense index.
#[unsafe(export_name = "loom_task_witness_v1")]
pub unsafe extern "C" fn task_witness_v1(
    task: *const LoomTask,
    index: u64,
) -> *const LoomWitnessInstance {
    let Some(task) = (unsafe { task.as_ref() }) else {
        return ptr::null();
    };
    let Ok(index) = usize::try_from(index) else {
        return ptr::null();
    };
    if !task.witnesses_captured {
        return ptr::null();
    }
    task.witness_slots
        .get(index)
        .copied()
        .unwrap_or(ptr::null())
}

unsafe fn spawn_io_task(executor: *mut LoomExecutor, operation: IoOperation) -> *mut LoomTask {
    unsafe { spawn_io_task_with_mode(executor, operation, false) }
}

unsafe fn spawn_try_io_task(executor: *mut LoomExecutor, operation: IoOperation) -> *mut LoomTask {
    unsafe { spawn_io_task_with_mode(executor, operation, true) }
}

unsafe fn spawn_io_task_with_mode(
    executor: *mut LoomExecutor,
    operation: IoOperation,
    fallible: bool,
) -> *mut LoomTask {
    let task = unsafe { task_spawn(executor, Some(resume_io), 1, 0) };
    if !task.is_null() {
        unsafe {
            (*task).io_operation = Some(operation);
            (*task).io_fallible = fallible;
        }
    }
    task
}

unsafe fn spawn_io_error_task(
    executor: *mut LoomExecutor,
    kind: u32,
    code: &'static str,
    message: impl Into<String>,
) -> *mut LoomTask {
    unsafe { spawn_io_failure_task(executor, true, kind, code, message) }
}

unsafe fn spawn_io_failure_task(
    executor: *mut LoomExecutor,
    fallible: bool,
    kind: u32,
    code: &'static str,
    message: impl Into<String>,
) -> *mut LoomTask {
    let task = unsafe { task_spawn(executor, Some(resume_io), 1, 0) };
    if !task.is_null() {
        unsafe {
            (*task).io_fallible = fallible;
            (*task).blocking_result = Some(BlockingResult::Fault {
                code,
                kind,
                message: message.into(),
            });
        }
    }
    task
}

#[cfg(test)]
unsafe extern "C" fn resume_slow_blocking_fixture(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
) -> i32 {
    if unsafe { (*task).cancel_requested } {
        return TASK_CANCELLED;
    }
    if unsafe { (*task).blocking_result.take().is_some() } {
        return unsafe { store_unit_result(task) };
    }
    unsafe {
        suspend_blocking(task, executor, || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            BlockingResult::Unit
        })
    }
}

#[cfg(test)]
pub(crate) unsafe fn spawn_slow_blocking_fixture(executor: *mut LoomExecutor) -> *mut LoomTask {
    unsafe { task_spawn(executor, Some(resume_slow_blocking_fixture), 1, 0) }
}

unsafe fn copy_text(data: *const u8, length: u64) -> Option<String> {
    let length = usize::try_from(length).ok()?;
    if data.is_null() && length != 0 {
        return None;
    }
    let bytes = if length == 0 {
        &[]
    } else {
        // SAFETY: generated Text passes its live byte pointer and exact length.
        unsafe { slice::from_raw_parts(data, length) }
    };
    std::str::from_utf8(bytes).ok().map(str::to_owned)
}

fn checked_resource_handle(handle: i64) -> Option<i64> {
    (handle != INVALID_HANDLE).then_some(handle)
}

#[unsafe(export_name = "loom_file_open_read")]
pub unsafe extern "C" fn file_open_read(
    executor: *mut LoomExecutor,
    path: *const u8,
    path_length: u64,
) -> *mut LoomTask {
    let Some(path) = (unsafe { copy_text(path, path_length) }) else {
        return ptr::null_mut();
    };
    unsafe {
        spawn_io_task(
            executor,
            IoOperation::FileOpen {
                path,
                create: false,
            },
        )
    }
}

#[unsafe(export_name = "loom_file_create")]
pub unsafe extern "C" fn file_create(
    executor: *mut LoomExecutor,
    path: *const u8,
    path_length: u64,
) -> *mut LoomTask {
    let Some(path) = (unsafe { copy_text(path, path_length) }) else {
        return ptr::null_mut();
    };
    unsafe { spawn_io_task(executor, IoOperation::FileOpen { path, create: true }) }
}

#[unsafe(export_name = "loom_file_try_open_read")]
pub unsafe extern "C" fn file_try_open_read(
    executor: *mut LoomExecutor,
    path: *const u8,
    path_length: u64,
) -> *mut LoomTask {
    let Some(path) = (unsafe { copy_text(path, path_length) }) else {
        return unsafe {
            spawn_io_error_task(
                executor,
                3,
                "FileOpenFault",
                "file path is not valid UTF-8 Text",
            )
        };
    };
    unsafe {
        spawn_try_io_task(
            executor,
            IoOperation::FileOpen {
                path,
                create: false,
            },
        )
    }
}

#[unsafe(export_name = "loom_file_try_create")]
pub unsafe extern "C" fn file_try_create(
    executor: *mut LoomExecutor,
    path: *const u8,
    path_length: u64,
) -> *mut LoomTask {
    let Some(path) = (unsafe { copy_text(path, path_length) }) else {
        return unsafe {
            spawn_io_error_task(
                executor,
                3,
                "FileCreateFault",
                "file path is not valid UTF-8 Text",
            )
        };
    };
    unsafe { spawn_try_io_task(executor, IoOperation::FileOpen { path, create: true }) }
}

#[unsafe(export_name = "loom_file_read_text")]
pub unsafe extern "C" fn file_read_text(executor: *mut LoomExecutor, handle: i64) -> *mut LoomTask {
    let Some(handle) = checked_resource_handle(handle) else {
        return unsafe {
            spawn_io_failure_task(
                executor,
                false,
                8,
                "FileReadFault",
                "file resource is closed",
            )
        };
    };
    let file = match duplicate_file(handle) {
        Ok(file) => file,
        Err(error) => unsafe {
            return spawn_io_failure_task(
                executor,
                false,
                io_error_kind(&error),
                "FileReadFault",
                error.to_string(),
            );
        },
    };
    unsafe { spawn_io_task(executor, IoOperation::FileRead { file }) }
}

#[unsafe(export_name = "loom_file_try_read_text")]
pub unsafe extern "C" fn file_try_read_text(
    executor: *mut LoomExecutor,
    handle: i64,
) -> *mut LoomTask {
    let Some(handle) = checked_resource_handle(handle) else {
        return unsafe {
            spawn_io_error_task(executor, 8, "FileReadFault", "file resource is closed")
        };
    };
    let file = match duplicate_file(handle) {
        Ok(file) => file,
        Err(error) => unsafe {
            return spawn_io_error_task(
                executor,
                io_error_kind(&error),
                "FileReadFault",
                error.to_string(),
            );
        },
    };
    unsafe { spawn_try_io_task(executor, IoOperation::FileRead { file }) }
}

#[unsafe(export_name = "loom_file_write_text")]
pub unsafe extern "C" fn file_write_text(
    executor: *mut LoomExecutor,
    handle: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let (Some(handle), Some(text)) = (checked_resource_handle(handle), unsafe {
        copy_text(data, length)
    }) else {
        return ptr::null_mut();
    };
    let file = match duplicate_file(handle) {
        Ok(file) => file,
        Err(error) => unsafe {
            return spawn_io_failure_task(
                executor,
                false,
                io_error_kind(&error),
                "FileWriteFault",
                error.to_string(),
            );
        },
    };
    unsafe {
        spawn_io_task(
            executor,
            IoOperation::FileWrite {
                file,
                bytes: text.into_bytes(),
            },
        )
    }
}

#[unsafe(export_name = "loom_file_try_write_text")]
pub unsafe extern "C" fn file_try_write_text(
    executor: *mut LoomExecutor,
    handle: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let Some(handle) = checked_resource_handle(handle) else {
        return unsafe {
            spawn_io_error_task(executor, 8, "FileWriteFault", "file resource is closed")
        };
    };
    let Some(text) = (unsafe { copy_text(data, length) }) else {
        return unsafe {
            spawn_io_error_task(
                executor,
                3,
                "FileWriteFault",
                "file contents are not valid UTF-8 Text",
            )
        };
    };
    let file = match duplicate_file(handle) {
        Ok(file) => file,
        Err(error) => unsafe {
            return spawn_io_error_task(
                executor,
                io_error_kind(&error),
                "FileWriteFault",
                error.to_string(),
            );
        },
    };
    unsafe {
        spawn_try_io_task(
            executor,
            IoOperation::FileWrite {
                file,
                bytes: text.into_bytes(),
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_connect")]
pub unsafe extern "C" fn socket_connect(
    executor: *mut LoomExecutor,
    host: *const u8,
    host_length: u64,
    port: i64,
) -> *mut LoomTask {
    let (Some(host), Ok(port)) = (unsafe { copy_text(host, host_length) }, u16::try_from(port))
    else {
        return ptr::null_mut();
    };
    unsafe { spawn_io_task(executor, IoOperation::SocketConnect { host, port }) }
}

#[unsafe(export_name = "loom_socket_try_connect")]
pub unsafe extern "C" fn socket_try_connect(
    executor: *mut LoomExecutor,
    host: *const u8,
    host_length: u64,
    port: i64,
) -> *mut LoomTask {
    let Some(host) = (unsafe { copy_text(host, host_length) }) else {
        return unsafe {
            spawn_io_error_task(
                executor,
                3,
                "SocketConnectFault",
                "socket host is not valid UTF-8 Text",
            )
        };
    };
    let Ok(port) = u16::try_from(port) else {
        return unsafe {
            spawn_io_error_task(
                executor,
                3,
                "SocketConnectFault",
                "socket port must be in 0..65535",
            )
        };
    };
    unsafe { spawn_try_io_task(executor, IoOperation::SocketConnect { host, port }) }
}

#[unsafe(export_name = "loom_socket_read_text")]
pub unsafe extern "C" fn socket_read_text(
    executor: *mut LoomExecutor,
    handle: i64,
) -> *mut LoomTask {
    let Some(handle) = checked_resource_handle(handle) else {
        return unsafe {
            spawn_io_failure_task(
                executor,
                false,
                8,
                "SocketReadFault",
                "socket resource is closed",
            )
        };
    };
    let socket = match duplicate_socket(handle) {
        Ok(socket) => socket,
        Err(error) => unsafe {
            return spawn_io_failure_task(
                executor,
                false,
                io_error_kind(&error),
                "SocketReadFault",
                error.to_string(),
            );
        },
    };
    unsafe {
        spawn_io_task(
            executor,
            IoOperation::SocketRead {
                socket,
                bytes: Vec::new(),
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_try_read_text")]
pub unsafe extern "C" fn socket_try_read_text(
    executor: *mut LoomExecutor,
    handle: i64,
) -> *mut LoomTask {
    let Some(handle) = checked_resource_handle(handle) else {
        return unsafe {
            spawn_io_error_task(executor, 8, "SocketReadFault", "socket resource is closed")
        };
    };
    let socket = match duplicate_socket(handle) {
        Ok(socket) => socket,
        Err(error) => unsafe {
            return spawn_io_error_task(
                executor,
                io_error_kind(&error),
                "SocketReadFault",
                error.to_string(),
            );
        },
    };
    unsafe {
        spawn_try_io_task(
            executor,
            IoOperation::SocketRead {
                socket,
                bytes: Vec::new(),
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_write_text")]
pub unsafe extern "C" fn socket_write_text(
    executor: *mut LoomExecutor,
    handle: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let (Some(handle), Some(text)) = (checked_resource_handle(handle), unsafe {
        copy_text(data, length)
    }) else {
        return ptr::null_mut();
    };
    let socket = match duplicate_socket(handle) {
        Ok(socket) => socket,
        Err(error) => unsafe {
            return spawn_io_failure_task(
                executor,
                false,
                io_error_kind(&error),
                "SocketWriteFault",
                error.to_string(),
            );
        },
    };
    unsafe {
        spawn_io_task(
            executor,
            IoOperation::SocketWrite {
                socket,
                bytes: text.into_bytes(),
                offset: 0,
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_try_write_text")]
pub unsafe extern "C" fn socket_try_write_text(
    executor: *mut LoomExecutor,
    handle: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let Some(handle) = checked_resource_handle(handle) else {
        return unsafe {
            spawn_io_error_task(executor, 8, "SocketWriteFault", "socket resource is closed")
        };
    };
    let Some(text) = (unsafe { copy_text(data, length) }) else {
        return unsafe {
            spawn_io_error_task(
                executor,
                3,
                "SocketWriteFault",
                "socket contents are not valid UTF-8 Text",
            )
        };
    };
    let socket = match duplicate_socket(handle) {
        Ok(socket) => socket,
        Err(error) => unsafe {
            return spawn_io_error_task(
                executor,
                io_error_kind(&error),
                "SocketWriteFault",
                error.to_string(),
            );
        },
    };
    unsafe {
        spawn_try_io_task(
            executor,
            IoOperation::SocketWrite {
                socket,
                bytes: text.into_bytes(),
                offset: 0,
            },
        )
    }
}

#[unsafe(export_name = "loom_io_close")]
pub unsafe extern "C" fn io_close(executor: *mut LoomExecutor, value: *mut c_void) -> i32 {
    let value = value.cast::<ValueSlot>();
    if executor.is_null()
        || value.is_null()
        || unsafe { (*value).words[0] } != VALUE_TAG_RECORD
        || unsafe { (*value).words[2] } != 1
    {
        return WAIT_INVALID_ARGUMENT;
    }
    let nominal = unsafe { (*value).words[1] };
    let Some(kind) = IoResourceKind::from_nominal(nominal) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let node = unsafe { (*value).words[4] as *mut ValueNode };
    if node.is_null() || unsafe { (*node).value.words[0] } != 2 {
        return WAIT_INVALID_ARGUMENT;
    }
    let handle = unsafe { (*node).value.words[3].cast_signed() };
    if handle == INVALID_HANDLE {
        return WAIT_OK;
    }
    let executor = unsafe { &mut *executor };
    if close_resource_handle(Some(executor), handle, kind).is_err() {
        return WAIT_INVALID_ARGUMENT;
    }
    unsafe { (*node).value.words[3] = INVALID_HANDLE.cast_unsigned() };
    WAIT_OK
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseResourceError {
    InvalidOwnership,
    CloseFailed,
}

fn select_tracked_resource(
    candidates: impl IntoIterator<Item = (usize, usize, bool)>,
) -> Result<Option<(usize, usize)>, CloseResourceError> {
    let mut exact = None;
    let mut opposite_seen = false;
    for (task_index, resource_index, kind_matches) in candidates {
        if kind_matches {
            if exact.replace((task_index, resource_index)).is_some() {
                return Err(CloseResourceError::InvalidOwnership);
            }
        } else {
            opposite_seen = true;
        }
    }
    match (exact, opposite_seen) {
        (Some(location), _) => Ok(Some(location)),
        (None, true) => Err(CloseResourceError::InvalidOwnership),
        (None, false) => Ok(None),
    }
}

fn close_resource_handle(
    executor: Option<&mut LoomExecutor>,
    handle: i64,
    kind: IoResourceKind,
) -> Result<(), CloseResourceError> {
    if let Some(executor) = executor {
        let candidates = executor
            .tasks
            .iter()
            .enumerate()
            .flat_map(|(task_index, task)| {
                task.owned_result_resources.iter().enumerate().filter_map(
                    move |(resource_index, candidate)| {
                        (candidate.handle_bits() == handle).then_some((
                            task_index,
                            resource_index,
                            candidate.is_file() == kind.is_file(),
                        ))
                    },
                )
            });
        if let Some((task_index, resource_index)) = select_tracked_resource(candidates)? {
            drop(
                executor.tasks[task_index]
                    .owned_result_resources
                    .swap_remove(resource_index),
            );
            return Ok(());
        }
    }
    // SAFETY: a well-formed externally transferred File/Socket value owns its
    // raw handle when it is no longer tracked by a runtime task. A handle still
    // present in any task ledger without a unique exact match was rejected
    // above, so this path cannot leave a second runtime owner behind. A unique
    // exact match is handled earlier even when an unrelated opposite-kind
    // Windows handle has the same numeric bits.
    unsafe { close_untracked(handle, kind.is_file()) }.map_err(|_| CloseResourceError::CloseFailed)
}

/// Closes one exact typed File/Socket record in place.
///
/// The current generated-code interval must already own `runtime`. This call
/// performs no scheduling and never constructs the legacy universal value
/// envelope. An attached executor is consulted only as an ownership registry
/// for a handle returned by an async IO task. An opposite-only match or
/// duplicate exact ledger entries fail before any close or ownership mutation;
/// a unique exact match wins across distinct Windows handle domains.
#[unsafe(export_name = "loom_runtime_resource_close_typed_v1")]
pub unsafe extern "C" fn resource_close_typed_v1(
    runtime: *mut LoomRuntime,
    kind: u32,
    handle: *mut i64,
) -> i32 {
    if runtime.is_null()
        || handle.is_null()
        || !(handle as usize).is_multiple_of(align_of::<i64>())
        || crate::gc::active_runtime_pointer() != runtime
    {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    }
    let Some(kind) = IoResourceKind::from_typed_kind(kind) else {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    };
    let current = unsafe { *handle };
    if current == INVALID_HANDLE {
        return TYPED_RESOURCE_CLOSE_OK;
    }
    // SAFETY: equality with the thread-local active runtime proves this is the
    // live runtime installed for the complete generated-code interval.
    let executor = unsafe { (*runtime).attached_executor_pointer() }.cast::<LoomExecutor>();
    let executor = if executor.is_null() {
        None
    } else {
        // SAFETY: runtime attachment owns this stable executor pointer and the
        // generated-code interval serializes runtime/executor mutation.
        let executor = unsafe { &mut *executor };
        if executor.runtime_pointer() != runtime {
            return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
        }
        Some(executor)
    };
    match close_resource_handle(executor, current, kind) {
        Ok(()) => {}
        Err(CloseResourceError::InvalidOwnership) => {
            return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
        }
        Err(CloseResourceError::CloseFailed) => return TYPED_RESOURCE_CLOSE_FAILED,
    }
    unsafe { *handle = INVALID_HANDLE };
    TYPED_RESOURCE_CLOSE_OK
}

#[unsafe(export_name = "loom_task_from_wait_source")]
pub unsafe extern "C" fn task_from_wait_source(
    executor: *mut LoomExecutor,
    source: *const LoomWaitSource,
) -> *mut LoomTask {
    if executor.is_null() || source.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: source is borrowed for this call only.
    let copied = unsafe { *source };
    if copied.abi_version != WAIT_ABI_VERSION
        || !matches!(copied.kind, WAIT_SOURCE_TIMER | WAIT_SOURCE_IO)
    {
        return ptr::null_mut();
    }
    // SAFETY: validated executor and internal callback/slot layout.
    let task = unsafe { task_spawn(executor, Some(resume_wait_leaf), 1, 0) };
    if !task.is_null() {
        unsafe {
            (*task).wait_leaf = true;
            (*task).wait_source = copied;
        }
    }
    task
}

#[unsafe(export_name = "loom_task_slot")]
pub unsafe extern "C" fn task_slot(task: *mut LoomTask, index: u64) -> *mut c_void {
    let Ok(index) = usize::try_from(index) else {
        return ptr::null_mut();
    };
    if task.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: task was checked above; this explicit reference avoids an
    // implicit raw-pointer autoref while inspecting the boxed-slice metadata.
    let slots = unsafe { &*ptr::addr_of!((*task).slots) };
    if index >= slots.len() {
        return ptr::null_mut();
    }
    unsafe { (&raw mut (*task).slots[index]).cast() }
}

#[unsafe(export_name = "loom_task_result")]
pub unsafe extern "C" fn task_result(task: *mut LoomTask) -> *mut c_void {
    if task.is_null() {
        return ptr::null_mut();
    }
    unsafe { task_slot(task, (*task).result_slot as u64) }
}

#[unsafe(export_name = "loom_task_state")]
pub unsafe extern "C" fn task_state(task: *const LoomTask) -> u64 {
    if task.is_null() {
        0
    } else {
        unsafe { (*task).state }
    }
}

#[unsafe(export_name = "loom_task_set_state")]
pub unsafe extern "C" fn task_set_state(task: *mut LoomTask, state: u64) {
    if !task.is_null() {
        unsafe { (*task).state = state };
    }
}

#[unsafe(export_name = "loom_task_is_cancelled")]
pub unsafe extern "C" fn task_is_cancelled(task: *const LoomTask) -> i32 {
    i32::from(!task.is_null() && unsafe { (*task).cancel_requested })
}

fn record_primary_task_fault(task: &mut LoomTask, code: String, message: String, detail: String) {
    if task.primary_fault_recorded {
        return;
    }
    task.primary_fault_recorded = true;
    task.fault_code = code;
    task.fault_message = message;
    task.fault_detail = detail;
}

fn inherit_primary_task_fault(parent: &mut LoomTask, child: &LoomTask) {
    if parent.primary_fault_recorded || !child.primary_fault_recorded {
        return;
    }
    record_primary_task_fault(
        parent,
        child.fault_code.clone(),
        child.fault_message.clone(),
        child.fault_detail.clone(),
    );
}

/// Stores the structured failure carried by a native task.
///
/// Generated coroutine code calls this before returning `TASK_FAULTED`.  The
/// scheduler deliberately does not print child failures: `Task.settled` and
/// `Task.race` must be able to consume them as ordinary values without an
/// observable side effect. The first recorded fault remains the task's primary
/// failure while lexical cleanup continues; later faults return success but do
/// not replace it. This ABI does not expose suppressed fault details.
#[unsafe(export_name = "loom_task_set_fault")]
pub unsafe extern "C" fn task_set_fault(
    task: *mut LoomTask,
    code: *const u8,
    code_length: u64,
    message: *const u8,
    message_length: u64,
) -> i32 {
    if task.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let (Some(code), Some(message)) = (unsafe { copy_text(code, code_length) }, unsafe {
        copy_text(message, message_length)
    }) else {
        return WAIT_INVALID_ARGUMENT;
    };
    // SAFETY: null was rejected above; scheduler ownership keeps the task live
    // throughout this call.
    unsafe { record_primary_task_fault(&mut *task, code, message, String::new()) };
    WAIT_OK
}

/// Reports an unhandled root-task failure exactly once at the executable
/// boundary. Child task failures are never routed through this function.
#[unsafe(export_name = "loom_task_report_fault")]
pub unsafe extern "C" fn task_report_fault(task: *const LoomTask) -> i32 {
    if task.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let task = unsafe { &*task };
    let code = if task.fault_code.is_empty() {
        TASK_FAULT_CODE
    } else {
        &task.fault_code
    };
    let message = if task.fault_message.is_empty() {
        TASK_FAULT_MESSAGE
    } else {
        &task.fault_message
    };
    report_fault(
        &task.fault_detail,
        code,
        message,
        &format!("{code}: {message}"),
    );
    WAIT_OK
}

#[derive(Clone, Copy)]
struct FaultArguments {
    code: *const u8,
    code_length: u64,
    message: *const u8,
    message_length: u64,
    display: *const u8,
    display_length: u64,
    detail: *const u8,
    detail_length: u64,
}

unsafe fn raise_fault_for_task_or_root(
    root_runtime: *mut LoomRuntime,
    active_task: *mut LoomTask,
    arguments: FaultArguments,
) -> i32 {
    if !active_task.is_null() {
        let (Some(code), Some(message), Some(detail)) = (
            unsafe { copy_text(arguments.code, arguments.code_length) },
            unsafe { copy_text(arguments.message, arguments.message_length) },
            unsafe { copy_text(arguments.detail, arguments.detail_length) },
        ) else {
            return WAIT_INVALID_ARGUMENT;
        };
        // SAFETY: the non-null task is owned by the executor validated by the
        // active runtime and remains live throughout this call.
        unsafe { record_primary_task_fault(&mut *active_task, code, message, detail) };
        return WAIT_OK;
    }
    if root_runtime.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }

    let (Some(code), Some(message), Some(display), Some(detail)) = (
        unsafe { copy_text(arguments.code, arguments.code_length) },
        unsafe { copy_text(arguments.message, arguments.message_length) },
        unsafe { copy_text(arguments.display, arguments.display_length) },
        unsafe { copy_text(arguments.detail, arguments.detail_length) },
    ) else {
        return WAIT_INVALID_ARGUMENT;
    };
    // SAFETY: context resolution proves this is the active runtime, either
    // directly or through its one attached live executor. The per-runtime
    // latch is reset only when a new outer generated-code interval begins.
    if !unsafe { (*root_runtime).latch_sync_fault() } {
        return WAIT_OK;
    }
    report_fault(&detail, &code, &message, &display);
    WAIT_OK
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultContextTarget {
    Root,
    Executor(*mut LoomExecutor),
}

fn resolve_fault_context(context: *mut c_void) -> Option<FaultContextTarget> {
    if context.is_null() {
        return None;
    }
    let runtime = active_runtime_pointer();
    if runtime.is_null() {
        return None;
    }
    if context == runtime.cast::<c_void>() {
        return Some(FaultContextTarget::Root);
    }
    // SAFETY: ACTIVE_RUNTIME contains only the stable runtime installed by
    // runtime/executor activation. The candidate context is compared as an
    // opaque identity and is not dereferenced here.
    if unsafe { (*runtime).is_attached_executor(context) } {
        Some(FaultContextTarget::Executor(context.cast()))
    } else {
        None
    }
}

/// Routes a generated-code fault through the currently active runtime.
///
/// A standalone runtime context is a synchronous executable boundary. An
/// attached executor context records the fault on its active task; when it has
/// no active task, it is also a synchronous boundary. No unvalidated context
/// pointer is dereferenced.
#[unsafe(export_name = "loom_context_raise_fault_v1")]
pub unsafe extern "C" fn context_raise_fault_v1(
    context: *mut c_void,
    code: *const u8,
    code_length: u64,
    message: *const u8,
    message_length: u64,
    display: *const u8,
    display_length: u64,
    detail: *const u8,
    detail_length: u64,
) -> i32 {
    let arguments = FaultArguments {
        code,
        code_length,
        message,
        message_length,
        display,
        display_length,
        detail,
        detail_length,
    };
    let (root_runtime, active_task) = match resolve_fault_context(context) {
        Some(FaultContextTarget::Root) => (active_runtime_pointer(), ptr::null_mut()),
        Some(FaultContextTarget::Executor(executor)) => {
            // SAFETY: resolution matched the candidate against the active
            // runtime's live attachment before converting it to an executor.
            unsafe { ((*executor).runtime_pointer(), (*executor).active_task) }
        }
        None => return WAIT_INVALID_ARGUMENT,
    };
    unsafe { raise_fault_for_task_or_root(root_runtime, active_task, arguments) }
}

/// Routes a generated-code fault whose structured detail contains a dynamic source span.
///
/// `detail_prefix` and `detail_suffix` are compiler-generated UTF-8 fragments surrounding the
/// serialized span. This keeps source locations exact across indirect calls without requiring a
/// JSON implementation in generated code. All byte ranges only need to remain live for this call.
#[allow(clippy::too_many_arguments)]
#[unsafe(export_name = "loom_context_raise_fault_with_span_v1")]
pub unsafe extern "C" fn context_raise_fault_with_span_v1(
    context: *mut c_void,
    code: *const u8,
    code_length: u64,
    message: *const u8,
    message_length: u64,
    display: *const u8,
    display_length: u64,
    detail_prefix: *const u8,
    detail_prefix_length: u64,
    file: u64,
    start: u64,
    end: u64,
    detail_suffix: *const u8,
    detail_suffix_length: u64,
) -> i32 {
    let (Some(prefix), Some(suffix)) = (
        unsafe { copy_text(detail_prefix, detail_prefix_length) },
        unsafe { copy_text(detail_suffix, detail_suffix_length) },
    ) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let detail =
        format!(r#"{prefix}{{"file":{file},"range":{{"start":{start},"end":{end}}}}}{suffix}"#);
    unsafe {
        context_raise_fault_v1(
            context,
            code,
            code_length,
            message,
            message_length,
            display,
            display_length,
            detail.as_ptr(),
            detail.len() as u64,
        )
    }
}

#[cfg(test)]
mod fault_context_tests {
    use super::*;
    use crate::gc::{activate_runtime_v1, deactivate_runtime_v1, enter_executor, leave_executor};
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};

    const CODE: &[u8] = b"ExampleFault";
    const MESSAGE: &[u8] = b"example message";
    const DISPLAY: &[u8] = b"example display";
    const DETAIL: &[u8] = br#"{"channel":"runtime"}"#;
    const CLEANUP_CODE: &[u8] = b"CleanupFault";
    const CLEANUP_MESSAGE: &[u8] = b"cleanup failed";
    const CLEANUP_DETAIL: &[u8] = br#"{"channel":"cleanup"}"#;
    const CONTRACT_PREFIX: &[u8] = br#"{"channel":"contract","fault":{"blameSpan":"#;
    const CONTRACT_SUFFIX: &[u8] = br#", "code":"PreconditionFault"}}"#;

    unsafe extern "C" fn completed_fixture(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_COMPLETED
    }

    unsafe fn raise_context_with(
        context: *mut c_void,
        code: &[u8],
        message: &[u8],
        detail: &[u8],
    ) -> i32 {
        unsafe {
            context_raise_fault_v1(
                context,
                code.as_ptr(),
                code.len() as u64,
                message.as_ptr(),
                message.len() as u64,
                DISPLAY.as_ptr(),
                DISPLAY.len() as u64,
                detail.as_ptr(),
                detail.len() as u64,
            )
        }
    }

    unsafe fn raise_context(context: *mut c_void) -> i32 {
        unsafe { raise_context_with(context, CODE, MESSAGE, DETAIL) }
    }

    unsafe fn set_task_fault_text(task: *mut LoomTask, code: &[u8], message: &[u8]) -> i32 {
        unsafe {
            task_set_fault(
                task,
                code.as_ptr(),
                code.len() as u64,
                message.as_ptr(),
                message.len() as u64,
            )
        }
    }

    #[test]
    fn standalone_runtime_context_routes_to_the_root_boundary() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            assert_eq!(activate_runtime_v1(runtime), WAIT_OK);
            assert_eq!(
                resolve_fault_context(runtime.cast()),
                Some(FaultContextTarget::Root),
            );
            assert_eq!(raise_context(runtime.cast()), WAIT_OK);
            assert_eq!(deactivate_runtime_v1(runtime), WAIT_OK);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn attached_executor_context_records_on_the_active_task() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            let executor = executor_create_for_runtime_v1(runtime);
            assert!(!executor.is_null());
            let task = task_spawn(executor, Some(completed_fixture), 1, 0);
            assert!(!task.is_null());

            (*executor).active_task = task;
            enter_executor(executor);
            assert_eq!(
                resolve_fault_context(executor.cast()),
                Some(FaultContextTarget::Executor(executor)),
            );
            assert_eq!(raise_context(executor.cast()), WAIT_OK);
            assert_eq!((*task).fault_code, "ExampleFault");
            assert_eq!((*task).fault_message, "example message");
            assert_eq!((*task).fault_detail, r#"{"channel":"runtime"}"#);

            assert_eq!(
                raise_context_with(
                    executor.cast(),
                    CLEANUP_CODE,
                    CLEANUP_MESSAGE,
                    CLEANUP_DETAIL,
                ),
                WAIT_OK,
            );
            assert_eq!((*task).fault_code, "ExampleFault");
            assert_eq!((*task).fault_message, "example message");
            assert_eq!((*task).fault_detail, r#"{"channel":"runtime"}"#);
            leave_executor();
            (*executor).active_task = ptr::null_mut();

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn span_fault_context_serializes_the_dynamic_location_exactly() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            let executor = executor_create_for_runtime_v1(runtime);
            assert!(!executor.is_null());
            let task = task_spawn(executor, Some(completed_fixture), 1, 0);
            assert!(!task.is_null());

            (*executor).active_task = task;
            enter_executor(executor);
            assert_eq!(
                context_raise_fault_with_span_v1(
                    executor.cast(),
                    CODE.as_ptr(),
                    CODE.len() as u64,
                    MESSAGE.as_ptr(),
                    MESSAGE.len() as u64,
                    DISPLAY.as_ptr(),
                    DISPLAY.len() as u64,
                    CONTRACT_PREFIX.as_ptr(),
                    CONTRACT_PREFIX.len() as u64,
                    7,
                    11,
                    19,
                    CONTRACT_SUFFIX.as_ptr(),
                    CONTRACT_SUFFIX.len() as u64,
                ),
                WAIT_OK,
            );
            assert_eq!(
                (*task).fault_detail,
                r#"{"channel":"contract","fault":{"blameSpan":{"file":7,"range":{"start":11,"end":19}}, "code":"PreconditionFault"}}"#,
            );
            leave_executor();
            (*executor).active_task = ptr::null_mut();

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn failed_child_primary_fault_is_inherited_once_by_awaiting_parent() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            let executor = executor_create_for_runtime_v1(runtime);
            assert!(!executor.is_null());
            let parent = task_spawn(executor, Some(completed_fixture), 1, 0);
            let child = task_spawn(executor, Some(completed_fixture), 1, 0);
            assert!(!parent.is_null() && !child.is_null());

            record_primary_task_fault(
                &mut *child,
                "PreconditionFault".into(),
                "child contract failed".into(),
                r#"{"channel":"contract"}"#.into(),
            );
            inherit_primary_task_fault(&mut *parent, &*child);
            record_primary_task_fault(
                &mut *parent,
                "CleanupFault".into(),
                "cleanup failed".into(),
                r#"{"channel":"cleanup"}"#.into(),
            );
            assert_eq!((*parent).fault_code, "PreconditionFault");
            assert_eq!((*parent).fault_message, "child contract failed");
            assert_eq!((*parent).fault_detail, r#"{"channel":"contract"}"#);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn task_fault_record_keeps_the_first_primary_failure() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        unsafe {
            let executor = executor_create_for_runtime_v1(runtime);
            assert!(!executor.is_null());
            let task = task_spawn(executor, Some(completed_fixture), 1, 0);
            assert!(!task.is_null());

            assert_eq!(set_task_fault_text(task, CODE, MESSAGE), WAIT_OK);
            assert_eq!(
                set_task_fault_text(task, CLEANUP_CODE, CLEANUP_MESSAGE),
                WAIT_OK,
            );
            assert_eq!((*task).fault_code, "ExampleFault");
            assert_eq!((*task).fault_message, "example message");
            assert_eq!((*task).fault_detail, "");

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn inactive_and_cross_runtime_contexts_are_rejected_without_dereferencing_them() {
        let first = runtime_create_v1();
        let second = runtime_create_v1();
        assert!(!first.is_null() && !second.is_null());
        unsafe {
            let first_executor = executor_create_for_runtime_v1(first);
            let second_executor = executor_create_for_runtime_v1(second);
            assert!(!first_executor.is_null() && !second_executor.is_null());

            assert_eq!(raise_context(first.cast()), WAIT_INVALID_ARGUMENT);
            assert_eq!(activate_runtime_v1(first), WAIT_OK);
            assert_eq!(
                resolve_fault_context(first_executor.cast()),
                Some(FaultContextTarget::Executor(first_executor)),
            );
            assert_eq!(raise_context(ptr::null_mut()), WAIT_INVALID_ARGUMENT);
            assert_eq!(raise_context(second.cast()), WAIT_INVALID_ARGUMENT);
            assert_eq!(raise_context(second_executor.cast()), WAIT_INVALID_ARGUMENT);
            assert_eq!(
                raise_context(std::ptr::dangling_mut::<c_void>()),
                WAIT_INVALID_ARGUMENT,
            );
            assert_eq!(deactivate_runtime_v1(first), WAIT_OK);

            executor_destroy(first_executor);
            executor_destroy(second_executor);
            assert_eq!(runtime_destroy_v1(first), WAIT_OK);
            assert_eq!(runtime_destroy_v1(second), WAIT_OK);
        }
    }
}

#[unsafe(export_name = "loom_task_prepare_join")]
pub unsafe extern "C" fn task_prepare_join(
    executor: *mut LoomExecutor,
    parent: *mut LoomTask,
    mode: u32,
) -> i32 {
    if executor.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let executor_ref = unsafe { &mut *executor };
    if executor_ref.cleanup_active()
        || !executor_owns(executor_ref, parent)
        || mode > TASK_JOIN_RACE
        || executor_ref.active_task != parent
        || unsafe { (*parent).status } != TaskStatus::Running
        || unsafe { (*parent).cancel_requested }
        || unsafe { (*parent).join_active }
    {
        return WAIT_INVALID_ARGUMENT;
    }
    unsafe {
        (*parent).join_children.clear();
        (*parent).join_mode = mode;
        (*parent).join_winner = NO_JOIN_WINNER;
        (*parent).join_step = TASK_COMPLETED;
        (*parent).join_active = true;
        if let Some(typed) = (*parent).typed.as_mut() {
            typed.join_completion_pending = false;
            typed.join_cancel_authorized = false;
            typed.join_winner_finalized = false;
        }
    }
    WAIT_OK
}

#[unsafe(export_name = "loom_task_add_join_child")]
pub unsafe extern "C" fn task_add_join_child(
    executor: *mut LoomExecutor,
    parent: *mut LoomTask,
    child: *mut LoomTask,
) -> i32 {
    if executor.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let executor_ref = unsafe { &mut *executor };
    if executor_ref.cleanup_active()
        || !executor_owns(executor_ref, parent)
        || !executor_owns(executor_ref, child)
        || executor_ref.active_task != parent
        || unsafe { (*parent).status } != TaskStatus::Running
        || unsafe { (*parent).cancel_requested }
        || !unsafe { (*parent).join_active }
        || child == parent
        || unsafe { (*child).owner } != parent
        || unsafe { (*parent).join_children.contains(&child) }
    {
        return WAIT_INVALID_ARGUMENT;
    }
    unsafe { (*parent).join_children.push(child) };
    WAIT_OK
}

#[unsafe(export_name = "loom_task_suspend_join")]
pub unsafe extern "C" fn task_suspend_join(
    executor: *mut LoomExecutor,
    parent: *mut LoomTask,
) -> i32 {
    if executor.is_null() {
        return -WAIT_INVALID_ARGUMENT;
    }
    let executor_ref = unsafe { &mut *executor };
    if executor_ref.cleanup_active()
        || !executor_owns(executor_ref, parent)
        || executor_ref.active_task != parent
        || unsafe { (*parent).status } != TaskStatus::Running
        || unsafe { (*parent).cancel_requested }
        || !unsafe { (*parent).join_active }
    {
        return -WAIT_INVALID_ARGUMENT;
    }
    if unsafe { (*parent).join_children.is_empty() } {
        unsafe { (*parent).join_active = false };
        return if matches!(
            unsafe { (*parent).join_mode },
            TASK_JOIN_ALL | TASK_JOIN_SETTLED
        ) {
            0
        } else {
            unsafe { (*parent).join_step = TASK_FAULTED };
            -WAIT_INVALID_ARGUMENT
        };
    }
    unsafe {
        (*parent).status = TaskStatus::Waiting;
        update_join(executor_ref, parent);
    }
    if unsafe { (*parent).status } != TaskStatus::Runnable {
        return 1;
    }

    // `update_join` uses the ordinary wake-up path even when every child was
    // already terminal. A zero return means the active callback never yielded,
    // so reclaim that queued wake-up and restore its Running activation before
    // generated code takes the result or starts another structured join.
    let Some(queued_index) = executor_ref
        .runnable
        .iter()
        .position(|candidate| *candidate == parent)
    else {
        unsafe {
            record_typed_runtime_defect(
                parent,
                "LOOM_RUNTIME_JOIN_READY_QUEUE",
                "an immediate join wake-up was missing from the ready queue",
            );
        }
        return -WAIT_INVALID_ARGUMENT;
    };
    if !unsafe { (*parent).queued }
        || executor_ref
            .runnable
            .iter()
            .skip(queued_index.saturating_add(1))
            .any(|candidate| *candidate == parent)
        || executor_ref.runnable.remove(queued_index) != Some(parent)
    {
        unsafe {
            record_typed_runtime_defect(
                parent,
                "LOOM_RUNTIME_JOIN_READY_QUEUE",
                "an immediate join wake-up disagreed with its queued state",
            );
        }
        return -WAIT_INVALID_ARGUMENT;
    }
    unsafe {
        (*parent).queued = false;
        (*parent).status = TaskStatus::Running;
    }
    0
}

#[unsafe(export_name = "loom_task_join_count")]
pub unsafe extern "C" fn task_join_count(parent: *const LoomTask) -> u64 {
    if parent.is_null() {
        0
    } else {
        unsafe { (*parent).join_children.len() as u64 }
    }
}

#[unsafe(export_name = "loom_task_join_winner")]
pub unsafe extern "C" fn task_join_winner(parent: *const LoomTask) -> u64 {
    if parent.is_null() {
        NO_JOIN_WINNER
    } else {
        unsafe { (*parent).join_winner }
    }
}

/// Finalizes a typed winner-selecting join before generated code observes its
/// terminal step. The original winner ordinal remains stable and that child
/// stays attached until its exact result or outcome is consumed. Every loser
/// is deterministically disposed in reverse input order and retired now.
unsafe fn finalize_typed_winner_join(executor: &mut LoomExecutor, parent: *mut LoomTask) {
    let mode = unsafe { (*parent).join_mode };
    if !executor_owns(executor, parent)
        || unsafe { (*parent).typed.is_none() }
        || !matches!(mode, TASK_JOIN_ANY | TASK_JOIN_RACE)
        || unsafe { (*parent).typed.as_ref().unwrap().join_winner_finalized }
    {
        return;
    }
    let children = unsafe { (*parent).join_children.clone() };
    let winner_raw = unsafe { (*parent).join_winner };
    let winner = (winner_raw != NO_JOIN_WINNER)
        .then(|| usize::try_from(winner_raw).ok())
        .flatten()
        .filter(|winner| *winner < children.len());
    let executor_pointer: *mut LoomExecutor = executor;
    let children_valid = !children.is_empty()
        && children
            .iter()
            .enumerate()
            .all(|(index, child)| !children[..index].contains(child))
        && children.iter().copied().all(|child| {
            executor_owns(executor, child)
                && unsafe {
                    (*child).executor == executor_pointer
                        && (*child).owner == parent
                        && (*child).typed.is_some()
                        && terminal((*child).status)
                        && (*parent)
                            .owned_children
                            .iter()
                            .filter(|owned| **owned == child)
                            .count()
                            == 1
                }
        });
    let outcome_valid = children_valid
        && match mode {
            TASK_JOIN_ANY => match (unsafe { (*parent).join_step }, winner) {
                (TASK_COMPLETED, Some(winner)) => {
                    (unsafe { (*children[winner]).status }) == TaskStatus::Completed
                }
                (TASK_FAULTED, None) if winner_raw == NO_JOIN_WINNER => children
                    .iter()
                    .all(|child| unsafe { (**child).status } != TaskStatus::Completed),
                _ => false,
            },
            TASK_JOIN_RACE => {
                (unsafe { (*parent).join_step }) == TASK_COMPLETED && winner.is_some()
            }
            _ => false,
        };
    if !children_valid || !outcome_valid {
        unsafe {
            (*parent).typed.as_mut().unwrap().join_winner_finalized = true;
            record_typed_runtime_defect(
                parent,
                "LOOM_RUNTIME_TYPED_WINNER_FINALIZE",
                "typed winner-selecting join had an invalid child topology or winner state",
            );
            (*parent).join_step = TASK_FAULTED;
        }
        return;
    }
    unsafe { (*parent).typed.as_mut().unwrap().join_winner_finalized = true };

    let mut first_non_clean = None;
    for (index, child) in children.into_iter().enumerate().rev() {
        if Some(index) == winner {
            continue;
        }
        if unsafe { (*child).status } == TaskStatus::Completed {
            let outcome = unsafe { dispose_typed_result(executor, child) };
            if !outcome.is_clean() {
                unsafe { (*child).status = TaskStatus::Faulted };
                first_non_clean.get_or_insert(child);
            }
        }
        unsafe { retire_typed_child(executor, parent, child) };
    }

    if let Some(failure) = first_non_clean {
        unsafe { inherit_primary_task_fault(&mut *parent, &*failure) };
        if !unsafe { (*parent).primary_fault_recorded } {
            unsafe {
                record_typed_runtime_defect(
                    parent,
                    "LOOM_RUNTIME_TYPED_WINNER_DISPOSE",
                    "a typed winner-selecting join loser result could not be disposed",
                );
            }
        }
        unsafe { (*parent).join_step = TASK_FAULTED };
    }
}

#[unsafe(export_name = "loom_task_join_step")]
pub unsafe extern "C" fn task_join_step(parent: *const LoomTask) -> i32 {
    let parent_pointer = parent.cast_mut();
    if parent_pointer.is_null() {
        return TASK_FAULTED;
    }
    let executor = unsafe { (*parent_pointer).executor };
    let active_parent = !executor.is_null()
        && !unsafe { (*executor).cleanup_active() }
        && executor_owns(unsafe { &*executor }, parent_pointer)
        && unsafe { (*executor).active_task } == parent_pointer
        && unsafe { (*parent_pointer).status } == TaskStatus::Running
        && !unsafe { (*parent_pointer).cancel_requested };
    let join_step = unsafe { (*parent_pointer).join_step };
    let completed = if let Some(typed) = unsafe { (*parent_pointer).typed.as_mut() } {
        // Only the scheduler can mint this token when a join becomes runnable.
        // Reading the outcome consumes it atomically, so an old Cancelled step
        // cannot authorize a later callback activation.
        let completed = active_parent && std::mem::take(&mut typed.join_completion_pending);
        typed.join_cancel_authorized = completed && join_step == TASK_CANCELLED;
        completed
    } else {
        false
    };
    if completed
        && matches!(
            unsafe { (*parent_pointer).join_mode },
            TASK_JOIN_ANY | TASK_JOIN_RACE
        )
    {
        unsafe { finalize_typed_winner_join(&mut *executor, parent_pointer) };
    }
    unsafe { (*parent_pointer).join_step }
}

#[unsafe(export_name = "loom_task_join_result_step")]
pub unsafe extern "C" fn task_join_result_step(parent: *const LoomTask, index: u64) -> i32 {
    let Ok(index) = usize::try_from(index) else {
        return TASK_FAULTED;
    };
    if parent.is_null() || index >= unsafe { (*parent).join_children.len() } {
        return TASK_FAULTED;
    }
    terminal_step(unsafe { (&(*parent).join_children)[index] })
}

#[unsafe(export_name = "loom_task_join_result")]
pub unsafe extern "C" fn task_join_result(parent: *mut LoomTask, index: u64) -> *mut c_void {
    let Ok(index) = usize::try_from(index) else {
        return ptr::null_mut();
    };
    if parent.is_null() || index >= unsafe { (*parent).join_children.len() } {
        return ptr::null_mut();
    }
    unsafe { task_result((&(*parent).join_children)[index]) }
}

unsafe fn write_outcome(parent: *mut LoomTask, index: usize, destination: *mut ValueSlot) -> i32 {
    if parent.is_null()
        || destination.is_null()
        || index >= unsafe { (*parent).join_children.len() }
    {
        return WAIT_INVALID_ARGUMENT;
    }
    let step = terminal_step(unsafe { (&(*parent).join_children)[index] });
    let value = match step {
        TASK_COMPLETED => {
            let result = unsafe { task_join_result(parent, index as u64).cast::<ValueSlot>() };
            if result.is_null() {
                return WAIT_INVALID_ARGUMENT;
            }
            let Some(value) = enum_value(
                TASK_OUTCOME_TYPE,
                TASK_OUTCOME_COMPLETED,
                vec![unsafe { *result }],
            ) else {
                return WAIT_NO_MEMORY;
            };
            value
        }
        TASK_CANCELLED => enum_value(TASK_OUTCOME_TYPE, TASK_OUTCOME_CANCELLED, Vec::new())
            .unwrap_or_else(|| unreachable!()),
        _ => {
            // Do not retain a Rust reference into a Task while managed
            // allocation can cause the collector to traverse and rewrite Task
            // slots. Own both strings before entering the first safepoint.
            let (code, message) = {
                let child = unsafe { &*(&(*parent).join_children)[index] };
                let code = if child.fault_code.is_empty() {
                    TASK_FAULT_CODE.to_owned()
                } else {
                    child.fault_code.clone()
                };
                let message = if child.fault_message.is_empty() {
                    TASK_FAULT_MESSAGE.to_owned()
                } else {
                    child.fault_message.clone()
                };
                (code, message)
            };
            let Ok(roots) = RuntimeRootScope::with_count(2) else {
                return WAIT_NO_MEMORY;
            };
            roots.write(0, text_value(code.as_bytes()));
            roots.write(1, text_value(message.as_bytes()));
            let Some(fault) = record_value(TASK_FAULT_TYPE, vec![roots.read(0), roots.read(1)])
            else {
                return WAIT_NO_MEMORY;
            };
            let Some(value) = enum_value(TASK_OUTCOME_TYPE, TASK_OUTCOME_FAULTED, vec![fault])
            else {
                return WAIT_NO_MEMORY;
            };
            value
        }
    };
    unsafe { destination.write(value) };
    WAIT_OK
}

fn text_value(bytes: &[u8]) -> ValueSlot {
    crate::gc::text_value(bytes).unwrap_or_else(|| std::process::abort())
}

unsafe fn retire_join_children(parent: *mut LoomTask) {
    if parent.is_null() {
        return;
    }
    let executor = unsafe { &mut *(*parent).executor };
    let children = unsafe { std::mem::take(&mut (*parent).join_children) };
    for child in children {
        if let Some(index) =
            unsafe { (*parent).owned_children.iter() }.position(|candidate| *candidate == child)
        {
            unsafe { (*parent).owned_children.remove(index) };
        }
        if !executor.retired_tasks.contains(&child) && terminal(unsafe { (*child).status }) {
            executor.retired_tasks.push(child);
        }
    }
}

unsafe fn transfer_child_result_resources(parent: *mut LoomTask, index: usize) {
    let child = unsafe { (&(*parent).join_children)[index] };
    unsafe { transfer_result_resources(parent, child) };
}

fn referenced_tasks_in_value(value: &ValueSlot, referenced: &mut std::collections::HashSet<usize>) {
    match value.words[0] {
        VALUE_TAG_TASK => {
            if value.words[2] == TASK_VALUE_DIRECT
                && value.words[4] != 0
                && let Ok(address) = usize::try_from(value.words[4])
            {
                referenced.insert(address);
            }
        }
        VALUE_TAG_RECORD | VALUE_TAG_TUPLE | VALUE_TAG_LIST => {
            referenced_tasks_in_nodes(
                value.words[4] as *const ValueNode,
                value.words[2],
                referenced,
            );
        }
        VALUE_TAG_ENUM => {
            referenced_tasks_in_nodes(
                value.words[4] as *const ValueNode,
                value.words[3],
                referenced,
            );
        }
        _ => {}
    }
}

fn referenced_tasks_in_nodes(
    mut node: *const ValueNode,
    count: u64,
    referenced: &mut std::collections::HashSet<usize>,
) {
    for _ in 0..count {
        if node.is_null() {
            return;
        }
        unsafe {
            referenced_tasks_in_value(&(*node).value, referenced);
            node = (*node).next;
        }
    }
}

fn reap_retired_tasks(executor: &mut LoomExecutor, root: *mut LoomTask) {
    if executor.retired_tasks.is_empty() {
        return;
    }
    let retired = executor
        .retired_tasks
        .iter()
        .map(|task| *task as usize)
        .collect::<std::collections::HashSet<_>>();
    let mut referenced = std::collections::HashSet::new();
    referenced.insert(root as usize);
    for task in &executor.tasks {
        let pointer = (&raw const **task) as usize;
        if retired.contains(&pointer) {
            continue;
        }
        for slot in &task.slots {
            referenced_tasks_in_value(slot, &mut referenced);
        }
        referenced.extend(task.owned_children.iter().map(|child| *child as usize));
        referenced.extend(task.join_children.iter().map(|child| *child as usize));
    }
    let reclaim = retired
        .difference(&referenced)
        .copied()
        .collect::<std::collections::HashSet<_>>();
    if reclaim.is_empty() {
        return;
    }
    executor
        .runnable
        .retain(|task| !reclaim.contains(&(*task as usize)));
    executor
        .join_specs
        .retain(|join| !reclaim.contains(&(join.task as usize)));
    let before = executor.tasks.len();
    executor
        .tasks
        .retain(|task| !reclaim.contains(&((&raw const **task) as usize)));
    executor
        .retired_tasks
        .retain(|task| !reclaim.contains(&(*task as usize)));
    executor.tasks_reclaimed = executor
        .tasks_reclaimed
        .saturating_add(before.saturating_sub(executor.tasks.len()) as u64);
}

#[unsafe(export_name = "loom_task_write_join_result")]
pub unsafe extern "C" fn task_write_join_result(
    parent: *mut LoomTask,
    destination: *mut c_void,
    shape: u32,
) -> i32 {
    if parent.is_null()
        || destination.is_null()
        || shape > JOIN_RESULT_OUTCOME_LIST
        || unsafe { (*parent).executor.is_null() }
        || unsafe { (*(*parent).executor).cleanup_active() }
    {
        return WAIT_INVALID_ARGUMENT;
    }
    let destination = destination.cast::<ValueSlot>();
    let count = unsafe { (*parent).join_children.len() };
    let outcome = matches!(
        shape,
        JOIN_RESULT_OUTCOME | JOIN_RESULT_OUTCOME_TUPLE | JOIN_RESULT_OUTCOME_LIST
    );
    let aggregate = matches!(
        shape,
        JOIN_RESULT_TUPLE | JOIN_RESULT_LIST | JOIN_RESULT_OUTCOME_TUPLE | JOIN_RESULT_OUTCOME_LIST
    );
    if !aggregate {
        if count == 0 {
            return WAIT_INVALID_ARGUMENT;
        }
        let winner = unsafe { (*parent).join_winner };
        let index = if winner == NO_JOIN_WINNER {
            0
        } else {
            let Ok(index) = usize::try_from(winner) else {
                return WAIT_INVALID_ARGUMENT;
            };
            index
        };
        if outcome {
            let status = unsafe { write_outcome(parent, index, destination) };
            if status == WAIT_OK {
                unsafe { transfer_child_result_resources(parent, index) };
                unsafe { retire_join_children(parent) };
            }
            return status;
        }
        let result = unsafe { task_join_result(parent, index as u64).cast::<ValueSlot>() };
        if result.is_null() {
            return WAIT_INVALID_ARGUMENT;
        }
        unsafe { destination.write(*result) };
        unsafe { transfer_child_result_resources(parent, index) };
        unsafe { retire_join_children(parent) };
        return WAIT_OK;
    }

    let mut result = ValueSlot::default();
    result.words[0] = if matches!(shape, JOIN_RESULT_LIST | JOIN_RESULT_OUTCOME_LIST) {
        VALUE_TAG_LIST
    } else {
        VALUE_TAG_TUPLE
    };
    let Ok(roots) = RuntimeRootScope::from_values(vec![result, ValueSlot::default()]) else {
        return WAIT_NO_MEMORY;
    };
    let stream = NodeStream::new(&roots, 0, result);
    for index in (0..count).rev() {
        let status = if outcome {
            unsafe { write_outcome(parent, index, roots.pointer(1)) }
        } else {
            let result = unsafe { task_join_result(parent, index as u64).cast::<ValueSlot>() };
            if result.is_null() {
                WAIT_INVALID_ARGUMENT
            } else {
                roots.write(1, unsafe { *result });
                WAIT_OK
            }
        };
        if status != WAIT_OK {
            return status;
        }
        if stream.prepend(1) != crate::GC_OK {
            return WAIT_NO_MEMORY;
        }
    }
    unsafe { destination.write(roots.read(0)) };
    for index in 0..count {
        unsafe { transfer_child_result_resources(parent, index) };
    }
    unsafe { retire_join_children(parent) };
    WAIT_OK
}

#[unsafe(export_name = "loom_join_create")]
pub unsafe extern "C" fn join_create(
    executor: *mut LoomExecutor,
    mode: u32,
    shape: u32,
) -> *mut LoomJoinSpec {
    if executor.is_null()
        || mode > TASK_JOIN_RACE
        || shape > JOIN_RESULT_OUTCOME_LIST
        || unsafe { (*executor).cleanup_active() }
        || unsafe { (*executor).active_task.is_null() }
    {
        return ptr::null_mut();
    }
    let task = unsafe { task_spawn(executor, Some(resume_composite), 1, 0) };
    if task.is_null() {
        return ptr::null_mut();
    }
    let executor_ref = unsafe { &mut *executor };
    let mut join = Box::new(LoomJoinSpec {
        executor,
        owner: executor_ref.active_task,
        task,
        mode,
        shape,
        tasks: Vec::new(),
    });
    let pointer = &raw mut *join;
    unsafe { (*task).composite_spec = pointer };
    executor_ref.join_specs.push(join);
    pointer
}

#[unsafe(export_name = "loom_join_task")]
pub unsafe extern "C" fn join_task(join: *mut LoomJoinSpec) -> *mut LoomTask {
    if join.is_null() {
        ptr::null_mut()
    } else {
        unsafe { (*join).task }
    }
}

#[unsafe(export_name = "loom_join_add_task")]
pub unsafe extern "C" fn join_add_task(join: *mut LoomJoinSpec, task: *mut LoomTask) -> i32 {
    if join.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let executor = unsafe { (*join).executor };
    let valid = !executor.is_null()
        && !unsafe { (*executor).cleanup_active() }
        && executor_owns(unsafe { &*executor }, task)
        && unsafe { (*join).owner } == unsafe { (*executor).active_task };
    if !valid
        || unsafe { (*task).owner } != unsafe { (*join).owner }
        || task == unsafe { (*join).owner }
        || task == unsafe { (*join).task }
        || unsafe { (*join).tasks.contains(&task) }
    {
        return WAIT_INVALID_ARGUMENT;
    }
    let owner = unsafe { (*join).owner };
    let composite = unsafe { (*join).task };
    unsafe {
        if let Some(index) = (*owner)
            .owned_children
            .iter()
            .position(|candidate| *candidate == task)
        {
            (*owner).owned_children.remove(index);
        } else {
            return WAIT_INVALID_ARGUMENT;
        }
        (*task).owner = composite;
        (*composite).owned_children.push(task);
        (*join).tasks.push(task);
    }
    WAIT_OK
}

#[unsafe(export_name = "loom_join_add_list")]
pub unsafe extern "C" fn join_add_list(join: *mut LoomJoinSpec, list_value: *const c_void) -> i32 {
    if join.is_null() || list_value.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let executor = unsafe { (*join).executor };
    if executor.is_null() || unsafe { (*executor).cleanup_active() } {
        return WAIT_INVALID_ARGUMENT;
    }
    let list = unsafe { &*list_value.cast::<ValueSlot>() };
    if list.words[0] != VALUE_TAG_LIST {
        return WAIT_INVALID_ARGUMENT;
    }
    let mut node = list.words[4] as *const ValueNode;
    for _ in 0..list.words[2] {
        if node.is_null()
            || unsafe { (*node).value.words[0] } != VALUE_TAG_TASK
            || unsafe { (*node).value.words[2] } != TASK_VALUE_DIRECT
        {
            return WAIT_INVALID_ARGUMENT;
        }
        let task = unsafe { (*node).value.words[4] as *mut LoomTask };
        let status = unsafe { join_add_task(join, task) };
        if status != WAIT_OK {
            return status;
        }
        node = unsafe { (*node).next };
    }
    i32::from(!node.is_null()) * WAIT_INVALID_ARGUMENT
}

#[unsafe(export_name = "loom_task_suspend_value")]
pub unsafe extern "C" fn task_suspend_value(
    executor: *mut LoomExecutor,
    parent: *mut LoomTask,
    task_value: *const c_void,
) -> i32 {
    if executor.is_null() || task_value.is_null() {
        return -WAIT_INVALID_ARGUMENT;
    }
    let value = unsafe { &*task_value.cast::<ValueSlot>() };
    if value.words[0] != VALUE_TAG_TASK {
        return -WAIT_INVALID_ARGUMENT;
    }
    if value.words[2] != TASK_VALUE_DIRECT {
        return -WAIT_INVALID_ARGUMENT;
    }
    let prepare = unsafe { task_prepare_join(executor, parent, TASK_JOIN_ALL) };
    if prepare != WAIT_OK {
        return -prepare;
    }
    let add = unsafe { task_add_join_child(executor, parent, value.words[4] as *mut LoomTask) };
    if add != WAIT_OK {
        unsafe { (*parent).join_active = false };
        return -add;
    }
    unsafe { task_suspend_join(executor, parent) }
}

#[unsafe(export_name = "loom_task_suspend_wait")]
pub unsafe extern "C" fn task_suspend_wait(
    executor: *mut LoomExecutor,
    task: *mut LoomTask,
    source: *const LoomWaitSource,
) -> i32 {
    if executor.is_null() || task.is_null() || source.is_null() {
        return WAIT_INVALID_ARGUMENT;
    }
    let valid = !unsafe { (*executor).cleanup_active() }
        && executor_owns(unsafe { &*executor }, task)
        && unsafe { (*executor).active_task } == task
        && unsafe { (*task).status } == TaskStatus::Running
        && !unsafe { (*task).cancel_requested };
    if !valid {
        return WAIT_INVALID_ARGUMENT;
    }
    let mut registration = LoomRegistration::default();
    let status = unsafe {
        register_for_task(
            executor,
            source,
            task.cast::<c_void>(),
            &raw mut registration,
        )
    };
    if status != WAIT_OK {
        return status;
    }
    unsafe {
        (*task).waits.push(registration);
        (*task).status = TaskStatus::Waiting;
    }
    WAIT_OK
}

#[unsafe(export_name = "loom_task_cancel")]
pub unsafe extern "C" fn task_cancel(executor: *mut LoomExecutor, task: *mut LoomTask) -> i32 {
    if executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, task)
    {
        return WAIT_INVALID_ARGUMENT;
    }
    unsafe { request_cancel(&mut *executor, task) };
    WAIT_OK
}

unsafe fn validate_typed_task_step(task: *mut LoomTask, cancelling: bool, step: i32) -> i32 {
    let join_cancel_authorized = unsafe {
        let typed = (*task).typed.as_mut().expect("typed scheduler branch");
        // A normal callback activation gets one opportunity to observe and
        // propagate its newly completed join. Both an unread completion and a
        // read authorization expire when this step is validated.
        typed.join_completion_pending = false;
        std::mem::take(&mut typed.join_cancel_authorized)
    };
    // Recording a primary fault commits this resume/cancel activation to the
    // faulted terminal path even if buggy generated cleanup returns otherwise.
    if unsafe { (*task).primary_fault_recorded } && step != TASK_FAULTED {
        return TASK_FAULTED;
    }
    if cancelling {
        return match step {
            TASK_CANCELLED | TASK_FAULTED => step,
            TASK_PENDING => {
                unsafe {
                    record_typed_runtime_defect(
                        task,
                        "LOOM_RUNTIME_TYPED_CANCEL_PENDING",
                        "typed Task cancellation must not suspend",
                    );
                }
                TASK_FAULTED
            }
            _ => {
                unsafe {
                    record_typed_runtime_defect(
                        task,
                        "LOOM_RUNTIME_TYPED_CANCEL_STATUS",
                        "typed Task cancellation returned an invalid terminal status",
                    );
                }
                TASK_FAULTED
            }
        };
    }
    let result_initialized = unsafe {
        (*task)
            .typed
            .as_ref()
            .expect("typed scheduler branch")
            .result_initialized
    };
    match step {
        TASK_PENDING if !result_initialized => TASK_PENDING,
        TASK_COMPLETED if result_initialized => TASK_COMPLETED,
        TASK_FAULTED => TASK_FAULTED,
        TASK_PENDING => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_RESULT_EARLY",
                    "typed Task published a result before returning completed",
                );
            }
            TASK_FAULTED
        }
        TASK_COMPLETED => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_RESULT_MISSING",
                    "typed Task returned completed without publishing its result",
                );
            }
            TASK_FAULTED
        }
        TASK_CANCELLED if join_cancel_authorized => TASK_CANCELLED,
        TASK_CANCELLED => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CANCEL_UNREQUESTED",
                    "typed Task returned cancelled without a cancellation request",
                );
            }
            TASK_FAULTED
        }
        _ => {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_RESUME_STATUS",
                    "typed Task resume returned an invalid status",
                );
            }
            TASK_FAULTED
        }
    }
}

unsafe fn run_typed_task_step(executor: *mut LoomExecutor, task: *mut LoomTask) -> i32 {
    let cancelling = unsafe { (*task).cancel_requested };
    if cancelling {
        let children = unsafe { (*task).owned_children.clone() };
        for child in &children {
            if !terminal(unsafe { (**child).status }) {
                unsafe { request_cancel(&mut *executor, *child) };
            }
        }
        if !all_terminal(&children) {
            unsafe {
                (*task).deferred_terminal = TASK_CANCELLED;
                (*task).status = TaskStatus::Draining;
            }
            return TASK_PENDING;
        }
    }
    let fault_before = unsafe { (*task).primary_fault_recorded };
    let (callback, frame) = {
        let typed = unsafe { (*task).typed.as_mut().expect("typed scheduler branch") };
        let callback = if cancelling {
            if typed.cancel_invoked {
                unsafe {
                    record_typed_runtime_defect(
                        task,
                        "LOOM_RUNTIME_TYPED_CANCEL_TWICE",
                        "typed Task cancellation callback was selected more than once",
                    );
                }
                return TASK_FAULTED;
            }
            typed.cancel_invoked = true;
            typed.cancel
        } else {
            typed.resume
        };
        (callback, typed.frame_pointer())
    };

    let (step, cleanup_protocol_intact) = if cancelling {
        let invocation =
            unsafe { invoke_typed_cleanup_callback(&mut *executor, task, callback, frame) };
        if !invocation.cleanup_phase_entered {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CLEANUP_DEPTH",
                    "typed Task cancellation exceeded the cleanup nesting limit",
                );
            }
        }
        if !invocation.activation_intact {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CANCEL_ACTIVATION",
                    "typed Task cancellation leaked runtime activation or root state",
                );
            }
        }
        if !invocation.topology_intact {
            unsafe {
                record_typed_runtime_defect(
                    task,
                    "LOOM_RUNTIME_TYPED_CANCEL_TOPOLOGY",
                    "typed Task cancellation changed scheduler topology",
                );
            }
        }
        (
            if invocation.protocol_intact() {
                invocation.step
            } else {
                TASK_FAULTED
            },
            invocation.protocol_intact(),
        )
    } else {
        enter_executor(executor);
        let step = unsafe { callback(task.cast(), executor.cast(), frame) };
        leave_executor();
        (step, true)
    };
    let cleanup_recorded_fault = !fault_before && unsafe { (*task).primary_fault_recorded };
    if cancelling
        && !fault_before
        && cleanup_protocol_intact
        && step == TASK_FAULTED
        && cleanup_recorded_fault
    {
        // Only a well-formed cleanup RuntimeFault is suppressed after an
        // established cancellation. Protocol and topology defects remain
        // faulted and are never laundered into Cancelled.
        unsafe { suppress_new_typed_cleanup_fault(task, fault_before) };
        TASK_CANCELLED
    } else if cancelling
        && !fault_before
        && cleanup_protocol_intact
        && step == TASK_CANCELLED
        && !cleanup_recorded_fault
    {
        TASK_CANCELLED
    } else {
        unsafe { validate_typed_task_step(task, cancelling, step) }
    }
}

#[unsafe(export_name = "loom_executor_run")]
pub unsafe extern "C" fn executor_run(executor: *mut LoomExecutor, root: *mut LoomTask) -> i32 {
    if executor.is_null()
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, root)
        || !unsafe { (*root).owner.is_null() }
        || unsafe { (*root).status } == TaskStatus::Unpublished
    {
        return WAIT_INVALID_ARGUMENT;
    }
    while !terminal(unsafe { (*root).status }) {
        unsafe { drain_worker_completions(executor) };
        unsafe { consume_notifications(executor) };
        if unsafe { (*executor).runnable.is_empty() } {
            let mut ready_count = 0;
            let status = unsafe { wait_for_scheduler(executor, &raw mut ready_count) };
            if status != WAIT_OK {
                return status;
            }
            unsafe { drain_worker_completions(executor) };
            unsafe { consume_notifications(executor) };
            if unsafe { (*executor).runnable.is_empty() } {
                if has_registrations(unsafe { &*executor }) {
                    continue;
                }
                return WAIT_UNSUPPORTED;
            }
        }
        let task = {
            // SAFETY: the scheduler is the executor's unique driver.
            let executor_ref = unsafe { &mut *executor };
            reap_retired_tasks(executor_ref, root);
            poll(executor_ref);
            move_task_frames(executor_ref);
            executor_ref.runnable.pop_front()
        };
        let Some(task) = task else {
            continue;
        };
        unsafe { (*task).queued = false };
        if unsafe { (*task).status } != TaskStatus::Runnable {
            continue;
        }
        unsafe { (*task).status = TaskStatus::Running };
        unsafe { (*executor).active_task = task };
        let step = if unsafe { (*task).typed.is_some() } {
            unsafe { run_typed_task_step(executor, task) }
        } else {
            let descriptor = unsafe { (*task).descriptor };
            let resume = if unsafe { (*task).cancel_requested } {
                descriptor.cancel.or(descriptor.resume)
            } else {
                descriptor.resume
            };
            let Some(resume) = resume else {
                unsafe {
                    (*executor).active_task = ptr::null_mut();
                    complete_terminal(&mut *executor, task, TASK_FAULTED);
                }
                continue;
            };
            enter_executor(executor);
            let step = unsafe { resume(task, executor) };
            leave_executor();
            step
        };
        unsafe { (*executor).active_task = ptr::null_mut() };
        if step == TASK_PENDING {
            if unsafe { (*task).status } == TaskStatus::Running {
                unsafe { (*task).status = TaskStatus::Waiting };
            }
        } else {
            // SAFETY: no generated code is active, so the scheduler may take
            // the unique executor borrow again.
            let executor_ref = unsafe { &mut *executor };
            unsafe { complete_terminal(executor_ref, task, step) };
        }
    }
    match unsafe { (*root).status } {
        TaskStatus::Completed => TASK_COMPLETED,
        TaskStatus::Cancelled => TASK_CANCELLED,
        _ => TASK_FAULTED,
    }
}

#[unsafe(export_name = "loom_executor_gc_collections")]
pub unsafe extern "C" fn executor_gc_collections(executor: *const LoomExecutor) -> u64 {
    if executor.is_null() {
        0
    } else {
        unsafe { (*executor).heap().collections }
    }
}

#[unsafe(export_name = "loom_executor_gc_relocations")]
pub unsafe extern "C" fn executor_gc_relocations(executor: *const LoomExecutor) -> u64 {
    if executor.is_null() {
        0
    } else {
        unsafe { (*executor).heap().relocations }
    }
}

#[unsafe(export_name = "loom_executor_gc_reclaimed")]
pub unsafe extern "C" fn executor_gc_reclaimed(executor: *const LoomExecutor) -> u64 {
    if executor.is_null() {
        0
    } else {
        unsafe { (*executor).heap().reclaimed }
    }
}

#[unsafe(export_name = "loom_executor_gc_live_objects")]
pub unsafe extern "C" fn executor_gc_live_objects(executor: *const LoomExecutor) -> u64 {
    if executor.is_null() {
        0
    } else {
        let executor = unsafe { &*executor };
        let heap = executor.heap();
        (heap.values.len() as u64)
            .saturating_add(heap.nodes.len() as u64)
            .saturating_add(heap.sequences.len() as u64)
            .saturating_add(heap.typed_object_count() as u64)
            .saturating_add(heap.witnesses.len() as u64)
    }
}

#[unsafe(export_name = "loom_executor_live_tasks")]
pub unsafe extern "C" fn executor_live_tasks(executor: *const LoomExecutor) -> u64 {
    if executor.is_null() {
        0
    } else {
        unsafe { (*executor).tasks.len() as u64 }
    }
}

#[unsafe(export_name = "loom_executor_tasks_reclaimed")]
pub unsafe extern "C" fn executor_tasks_reclaimed(executor: *const LoomExecutor) -> u64 {
    if executor.is_null() {
        0
    } else {
        unsafe { (*executor).tasks_reclaimed }
    }
}

#[cfg(test)]
mod resource_ownership_tests {
    use std::ffi::c_void;
    use std::io::{self, Read};
    use std::mem::{align_of, size_of};
    use std::net::{TcpListener, TcpStream};

    use super::*;
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};

    unsafe extern "C" fn complete_fixture(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_COMPLETED
    }

    unsafe extern "C" fn typed_pending_fixture(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_PENDING
    }

    unsafe extern "C" fn typed_cancel_fixture(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_CANCELLED
    }

    unsafe extern "C" fn typed_invalid_dispose_fixture(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        i32::MIN
    }

    fn typed_resource_descriptor() -> LoomTypedCoroutineDescriptor {
        LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(typed_pending_fixture),
            cancel: Some(typed_cancel_fixture),
            dispose_result: None,
            frame_size: size_of::<i64>() as u64,
            frame_align: align_of::<i64>() as u64,
            result_offset: 0,
            result_size: size_of::<i64>() as u64,
            result_align: align_of::<i64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        }
    }

    unsafe fn create_typed_resource_task(executor: *mut LoomExecutor) -> *mut LoomTask {
        let descriptor = typed_resource_descriptor();
        let task = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
        assert!(!task.is_null());
        assert_eq!(unsafe { typed_task_initialize_v1(task, 0) }, TYPED_TASK_OK);
        assert_eq!(
            unsafe { typed_task_publish_v1(executor, task) },
            TYPED_TASK_OK
        );
        task
    }

    unsafe fn activate_typed_task(executor: *mut LoomExecutor, task: *mut LoomTask) {
        let index = unsafe {
            (*executor)
                .runnable
                .iter()
                .position(|candidate| *candidate == task)
                .expect("typed fixture must be runnable")
        };
        assert_eq!(unsafe { (*executor).runnable.remove(index) }, Some(task));
        unsafe {
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
        }
    }

    unsafe fn detach_from_ready_queue(executor: *mut LoomExecutor, task: *mut LoomTask) {
        unsafe {
            (*executor).runnable.retain(|candidate| *candidate != task);
            (*task).queued = false;
        }
    }

    unsafe fn complete_typed_resource(
        executor: *mut LoomExecutor,
        task: *mut LoomTask,
        resource: OwnedResource,
    ) -> i64 {
        let handle = resource.handle_bits();
        unsafe { detach_from_ready_queue(executor, task) };
        unsafe {
            let typed = (*task).typed.as_mut().expect("typed resource fixture");
            typed.frame_pointer().cast::<i64>().write(handle);
            typed.result_initialized = true;
            typed.root_state = typed.completed_root_state;
            (*task).owned_result_resources.push(resource);
            (*task).status = TaskStatus::Completed;
        }
        handle
    }

    unsafe fn attach_terminal_resource(
        executor: *mut LoomExecutor,
        task: *mut LoomTask,
        status: TaskStatus,
        resource: OwnedResource,
    ) -> i64 {
        assert!(matches!(
            status,
            TaskStatus::Faulted | TaskStatus::Cancelled
        ));
        let handle = resource.handle_bits();
        unsafe { detach_from_ready_queue(executor, task) };
        unsafe {
            (*task).owned_result_resources.push(resource);
            (*task).status = status;
        }
        handle
    }

    unsafe fn cancel_test_parent(executor: *mut LoomExecutor, parent: *mut LoomTask) {
        unsafe {
            if (*executor).active_task == parent {
                (*executor).active_task = ptr::null_mut();
            }
            if !terminal((*parent).status) {
                complete_terminal(&mut *executor, parent, TASK_CANCELLED);
            }
        }
    }

    #[test]
    fn io_close_accepts_only_file_and_socket_nominals() {
        assert_eq!(
            IoResourceKind::from_nominal(FILE_TYPE),
            Some(IoResourceKind::File)
        );
        assert_eq!(
            IoResourceKind::from_nominal(SOCKET_TYPE),
            Some(IoResourceKind::Socket)
        );
        assert_eq!(IoResourceKind::from_nominal(0), None);
        assert_eq!(IoResourceKind::from_nominal(u64::MAX), None);
    }

    #[test]
    fn tracked_resource_selection_keeps_windows_handle_domains_distinct() {
        assert_eq!(select_tracked_resource([]), Ok(None));
        assert_eq!(
            select_tracked_resource([(0, 0, false)]),
            Err(CloseResourceError::InvalidOwnership)
        );
        assert_eq!(
            select_tracked_resource([(0, 0, false), (1, 2, true)]),
            Ok(Some((1, 2)))
        );
        assert_eq!(
            select_tracked_resource([(0, 0, true), (1, 2, true)]),
            Err(CloseResourceError::InvalidOwnership)
        );
    }

    #[test]
    fn io_close_rejects_a_hostile_nominal_before_resource_dispatch() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let mut payload = ValueNode {
            value: ValueSlot {
                words: [2, 0, 0, INVALID_HANDLE.cast_unsigned(), 0, 0],
            },
            next: ptr::null_mut(),
        };
        let mut value = ValueSlot {
            words: [
                VALUE_TAG_RECORD,
                u64::MAX,
                1,
                0,
                (&raw mut payload) as u64,
                0,
            ],
        };

        unsafe {
            assert_eq!(
                io_close(executor, (&raw mut value).cast()),
                WAIT_INVALID_ARGUMENT
            );
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_rejects_invalid_or_inactive_boundaries() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let mut handle = INVALID_HANDLE;
        unsafe {
            assert_eq!(
                resource_close_typed_v1(ptr::null_mut(), TYPED_RESOURCE_KIND_FILE, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_FILE, ptr::null_mut()),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(
                resource_close_typed_v1(runtime, 0, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_FILE, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    fn socket_pair() -> io::Result<(TcpStream, TcpStream)> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let client = TcpStream::connect(address)?;
        let (server, _) = listener.accept()?;
        Ok((client, server))
    }

    #[test]
    fn typed_resource_close_directly_closes_and_writes_back_an_untracked_socket() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let (socket, mut peer) = socket_pair().expect("create socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let mut handle = crate::platform::socket_handle_bits(&socket);
        std::mem::forget(socket);

        unsafe {
            assert_eq!(crate::gc::activate_runtime_v1(runtime), WAIT_OK);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle,),
                TYPED_RESOURCE_CLOSE_OK
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_peer_closed(&mut peer);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle,),
                TYPED_RESOURCE_CLOSE_OK
            );
            assert_eq!(crate::gc::deactivate_runtime_v1(runtime), WAIT_OK);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_removes_async_ownership_without_scheduling() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let mut handle = crate::platform::socket_handle_bits(&socket);

        unsafe {
            let task = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!task.is_null());
            crate::gc::enter_executor(executor);
            assert_eq!(
                store_resource_result(task, SOCKET_TYPE, socket.into()),
                TASK_COMPLETED
            );
            assert_eq!((*task).owned_result_resources.len(), 1);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle,),
                TYPED_RESOURCE_CLOSE_OK
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert!((*task).owned_result_resources.is_empty());
            assert_peer_closed(&mut peer);
            crate::gc::leave_executor();
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_rejects_a_tracked_opposite_kind_without_closing_it() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let mut handle = crate::platform::socket_handle_bits(&socket);

        unsafe {
            let task = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!task.is_null());
            crate::gc::enter_executor(executor);
            assert_eq!(
                store_resource_result(task, SOCKET_TYPE, socket.into()),
                TASK_COMPLETED
            );
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_FILE, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!((*task).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_OK
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert!((*task).owned_result_resources.is_empty());
            assert_peer_closed(&mut peer);
            crate::gc::leave_executor();
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn root_typed_take_keeps_a_real_resource_in_the_executor_task_registry() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create root resource pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let root = create_typed_resource_task(executor);
            let expected = complete_typed_resource(executor, root, socket.into());
            let mut handle = INVALID_HANDLE;

            assert_eq!(
                typed_task_take_result_v1(
                    root,
                    (&raw mut handle).cast(),
                    size_of::<u32>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!((*root).owned_result_resources.len(), 1);
            assert!(!(*executor).retired_tasks.contains(&root));
            assert_peer_still_connected(&mut peer);

            assert_eq!(
                typed_task_take_result_v1(
                    root,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(handle, expected);
            assert_eq!((*root).owned_result_resources.len(), 1);
            reap_retired_tasks(&mut *executor, root);
            assert_eq!(executor_live_tasks(executor), 1);
            assert_peer_still_connected(&mut peer);

            enter_executor(executor);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_OK
            );
            leave_executor();
            assert_eq!(handle, INVALID_HANDLE);
            assert!((*root).owned_result_resources.is_empty());
            assert_peer_closed(&mut peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn ownerless_root_take_keeps_the_resource_until_executor_teardown() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create root teardown pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let root = create_typed_resource_task(executor);
            let expected = complete_typed_resource(executor, root, socket.into());
            let mut handle = INVALID_HANDLE;
            assert_eq!(
                typed_task_take_result_v1(
                    root,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(handle, expected);
            assert_eq!((*root).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
            assert_peer_closed(&mut peer);
        }
    }

    #[test]
    fn typed_take_rejects_an_owned_child_outside_the_active_join_transactionally() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create sibling resource pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            let sibling = create_typed_resource_task(executor);
            let expected = complete_typed_resource(executor, sibling, socket.into());
            assert_eq!(
                (*parent)
                    .owned_children
                    .iter()
                    .filter(|child| **child == sibling)
                    .count(),
                1
            );
            assert!(!(*parent).join_children.contains(&sibling));

            let mut handle = INVALID_HANDLE;
            assert_eq!(
                typed_task_take_result_v1(
                    sibling,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!((*sibling).owned_result_resources.len(), 1);
            assert_eq!(
                (*sibling)
                    .typed
                    .as_ref()
                    .expect("typed sibling")
                    .frame_pointer()
                    .cast::<i64>()
                    .read(),
                expected
            );
            assert!(!(*executor).retired_tasks.contains(&sibling));
            assert_peer_still_connected(&mut peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
            assert_peer_closed(&mut peer);
        }
    }

    #[test]
    fn typed_take_rejects_a_child_before_the_join_settles_transactionally() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create unsettled resource pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            let child = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            let expected = complete_typed_resource(executor, child, socket.into());

            let mut handle = INVALID_HANDLE;
            assert_eq!(
                typed_task_take_result_v1(
                    child,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!((*parent).owned_children, vec![child]);
            assert_eq!((*parent).join_children, vec![child]);
            assert_eq!((*child).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            assert_eq!(task_suspend_join(executor, parent), 0);
            let sentinel = ptr::dangling_mut::<c_void>();
            let mut code = sentinel;
            let mut message = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    child,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                    &raw mut code,
                    &raw mut message,
                ),
                TYPED_TASK_STATUS_INVALID
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!(code, sentinel);
            assert_eq!(message, sentinel);
            assert_eq!(
                typed_task_take_result_v1(
                    child,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(handle, expected);
            assert_eq!((*parent).owned_result_resources.len(), 1);

            enter_executor(executor);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_OK
            );
            leave_executor();
            assert_peer_closed(&mut peer);
            cancel_test_parent(executor, parent);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn fault_and_cancellation_terminal_cleanup_close_live_result_ledgers() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (fault_socket, mut fault_peer) = socket_pair().expect("create fault pair");
        let (cancel_socket, mut cancel_peer) = socket_pair().expect("create cancellation pair");
        fault_peer
            .set_nonblocking(true)
            .expect("make fault peer nonblocking");
        cancel_peer
            .set_nonblocking(true)
            .expect("make cancellation peer nonblocking");

        unsafe {
            let faulted = create_typed_resource_task(executor);
            detach_from_ready_queue(executor, faulted);
            (*faulted).status = TaskStatus::Running;
            (*faulted).owned_result_resources.push(fault_socket.into());
            record_primary_task_fault(
                &mut *faulted,
                "FixtureFault".into(),
                "fixture failed".into(),
                String::new(),
            );
            complete_terminal(&mut *executor, faulted, TASK_FAULTED);
            assert!((*faulted).status == TaskStatus::Faulted);
            assert!((*faulted).owned_result_resources.is_empty());
            assert_peer_closed(&mut fault_peer);

            let cancelled = create_typed_resource_task(executor);
            detach_from_ready_queue(executor, cancelled);
            (*cancelled).status = TaskStatus::Running;
            (*cancelled).cancel_requested = true;
            (*cancelled)
                .owned_result_resources
                .push(cancel_socket.into());
            complete_terminal(&mut *executor, cancelled, TASK_CANCELLED);
            assert!((*cancelled).status == TaskStatus::Cancelled);
            assert!((*cancelled).owned_result_resources.is_empty());
            assert_peer_closed(&mut cancel_peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_failure_preserves_an_untracked_handle() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        #[cfg(unix)]
        let mut handle = -2_i64;
        #[cfg(windows)]
        let mut handle = 0_i64;
        let original = handle;

        unsafe {
            assert_eq!(crate::gc::activate_runtime_v1(runtime), WAIT_OK);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_FILE, &raw mut handle),
                TYPED_RESOURCE_CLOSE_FAILED
            );
            assert_eq!(handle, original);
            assert_eq!(crate::gc::deactivate_runtime_v1(runtime), WAIT_OK);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_all_take_transfers_every_real_resource_before_child_reaping() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (left_socket, mut left_peer) = socket_pair().expect("create left resource pair");
        let (right_socket, mut right_peer) = socket_pair().expect("create right resource pair");
        left_peer
            .set_nonblocking(true)
            .expect("make left peer nonblocking");
        right_peer
            .set_nonblocking(true)
            .expect("make right peer nonblocking");

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            let left = create_typed_resource_task(executor);
            let right = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, left), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, right), WAIT_OK);
            let expected_left = complete_typed_resource(executor, left, left_socket.into());
            let expected_right = complete_typed_resource(executor, right, right_socket.into());

            assert_eq!(task_suspend_join(executor, parent), 0);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);
            let mut left_handle = INVALID_HANDLE;
            let mut right_handle = INVALID_HANDLE;
            for (child, output) in [(left, &raw mut left_handle), (right, &raw mut right_handle)] {
                assert_eq!(
                    typed_task_take_result_v1(
                        child,
                        output.cast(),
                        size_of::<i64>() as u64,
                        align_of::<i64>() as u64,
                    ),
                    TYPED_TASK_OK
                );
            }
            assert_eq!([left_handle, right_handle], [expected_left, expected_right]);
            assert_eq!((*parent).owned_result_resources.len(), 2);
            assert!((*left).owned_result_resources.is_empty());
            assert!((*right).owned_result_resources.is_empty());
            assert!((*parent).owned_children.is_empty());
            assert!((*parent).join_children.is_empty());

            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 1);
            assert_peer_still_connected(&mut left_peer);
            assert_peer_still_connected(&mut right_peer);

            enter_executor(executor);
            for handle in [&raw mut left_handle, &raw mut right_handle] {
                assert_eq!(
                    resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, handle),
                    TYPED_RESOURCE_CLOSE_OK
                );
            }
            leave_executor();
            assert_peer_closed(&mut left_peer);
            assert_peer_closed(&mut right_peer);
            cancel_test_parent(executor, parent);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn transferred_result_resource_closes_when_its_owner_faults() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create transferred fault pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            let child = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            complete_typed_resource(executor, child, socket.into());
            assert_eq!(task_suspend_join(executor, parent), 0);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);

            let mut handle = INVALID_HANDLE;
            assert_eq!(
                typed_task_take_result_v1(
                    child,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!((*parent).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            (*executor).active_task = ptr::null_mut();
            record_primary_task_fault(
                &mut *parent,
                "FixtureFault".into(),
                "owner failed".into(),
                String::new(),
            );
            complete_terminal(&mut *executor, parent, TASK_FAULTED);
            assert!((*parent).status == TaskStatus::Faulted);
            assert!((*parent).owned_result_resources.is_empty());
            assert_peer_closed(&mut peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn typed_outcome_transfers_only_completed_resources_and_is_transactional() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (completed_socket, mut completed_peer) =
            socket_pair().expect("create completed resource pair");
        let (faulted_socket, mut faulted_peer) =
            socket_pair().expect("create faulted resource pair");
        let (cancelled_socket, mut cancelled_peer) =
            socket_pair().expect("create cancelled resource pair");
        for peer in [&mut completed_peer, &mut faulted_peer, &mut cancelled_peer] {
            peer.set_nonblocking(true).expect("make peer nonblocking");
        }

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            assert_eq!(
                task_prepare_join(executor, parent, TASK_JOIN_SETTLED),
                WAIT_OK
            );
            let completed = create_typed_resource_task(executor);
            let faulted = create_typed_resource_task(executor);
            let cancelled = create_typed_resource_task(executor);
            for child in [completed, faulted, cancelled] {
                assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            }
            let expected = complete_typed_resource(executor, completed, completed_socket.into());
            record_primary_task_fault(
                &mut *faulted,
                "FixtureFault".into(),
                "fixture failed".into(),
                String::new(),
            );
            attach_terminal_resource(
                executor,
                faulted,
                TaskStatus::Faulted,
                faulted_socket.into(),
            );
            attach_terminal_resource(
                executor,
                cancelled,
                TaskStatus::Cancelled,
                cancelled_socket.into(),
            );
            assert_eq!(task_suspend_join(executor, parent), 0);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);

            let sentinel = ptr::dangling_mut::<c_void>();
            let mut handle = INVALID_HANDLE;
            assert_eq!(
                typed_task_take_result_v1(
                    completed,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!((*completed).owned_result_resources.len(), 1);
            let mut aliased_text = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    completed,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                    &raw mut aliased_text,
                    &raw mut aliased_text,
                ),
                TYPED_TASK_STATUS_INVALID
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!(aliased_text, sentinel);
            assert_eq!((*completed).owned_result_resources.len(), 1);
            assert!(!(*executor).retired_tasks.contains(&completed));
            assert_peer_still_connected(&mut completed_peer);

            enter_executor(executor);
            let mut completed_code = sentinel;
            let mut completed_message = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    completed,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                    &raw mut completed_code,
                    &raw mut completed_message,
                ),
                TASK_COMPLETED
            );
            assert_eq!(handle, expected);
            assert!(completed_code.is_null());
            assert!(completed_message.is_null());

            let mut fault_value = 17_i64;
            let mut fault_code = ptr::null_mut();
            let mut fault_message = ptr::null_mut();
            assert_eq!(
                typed_task_take_outcome_v1(
                    faulted,
                    (&raw mut fault_value).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                    &raw mut fault_code,
                    &raw mut fault_message,
                ),
                TASK_FAULTED
            );
            assert_eq!(fault_value, 17);
            assert_eq!(
                crate::text::text_bytes(fault_code).unwrap(),
                b"FixtureFault"
            );
            assert_eq!(
                crate::text::text_bytes(fault_message).unwrap(),
                b"fixture failed"
            );

            let mut cancelled_value = 19_i64;
            let mut cancelled_code = sentinel;
            let mut cancelled_message = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    cancelled,
                    (&raw mut cancelled_value).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                    &raw mut cancelled_code,
                    &raw mut cancelled_message,
                ),
                TASK_CANCELLED
            );
            leave_executor();
            assert_eq!(cancelled_value, 19);
            assert!(cancelled_code.is_null());
            assert!(cancelled_message.is_null());

            assert_eq!((*parent).owned_result_resources.len(), 1);
            assert!((*completed).owned_result_resources.is_empty());
            assert_eq!((*faulted).owned_result_resources.len(), 1);
            assert_eq!((*cancelled).owned_result_resources.len(), 1);
            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 1);
            assert_peer_still_connected(&mut completed_peer);
            assert_peer_closed(&mut faulted_peer);
            assert_peer_closed(&mut cancelled_peer);

            enter_executor(executor);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_OK
            );
            leave_executor();
            assert_peer_closed(&mut completed_peer);
            cancel_test_parent(executor, parent);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_any_loser_resource_is_closed_instead_of_transferred() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (winner_socket, mut winner_peer) = socket_pair().expect("create winner resource pair");
        let (loser_socket, mut loser_peer) = socket_pair().expect("create loser resource pair");
        winner_peer
            .set_nonblocking(true)
            .expect("make winner peer nonblocking");
        loser_peer
            .set_nonblocking(true)
            .expect("make loser peer nonblocking");

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
            let winner = create_typed_resource_task(executor);
            let loser = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, winner), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, loser), WAIT_OK);
            let expected = complete_typed_resource(executor, winner, winner_socket.into());
            complete_typed_resource(executor, loser, loser_socket.into());

            assert_eq!(task_suspend_join(executor, parent), 0);
            let mut handle = INVALID_HANDLE;
            assert!(
                !(*parent)
                    .typed
                    .as_ref()
                    .expect("typed parent")
                    .join_winner_finalized
            );
            assert_eq!(
                typed_task_take_result_v1(
                    winner,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_HANDLE);
            assert_eq!((*parent).owned_children, vec![winner, loser]);
            assert_eq!((*parent).join_children, vec![winner, loser]);
            assert_eq!((*winner).owned_result_resources.len(), 1);
            assert_eq!((*loser).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut winner_peer);
            assert_peer_still_connected(&mut loser_peer);

            assert_eq!(task_join_step(parent), TASK_COMPLETED);
            assert_eq!((*parent).owned_children, vec![winner]);
            assert_eq!((*parent).join_children, vec![winner]);
            assert!((*parent).owned_result_resources.is_empty());
            assert_eq!((*winner).owned_result_resources.len(), 1);
            assert!((*loser).owned_result_resources.is_empty());
            assert_peer_still_connected(&mut winner_peer);
            assert_peer_closed(&mut loser_peer);

            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 2);

            assert_eq!(
                typed_task_take_result_v1(
                    winner,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(handle, expected);
            assert_eq!((*parent).owned_result_resources.len(), 1);
            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 1);
            assert_peer_still_connected(&mut winner_peer);

            enter_executor(executor);
            assert_eq!(
                resource_close_typed_v1(runtime, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_OK
            );
            leave_executor();
            assert_peer_closed(&mut winner_peer);
            cancel_test_parent(executor, parent);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn unconsumed_typed_resource_closes_at_disposal_before_child_reaping() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create unconsumed resource pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let parent = create_typed_resource_task(executor);
            activate_typed_task(executor, parent);
            let child = create_typed_resource_task(executor);
            complete_typed_resource(executor, child, socket.into());
            {
                let typed = (*parent).typed.as_mut().expect("typed parent fixture");
                typed.frame_pointer().cast::<i64>().write(7);
                typed.result_initialized = true;
                typed.root_state = typed.completed_root_state;
            }
            (*executor).active_task = ptr::null_mut();
            complete_terminal(&mut *executor, parent, TASK_COMPLETED);
            assert!((*parent).status == TaskStatus::Completed);
            assert!((*parent).owned_result_resources.is_empty());
            assert!((*child).owned_result_resources.is_empty());
            assert!((*executor).retired_tasks.contains(&child));
            assert_peer_closed(&mut peer);

            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 1);
            let mut result = 0_i64;
            assert_eq!(
                typed_task_take_result_v1(
                    parent,
                    (&raw mut result).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(result, 7);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_result_disposal_closes_remaining_resources_after_a_disposer_defect() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create disposal resource pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let mut descriptor = typed_resource_descriptor();
            descriptor.dispose_result = Some(typed_invalid_dispose_fixture);
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert!(!task.is_null());
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            complete_typed_resource(executor, task, socket.into());
            assert_eq!((*task).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            assert_eq!(
                dispose_typed_result(&mut *executor, task),
                CleanupOutcome::Defect
            );
            assert!((*task).owned_result_resources.is_empty());
            assert_peer_closed(&mut peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    fn assert_peer_still_connected(peer: &mut TcpStream) {
        let mut byte = [0_u8; 1];
        let result = peer.read(&mut byte);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    }

    fn assert_peer_closed(peer: &mut TcpStream) {
        let mut byte = [0_u8; 1];
        for _ in 0..100 {
            match peer.read(&mut byte) {
                Ok(0) => return,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::BrokenPipe
                            | io::ErrorKind::NotConnected
                    ) =>
                {
                    return;
                }
                result => panic!("expected closed socket peer, got {result:?}"),
            }
        }
        panic!("socket peer did not observe closure before the test deadline");
    }

    #[test]
    fn pending_io_task_owns_a_duplicate_and_cancellation_closes_it() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let text = b"not written";

        unsafe {
            let task = socket_write_text(
                executor,
                crate::platform::socket_handle_bits(&socket),
                text.as_ptr(),
                text.len() as u64,
            );
            assert!(!task.is_null());
            drop(socket);
            assert_peer_still_connected(&mut peer);

            assert_eq!(task_cancel(executor, task), WAIT_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert_peer_closed(&mut peer);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn join_transfers_the_winner_and_reaps_unconsumed_resources() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (winner_socket, mut winner_peer) = socket_pair().expect("create winner pair");
        let (loser_socket, mut loser_peer) = socket_pair().expect("create loser pair");
        winner_peer
            .set_nonblocking(true)
            .expect("make winner peer nonblocking");
        loser_peer
            .set_nonblocking(true)
            .expect("make loser peer nonblocking");

        unsafe {
            let parent = task_spawn(executor, Some(complete_fixture), 1, 0);
            let winner = task_spawn(executor, Some(complete_fixture), 1, 0);
            let loser = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!parent.is_null() && !winner.is_null() && !loser.is_null());

            (*winner).owner = parent;
            (*loser).owner = parent;
            (*parent).owned_children.extend([winner, loser]);
            (*parent).join_children.extend([winner, loser]);
            (*parent).join_winner = 0;
            (*winner).status = TaskStatus::Completed;
            (*loser).status = TaskStatus::Completed;

            crate::gc::enter_executor(executor);
            assert_eq!(
                store_resource_result(winner, SOCKET_TYPE, winner_socket.into()),
                TASK_COMPLETED
            );
            assert_eq!(
                store_resource_result(loser, SOCKET_TYPE, loser_socket.into()),
                TASK_COMPLETED
            );
            crate::gc::leave_executor();

            let destination = task_slot(parent, 0).cast::<ValueSlot>();
            assert_eq!(
                task_write_join_result(parent, destination.cast(), 0),
                WAIT_OK
            );
            assert_eq!((*parent).owned_result_resources.len(), 1);
            assert!((*winner).owned_result_resources.is_empty());
            assert_eq!((*loser).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut winner_peer);
            assert_peer_still_connected(&mut loser_peer);

            assert_eq!(io_close(executor, destination.cast()), WAIT_OK);
            assert_peer_closed(&mut winner_peer);

            reap_retired_tasks(&mut *executor, parent);
            assert_peer_closed(&mut loser_peer);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn aggregate_join_transfers_every_resource_to_the_awaiting_task() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (left_socket, mut left_peer) = socket_pair().expect("create left pair");
        let (right_socket, mut right_peer) = socket_pair().expect("create right pair");
        left_peer
            .set_nonblocking(true)
            .expect("make left peer nonblocking");
        right_peer
            .set_nonblocking(true)
            .expect("make right peer nonblocking");

        unsafe {
            let parent = task_spawn(executor, Some(complete_fixture), 1, 0);
            let left = task_spawn(executor, Some(complete_fixture), 1, 0);
            let right = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!parent.is_null() && !left.is_null() && !right.is_null());

            (*left).owner = parent;
            (*right).owner = parent;
            (*parent).owned_children.extend([left, right]);
            (*parent).join_children.extend([left, right]);
            (*left).status = TaskStatus::Completed;
            (*right).status = TaskStatus::Completed;

            crate::gc::enter_executor(executor);
            assert_eq!(
                store_resource_result(left, SOCKET_TYPE, left_socket.into()),
                TASK_COMPLETED
            );
            assert_eq!(
                store_resource_result(right, SOCKET_TYPE, right_socket.into()),
                TASK_COMPLETED
            );
            crate::gc::leave_executor();

            let destination = task_slot(parent, 0).cast::<ValueSlot>();
            crate::gc::enter_executor(executor);
            assert_eq!(
                task_write_join_result(parent, destination.cast(), JOIN_RESULT_TUPLE),
                WAIT_OK
            );
            crate::gc::leave_executor();
            assert_eq!((*parent).owned_result_resources.len(), 2);
            assert!((*left).owned_result_resources.is_empty());
            assert!((*right).owned_result_resources.is_empty());

            let first = (*destination).words[4] as *mut ValueNode;
            assert!(!first.is_null());
            let second = (*first).next;
            assert!(!second.is_null());
            assert_eq!(
                io_close(executor, (&raw mut (*first).value).cast()),
                WAIT_OK
            );
            assert_eq!(
                io_close(executor, (&raw mut (*second).value).cast()),
                WAIT_OK
            );
            assert_peer_closed(&mut left_peer);
            assert_peer_closed(&mut right_peer);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn io_results_and_join_outcomes_survive_every_allocator_collection() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());

        unsafe {
            let parent = task_spawn(executor, Some(complete_fixture), 1, 0);
            let success = task_spawn(executor, Some(complete_fixture), 1, 0);
            let io_error = task_spawn(executor, Some(complete_fixture), 1, 0);
            let fault = task_spawn(executor, Some(complete_fixture), 1, 0);
            let cancelled = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(
                !parent.is_null()
                    && !success.is_null()
                    && !io_error.is_null()
                    && !fault.is_null()
                    && !cancelled.is_null()
            );
            for child in [success, io_error, fault, cancelled] {
                (*child).owner = parent;
                (*parent).owned_children.push(child);
                (*parent).join_children.push(child);
            }
            (*success).io_fallible = true;
            (*io_error).io_fallible = true;
            (*fault).fault_code = "FixtureFault".to_owned();
            (*fault).fault_message = "managed failure message".to_owned();

            crate::gc::enter_executor(executor);
            (*runtime).heap.collect_before_every_allocation = true;
            assert_eq!(
                store_text_result(success, executor, b"managed success", "FixtureReadFault"),
                TASK_COMPLETED,
            );
            (*success).status = TaskStatus::Completed;
            assert_eq!(
                complete_io_error(io_error, 3, "FixtureIoFault", "managed I/O error"),
                TASK_COMPLETED,
            );
            (*io_error).status = TaskStatus::Completed;
            (*fault).status = TaskStatus::Faulted;
            (*cancelled).status = TaskStatus::Cancelled;

            let destination = task_slot(parent, 0).cast::<ValueSlot>();
            assert_eq!(
                task_write_join_result(parent, destination.cast(), JOIN_RESULT_OUTCOME_TUPLE,),
                WAIT_OK,
            );
            assert_eq!((*destination).words[0], VALUE_TAG_TUPLE);
            assert_eq!((*destination).words[2], 4);
            let mut node = (*destination).words[4] as *const ValueNode;
            for variant in [
                TASK_OUTCOME_COMPLETED,
                TASK_OUTCOME_COMPLETED,
                TASK_OUTCOME_FAULTED,
                TASK_OUTCOME_CANCELLED,
            ] {
                assert!(!node.is_null());
                assert_eq!((*node).value.words[0], VALUE_TAG_ENUM);
                assert_eq!((*node).value.words[1], TASK_OUTCOME_TYPE);
                assert_eq!((*node).value.words[2], variant);
                node = (*node).next;
            }
            assert!(node.is_null());
            assert!((*runtime).heap.collections > 10);
            (*runtime).heap.collect_before_every_allocation = false;
            crate::gc::leave_executor();

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }
}

#[cfg(test)]
mod typed_task_tests {
    use std::ffi::c_void;
    use std::mem::{align_of, offset_of, size_of};
    use std::ptr;
    use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};

    use loom_runtime_abi::{
        LoomGcObjectDescriptor, LoomGcRootDescriptor, LoomGcRootFrame, LoomGcTypedRootDescriptor,
        LoomGcTypedRootFrame, LoomTypedCoroutineDescriptor, LoomTypedTaskFaultView,
        SHADOW_STACK_ABI_VERSION, TYPED_GC_ABI_VERSION, TYPED_SHADOW_STACK_ABI_VERSION,
        TYPED_TASK_ABI_VERSION,
    };

    use super::*;
    use crate::GC_OK;
    use crate::gc::{
        activate_runtime_v1, active_runtime_pointer, collect, enter_executor, leave_executor,
        root_pop_v1, root_push_v1, typed_alloc_v1, typed_root_pop_v1, typed_root_push_v1,
    };
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy, executor_register};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};

    unsafe extern "C" fn complete_u64(
        task: *mut c_void,
        _executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        if unsafe { typed_task_set_root_state_v1(task.cast(), 0) } != TYPED_TASK_OK {
            return TASK_FAULTED;
        }
        unsafe { frame.cast::<u64>().write(42) };
        if unsafe { typed_task_publish_result_v1(task.cast()) } == TYPED_TASK_OK {
            TASK_COMPLETED
        } else {
            TASK_FAULTED
        }
    }

    unsafe extern "C" fn cancel_noop(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_CANCELLED
    }

    fn descriptor(
        resume: LoomTypedTaskCallback,
        cancel: LoomTypedTaskCallback,
    ) -> LoomTypedCoroutineDescriptor {
        LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(resume),
            cancel: Some(cancel),
            dispose_result: None,
            frame_size: size_of::<u64>() as u64,
            frame_align: align_of::<u64>() as u64,
            result_offset: 0,
            result_size: size_of::<u64>() as u64,
            result_align: align_of::<u64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        }
    }

    unsafe fn with_sync_and_typed_roots(action: impl FnOnce() -> bool) -> bool {
        let bitmap = [1_u64];
        let sync_descriptor = LoomGcRootDescriptor {
            abi_version: SHADOW_STACK_ABI_VERSION,
            flags: 0,
            slot_count: 1,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        let mut sync_value = ValueSlot::default();
        let sync_slots = [(&raw mut sync_value).cast::<c_void>()];
        let mut sync_frame = LoomGcRootFrame {
            abi_version: SHADOW_STACK_ABI_VERSION,
            flags: 0,
            state: 0,
            descriptor: &raw const sync_descriptor,
            slots: sync_slots.as_ptr(),
            previous: ptr::null_mut(),
        };
        let typed_descriptor = LoomGcTypedRootDescriptor {
            abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
            flags: 0,
            slot_count: 1,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        let mut typed_value = ptr::null_mut::<c_void>();
        let typed_slots = [(&raw mut typed_value).cast::<c_void>()];
        let mut typed_frame = LoomGcTypedRootFrame {
            abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
            flags: 0,
            state: 0,
            descriptor: &raw const typed_descriptor,
            slots: typed_slots.as_ptr(),
            previous: ptr::null_mut(),
        };

        if unsafe { root_push_v1(&raw mut sync_frame) } != GC_OK {
            return false;
        }
        if unsafe { typed_root_push_v1(&raw mut typed_frame) } != GC_OK {
            let _ = unsafe { root_pop_v1(&raw mut sync_frame) };
            return false;
        }
        let action_ok = action();
        let typed_pop_ok = unsafe { typed_root_pop_v1(&raw mut typed_frame) } == GC_OK;
        let sync_pop_ok = unsafe { root_pop_v1(&raw mut sync_frame) } == GC_OK;
        action_ok && typed_pop_ok && sync_pop_ok
    }

    fn runtime_and_executor() -> (*mut crate::runtime::LoomRuntime, *mut LoomExecutor) {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        (runtime, executor)
    }

    unsafe fn destroy(runtime: *mut crate::runtime::LoomRuntime, executor: *mut LoomExecutor) {
        unsafe {
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_timer_task_completes_with_a_zero_sized_unit_result() {
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let timer = typed_timer_task_create_v1(executor, crate::reactor::wait_now_ns());
            assert!(!timer.is_null());
            assert_eq!(executor_run(executor, timer), TASK_COMPLETED);
            assert_eq!(typed_task_status_v1(timer), TASK_COMPLETED);
            assert_eq!(
                typed_task_take_result_v1(timer, ptr::null_mut(), 0, 1),
                TYPED_TASK_OK
            );
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_timer_registration_failure_records_a_primary_task_fault() {
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let timer = typed_timer_task_create_v1(executor, u64::MAX);
            assert!(!timer.is_null());
            (*timer).wait_source.reserved = 1;
            assert_eq!(executor_run(executor, timer), TASK_FAULTED);
            let mut fault = LoomTypedTaskFaultView::default();
            assert_eq!(
                typed_task_fault_view_v1(timer, &raw mut fault),
                TYPED_TASK_OK
            );
            let bytes = |view: LoomByteView| {
                if view.length == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(
                        view.data,
                        usize::try_from(view.length).expect("fault byte view fits usize"),
                    )
                }
            };
            assert_eq!(
                bytes(fault.code),
                TYPED_TIMER_REGISTRATION_FAULT_CODE.as_bytes()
            );
            assert_eq!(
                bytes(fault.message),
                TYPED_TIMER_REGISTRATION_FAULT_MESSAGE.as_bytes()
            );
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_timer_cancellation_drops_registration_and_ignores_stale_ready() {
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let timer = typed_timer_task_create_v1(executor, crate::reactor::wait_now_ns());
            assert!(!timer.is_null());
            (*executor).runnable.clear();
            (*timer).queued = false;
            (*timer).status = TaskStatus::Running;
            (*executor).active_task = timer;
            assert_eq!(run_typed_task_step(executor, timer), TASK_PENDING);
            (*executor).active_task = ptr::null_mut();
            assert!((*timer).status == TaskStatus::Waiting);
            assert_eq!((*timer).waits.len(), 1);
            assert!(has_registrations(&*executor));

            let mut ready = 0;
            assert_eq!(wait_for_scheduler(executor, &raw mut ready), WAIT_OK);
            assert_eq!(ready, 1);
            assert!(!has_registrations(&*executor));
            assert_eq!((*timer).waits.len(), 1);

            assert_eq!(typed_task_request_cancel_v1(executor, timer), TYPED_TASK_OK);
            assert!((*timer).waits.is_empty());
            consume_notifications(executor);
            assert_eq!(
                (*executor)
                    .runnable
                    .iter()
                    .filter(|candidate| **candidate == timer)
                    .count(),
                1
            );
            assert_eq!(executor_run(executor, timer), TASK_CANCELLED);
            assert_eq!(typed_task_status_v1(timer), TASK_CANCELLED);
            destroy(runtime, executor);
        }
    }

    unsafe fn prime_pending_parent(
        executor: *mut LoomExecutor,
        parent: *mut LoomTask,
        resume: LoomTypedTaskCallback,
    ) -> Vec<*mut LoomTask> {
        unsafe {
            (*executor)
                .runnable
                .retain(|candidate| *candidate != parent);
            (*parent).queued = false;
            (*parent).status = TaskStatus::Running;
            (*executor).active_task = parent;
            let frame = (*parent)
                .typed
                .as_ref()
                .expect("typed test parent")
                .frame_pointer();
            enter_executor(executor);
            let step = resume(parent.cast(), executor.cast(), frame);
            leave_executor();
            assert_eq!(step, TASK_PENDING);
            (*executor).active_task = ptr::null_mut();
            (*parent).status = TaskStatus::Waiting;
            let children = (*parent).owned_children.clone();
            for child in &children {
                (*executor).runnable.retain(|candidate| candidate != child);
                (**child).queued = false;
                (**child).status = TaskStatus::Waiting;
            }
            children
        }
    }

    unsafe fn complete_typed_u64_for_test(task: *mut LoomTask, value: u64) {
        unsafe {
            let typed = (*task).typed.as_mut().expect("typed test child");
            typed.frame_pointer().cast::<u64>().write(value);
            typed.result_initialized = true;
            typed.root_state = typed.completed_root_state;
            (*task).status = TaskStatus::Completed;
        }
    }

    unsafe fn activate_test_task(executor: *mut LoomExecutor, task: *mut LoomTask) {
        let positions = unsafe {
            (*executor)
                .runnable
                .iter()
                .enumerate()
                .filter_map(|(index, candidate)| (*candidate == task).then_some(index))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            positions.len(),
            1,
            "test task must have one ready-queue entry"
        );
        assert_eq!(
            unsafe { (*executor).runnable.remove(positions[0]) },
            Some(task)
        );
        unsafe {
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
        }
    }

    unsafe fn create_typed_u64_child(
        executor: *mut LoomExecutor,
        descriptor: &LoomTypedCoroutineDescriptor,
    ) -> *mut LoomTask {
        let child = unsafe { typed_task_create_v1(executor, descriptor) };
        assert!(!child.is_null());
        assert_eq!(unsafe { typed_task_initialize_v1(child, 0) }, TYPED_TASK_OK);
        assert_eq!(
            unsafe { typed_task_publish_v1(executor, child) },
            TYPED_TASK_OK
        );
        child
    }

    unsafe fn create_initialized_typed_task(
        executor: *mut LoomExecutor,
        descriptor: &LoomTypedCoroutineDescriptor,
    ) -> *mut LoomTask {
        let task = unsafe { typed_task_create_v1(executor, descriptor) };
        assert!(!task.is_null());
        assert_eq!(unsafe { typed_task_initialize_v1(task, 0) }, TYPED_TASK_OK);
        task
    }

    unsafe fn executor_task_order(executor: *const LoomExecutor) -> Vec<*mut LoomTask> {
        unsafe {
            (*executor)
                .tasks
                .iter()
                .map(|task| (&raw const **task).cast_mut())
                .collect()
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn typed_task_take_outcome_consumes_every_settled_terminal_case_exactly() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let parent = create_initialized_typed_task(executor, &descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);

            let completed = create_typed_u64_child(executor, &descriptor);
            let faulted = create_typed_u64_child(executor, &descriptor);
            let cancelled = create_typed_u64_child(executor, &descriptor);
            complete_typed_u64_for_test(completed, 0xfeed_beef);
            record_primary_task_fault(
                &mut *faulted,
                "AssertionFault".into(),
                "assertion failed".into(),
                "fault detail is not part of TaskFault".into(),
            );
            (*faulted).status = TaskStatus::Faulted;
            (*cancelled).status = TaskStatus::Cancelled;

            // Being a structured child is insufficient: outcome consumption
            // is authorized only after the active join publishes the handle.
            let mut untouched_value = 7_u64;
            let sentinel = ptr::dangling_mut::<c_void>();
            let mut untouched_code = sentinel;
            let mut untouched_message = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    completed,
                    (&raw mut untouched_value).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                    &raw mut untouched_code,
                    &raw mut untouched_message,
                ),
                TYPED_TASK_STATUS_INVALID
            );
            assert_eq!(untouched_value, 7);
            assert_eq!(untouched_code, sentinel);
            assert_eq!(untouched_message, sentinel);

            assert_eq!(
                task_prepare_join(executor, parent, TASK_JOIN_SETTLED),
                WAIT_OK
            );
            for child in [completed, faulted, cancelled] {
                assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            }
            (*parent).status = TaskStatus::Waiting;
            update_join(&mut *executor, parent);
            activate_test_task(executor, parent);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);
            assert_eq!((*parent).join_children, vec![completed, faulted, cancelled]);

            enter_executor(executor);
            (*runtime).heap.collect_before_every_allocation = true;

            let mut completed_value = 0_u64;
            let mut completed_code = sentinel;
            let mut completed_message = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    completed,
                    (&raw mut completed_value).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                    &raw mut completed_code,
                    &raw mut completed_message,
                ),
                TASK_COMPLETED
            );
            assert_eq!(completed_value, 0xfeed_beef);
            assert!(completed_code.is_null());
            assert!(completed_message.is_null());

            let mut fault_value = 19_u64;
            let mut fault_code = ptr::null_mut();
            let mut fault_message = ptr::null_mut();
            assert_eq!(
                typed_task_take_outcome_v1(
                    faulted,
                    (&raw mut fault_value).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                    &raw mut fault_code,
                    &raw mut fault_message,
                ),
                TASK_FAULTED
            );
            assert_eq!(fault_value, 19, "Faulted has no T payload");
            assert_eq!(
                crate::text::text_bytes(fault_code).unwrap(),
                b"AssertionFault"
            );
            assert_eq!(
                crate::text::text_bytes(fault_message).unwrap(),
                b"assertion failed"
            );

            let mut cancelled_value = 23_u64;
            let mut cancelled_code = sentinel;
            let mut cancelled_message = sentinel;
            assert_eq!(
                typed_task_take_outcome_v1(
                    cancelled,
                    (&raw mut cancelled_value).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                    &raw mut cancelled_code,
                    &raw mut cancelled_message,
                ),
                TASK_CANCELLED
            );
            assert_eq!(cancelled_value, 23, "Cancelled has no T payload");
            assert!(cancelled_code.is_null());
            assert!(cancelled_message.is_null());

            (*runtime).heap.collect_before_every_allocation = false;
            leave_executor();
            assert!((*parent).owned_children.is_empty());
            assert!((*parent).join_children.is_empty());
            for child in [completed, faulted, cancelled] {
                assert!((*child).owner.is_null());
                assert!((*executor).retired_tasks.contains(&child));
            }
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_adopting_publish_preserves_input_order_and_reorders_stable_task_handles() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let parent = create_initialized_typed_task(executor, &descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);

            let left = create_typed_u64_child(executor, &descriptor);
            let untouched = create_typed_u64_child(executor, &descriptor);
            let right = create_typed_u64_child(executor, &descriptor);
            let composite = create_initialized_typed_task(executor, &descriptor);
            let children = [right, left];

            assert_eq!(
                typed_task_publish_adopting_v1(
                    executor,
                    composite,
                    children.as_ptr(),
                    children.len() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!((*parent).owned_children, vec![untouched, composite]);
            assert_eq!((*composite).owned_children, children);
            assert_eq!((*right).owner, composite);
            assert_eq!((*left).owner, composite);
            assert_eq!((*untouched).owner, parent);
            assert_eq!((*composite).owner, parent);
            assert!((*composite).pending_owner.is_null());
            assert!((*composite).status == TaskStatus::Runnable);
            assert!((*composite).queued);
            assert!((*composite).typed.as_ref().unwrap().published);
            assert_eq!(
                (*executor)
                    .runnable
                    .iter()
                    .filter(|candidate| **candidate == composite)
                    .count(),
                1
            );
            assert_eq!(
                executor_task_order(executor),
                vec![parent, untouched, composite, right, left]
            );

            destroy(runtime, executor);
        }
    }

    #[test]
    fn invalid_empty_and_duplicate_adoption_leave_all_topology_unchanged() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let parent = create_initialized_typed_task(executor, &descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let child = create_typed_u64_child(executor, &descriptor);
            let composite = create_initialized_typed_task(executor, &descriptor);

            let task_order = executor_task_order(executor);
            let runnable = (*executor).runnable.clone();
            let parent_children = (*parent).owned_children.clone();
            let child_owner = (*child).owner;
            let composite_frame = typed_task_frame_v1(composite);
            let assert_unchanged = || {
                assert_eq!(executor_task_order(executor), task_order);
                assert_eq!((*executor).runnable, runnable);
                assert_eq!((*parent).owned_children, parent_children);
                assert_eq!((*child).owner, child_owner);
                assert!((*composite).owner.is_null());
                assert_eq!((*composite).pending_owner, parent);
                assert!((*composite).owned_children.is_empty());
                assert!((*composite).status == TaskStatus::Unpublished);
                assert!(!(*composite).queued);
                assert!(!(*composite).typed.as_ref().unwrap().published);
                assert_eq!(typed_task_frame_v1(composite), composite_frame);
            };

            assert_eq!(
                typed_task_publish_adopting_v1(executor, composite, ptr::null(), 0),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_unchanged();

            let duplicate = [child, child];
            assert_eq!(
                typed_task_publish_adopting_v1(
                    executor,
                    composite,
                    duplicate.as_ptr(),
                    duplicate.len() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_unchanged();
            destroy(runtime, executor);
        }
    }

    #[test]
    fn foreign_owner_and_cross_executor_adoption_fail_atomically() {
        let (runtime, executor) = runtime_and_executor();
        let (other_runtime, other_executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let parent = create_initialized_typed_task(executor, &descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let valid = create_typed_u64_child(executor, &descriptor);
            let foreign_owner = create_typed_u64_child(executor, &descriptor);

            (*parent).status = TaskStatus::Waiting;
            activate_test_task(executor, foreign_owner);
            let foreign_child = create_typed_u64_child(executor, &descriptor);
            (*foreign_owner).status = TaskStatus::Waiting;
            (*executor).active_task = parent;
            (*parent).status = TaskStatus::Running;

            let composite = create_initialized_typed_task(executor, &descriptor);
            let cross_executor = create_initialized_typed_task(other_executor, &descriptor);
            assert_eq!(
                typed_task_publish_v1(other_executor, cross_executor),
                TYPED_TASK_OK
            );

            let task_order = executor_task_order(executor);
            let runnable = (*executor).runnable.clone();
            let parent_children = (*parent).owned_children.clone();
            let foreign_children = (*foreign_owner).owned_children.clone();
            let assert_unchanged = || {
                assert_eq!(executor_task_order(executor), task_order);
                assert_eq!((*executor).runnable, runnable);
                assert_eq!((*parent).owned_children, parent_children);
                assert_eq!((*foreign_owner).owned_children, foreign_children);
                assert_eq!((*valid).owner, parent);
                assert_eq!((*foreign_child).owner, foreign_owner);
                assert!((*composite).owner.is_null());
                assert_eq!((*composite).pending_owner, parent);
                assert!((*composite).owned_children.is_empty());
                assert!((*composite).status == TaskStatus::Unpublished);
                assert!(!(*composite).typed.as_ref().unwrap().published);
            };

            let foreign = [valid, foreign_child];
            assert_eq!(
                typed_task_publish_adopting_v1(
                    executor,
                    composite,
                    foreign.as_ptr(),
                    foreign.len() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_unchanged();

            let cross = [valid, cross_executor];
            assert_eq!(
                typed_task_publish_adopting_v1(
                    executor,
                    composite,
                    cross.as_ptr(),
                    cross.len() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_unchanged();

            destroy(other_runtime, other_executor);
            destroy(runtime, executor);
        }
    }

    unsafe extern "C" fn legacy_complete(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_COMPLETED
    }

    #[test]
    fn immediate_join_keeps_the_active_callback_running_and_supports_another_join() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(complete_u64, cancel_noop);
        unsafe {
            let parent = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(parent, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            enter_executor(executor);

            let first = create_typed_u64_child(executor, &descriptor);
            complete_typed_u64_for_test(first, 20);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, first), WAIT_OK);
            assert_eq!(task_suspend_join(executor, parent), 0);
            assert!((*parent).status == TaskStatus::Running);
            assert!(!(*parent).queued);
            assert!(
                !(*executor)
                    .runnable
                    .iter()
                    .any(|candidate| *candidate == parent)
            );
            let mut first_result = 0_u64;
            assert_eq!(
                typed_task_take_result_v1(
                    first,
                    (&raw mut first_result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(first_result, 20);

            // A zero suspension result did not end this activation: generated
            // code may construct another structured child and await it.
            let second = create_typed_u64_child(executor, &descriptor);
            complete_typed_u64_for_test(second, 22);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, second), WAIT_OK);
            assert_eq!(task_suspend_join(executor, parent), 0);
            assert!((*parent).status == TaskStatus::Running);
            assert!(!(*parent).queued);
            assert!(
                !(*executor)
                    .runnable
                    .iter()
                    .any(|candidate| *candidate == parent)
            );
            let mut second_result = 0_u64;
            assert_eq!(
                typed_task_take_result_v1(
                    second,
                    (&raw mut second_result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(second_result, 22);

            let frame = (*parent)
                .typed
                .as_ref()
                .expect("typed parent")
                .frame_pointer()
                .cast::<u64>();
            frame.write(first_result + second_result);
            assert_eq!(typed_task_publish_result_v1(parent), TYPED_TASK_OK);
            leave_executor();
            (*executor).active_task = ptr::null_mut();
            complete_terminal(&mut *executor, parent, TASK_COMPLETED);
            assert!((*parent).status == TaskStatus::Completed);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn pending_join_keeps_the_parent_waiting_and_out_of_the_ready_queue() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(complete_u64, cancel_noop);
        unsafe {
            let parent = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(parent, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            enter_executor(executor);
            let child = create_typed_u64_child(executor, &descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            assert_eq!(task_suspend_join(executor, parent), 1);
            assert!((*parent).status == TaskStatus::Waiting);
            assert!(!(*parent).queued);
            assert!(
                !(*executor)
                    .runnable
                    .iter()
                    .any(|candidate| *candidate == parent)
            );
            assert!((*child).queued);
            assert!(
                (*executor)
                    .runnable
                    .iter()
                    .any(|candidate| *candidate == child)
            );
            leave_executor();
            (*executor).active_task = ptr::null_mut();
            destroy(runtime, executor);
        }
    }

    #[test]
    fn legacy_composite_consumes_an_already_completed_child_inline() {
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let owner = task_spawn(executor, Some(legacy_complete), 1, 0);
            activate_test_task(executor, owner);
            let child = task_spawn(executor, Some(legacy_complete), 1, 0);
            (*child).status = TaskStatus::Completed;
            let join = join_create(executor, TASK_JOIN_ALL, 0);
            assert!(!join.is_null());
            assert_eq!(join_add_task(join, child), WAIT_OK);
            let composite = join_task(join);
            assert!(!composite.is_null());
            (*executor).active_task = ptr::null_mut();
            activate_test_task(executor, composite);
            enter_executor(executor);
            assert_eq!(resume_composite(composite, executor), TASK_COMPLETED);
            assert!((*composite).status == TaskStatus::Running);
            assert!(!(*composite).queued);
            assert!(
                !(*executor)
                    .runnable
                    .iter()
                    .any(|candidate| *candidate == composite)
            );
            leave_executor();
            (*executor).active_task = ptr::null_mut();
            (*composite).status = TaskStatus::Completed;
            (*owner).status = TaskStatus::Completed;
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_task_requires_initialize_before_publish_and_take_zeros_the_result() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(complete_u64, cancel_noop);
        unsafe {
            let aborted = typed_task_create_v1(executor, &raw const descriptor);
            assert!(!aborted.is_null());
            assert_eq!(executor_live_tasks(executor), 1);
            let aborted_frame = typed_task_frame_v1(aborted);
            assert!(!aborted_frame.is_null());
            assert_eq!(
                typed_task_publish_v1(executor, aborted),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(typed_task_initialize_v1(aborted, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_frame_v1(aborted), aborted_frame);
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, aborted),
                TYPED_TASK_OK
            );
            assert_eq!(executor_live_tasks(executor), 0);

            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert!(!task.is_null());
            let frame = typed_task_frame_v1(task).cast::<u64>();
            assert!(!frame.is_null());
            assert_eq!(frame.read(), 0);
            assert_eq!(
                typed_task_initialize_v1(task, 1),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            assert!(typed_task_frame_v1(task).is_null());
            assert_eq!(
                typed_task_set_root_state_v1(task, 0),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, task),
                TYPED_TASK_INVALID_ARGUMENT
            );
            move_task_frames(&mut *executor);
            assert_eq!((*task).typed.as_ref().unwrap().frame.as_ptr(), frame.cast());
            assert_eq!(executor_run(executor, task), TASK_COMPLETED);
            assert_eq!(typed_task_status_v1(task), TASK_COMPLETED);

            let mut result = 0_u64;
            assert_eq!(
                typed_task_take_result_v1(
                    task,
                    (&raw mut result).cast(),
                    size_of::<u32>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            let mut misaligned_storage = [0_u8; size_of::<u64>() + align_of::<u64>()];
            let mut misaligned = misaligned_storage.as_mut_ptr();
            if (misaligned as usize).is_multiple_of(align_of::<u64>()) {
                misaligned = misaligned.add(1);
            }
            assert_eq!(
                typed_task_take_result_v1(
                    task,
                    misaligned.cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_task_take_result_v1(
                    task,
                    frame.cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_task_take_result_v1(
                    task,
                    (&raw mut result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(result, 42);
            assert_eq!(frame.read(), 0);
            assert_eq!(
                typed_task_take_result_v1(
                    task,
                    (&raw mut result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            destroy(runtime, executor);
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn typed_descriptor_rejects_hostile_shapes_and_misaligned_metadata() {
        let (runtime, executor) = runtime_and_executor();
        let base = descriptor(complete_u64, cancel_noop);
        unsafe {
            let mut descriptor_bytes = [0_u8; size_of::<LoomTypedCoroutineDescriptor>() + 1];
            let mut misaligned_descriptor = descriptor_bytes.as_mut_ptr();
            if (misaligned_descriptor as usize)
                .is_multiple_of(align_of::<LoomTypedCoroutineDescriptor>())
            {
                misaligned_descriptor = misaligned_descriptor.add(1);
            }
            assert!(typed_task_create_v1(executor, misaligned_descriptor.cast()).is_null());

            let reject = |candidate: LoomTypedCoroutineDescriptor| {
                assert!(typed_task_create_v1(executor, &raw const candidate).is_null());
            };
            reject(LoomTypedCoroutineDescriptor {
                abi_version: TYPED_TASK_ABI_VERSION + 1,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor { flags: 1, ..base });
            reject(LoomTypedCoroutineDescriptor {
                resume: None,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                cancel: None,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                frame_size: 0,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                frame_align: 3,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                result_offset: 1,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                result_size: 9,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                root_state_count: 0,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                completed_root_state: 1,
                ..base
            });
            reject(LoomTypedCoroutineDescriptor {
                root_slot_count: 1,
                root_bitmap_words: 0,
                ..base
            });

            let aligned_offsets = [0_u64];
            let aligned_bitmaps = [1_u64];
            let rooted = LoomTypedCoroutineDescriptor {
                root_slot_count: 1,
                root_bitmap_words: 1,
                root_offsets: aligned_offsets.as_ptr(),
                live_bitmaps: aligned_bitmaps.as_ptr(),
                ..base
            };
            let offset_storage = [0_u64; 2];
            reject(LoomTypedCoroutineDescriptor {
                root_offsets: offset_storage.as_ptr().cast::<u8>().add(1).cast(),
                ..rooted
            });
            let bitmap_storage = [0_u64; 2];
            reject(LoomTypedCoroutineDescriptor {
                live_bitmaps: bitmap_storage.as_ptr().cast::<u8>().add(1).cast(),
                ..rooted
            });
            let unaligned_offset = [1_u64];
            reject(LoomTypedCoroutineDescriptor {
                root_offsets: unaligned_offset.as_ptr(),
                ..rooted
            });
            let outside_offset = [8_u64];
            reject(LoomTypedCoroutineDescriptor {
                root_offsets: outside_offset.as_ptr(),
                ..rooted
            });
            let dirty_tail = [2_u64];
            reject(LoomTypedCoroutineDescriptor {
                live_bitmaps: dirty_tail.as_ptr(),
                ..rooted
            });

            let two_offsets = [0_u64, 8_u64];
            let two_bitmaps = [3_u64];
            let two_roots = LoomTypedCoroutineDescriptor {
                frame_size: 16,
                result_offset: 8,
                root_slot_count: 2,
                root_bitmap_words: 1,
                root_offsets: two_offsets.as_ptr(),
                live_bitmaps: two_bitmaps.as_ptr(),
                ..base
            };
            reject(two_roots);
            let duplicate_offsets = [0_u64, 0_u64];
            reject(LoomTypedCoroutineDescriptor {
                root_offsets: duplicate_offsets.as_ptr(),
                ..two_roots
            });
            assert_eq!(executor_live_tasks(executor), 0);
            destroy(runtime, executor);
        }
    }

    static CANCEL_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_cancel(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        CANCEL_CALLS.fetch_add(1, Ordering::SeqCst);
        TASK_CANCELLED
    }

    #[test]
    fn repeated_cancel_requests_invoke_the_callback_exactly_once() {
        CANCEL_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(complete_u64, count_cancel);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(typed_task_is_cancel_requested_v1(task), 1);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert_eq!(CANCEL_CALLS.load(Ordering::SeqCst), 1);
            destroy(runtime, executor);
        }
    }

    static DISPOSE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_dispose(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        DISPOSE_CALLS.fetch_add(1, Ordering::SeqCst);
        TASK_COMPLETED
    }

    unsafe extern "C" fn pending_dispose(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        DISPOSE_CALLS.fetch_add(1, Ordering::SeqCst);
        TASK_PENDING
    }

    unsafe extern "C" fn remain_pending(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_PENDING
    }

    unsafe extern "C" fn outer_aborts_child_with_live_roots(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        let roots_restored = unsafe {
            with_sync_and_typed_roots(|| {
                let descriptor = descriptor(remain_pending, cancel_noop);
                let child = typed_task_create_v1(executor, &raw const descriptor);
                !child.is_null()
                    && typed_task_initialize_v1(child, 0) == TYPED_TASK_OK
                    && typed_task_abort_unpublished_v1(executor, child) == TYPED_TASK_OK
            })
        };
        unsafe { frame.cast::<u64>().write(73) };
        if roots_restored && unsafe { typed_task_publish_result_v1(task) } == TYPED_TASK_OK {
            TASK_COMPLETED
        } else {
            TASK_FAULTED
        }
    }

    static REENTRANT_DISPOSE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn reentrant_dispose_callback(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        REENTRANT_DISPOSE_CALLS.fetch_add(1, Ordering::SeqCst);
        TASK_COMPLETED
    }

    unsafe extern "C" fn outer_disposes_child_with_live_roots(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        let roots_restored = unsafe {
            with_sync_and_typed_roots(|| {
                let mut descriptor = descriptor(complete_u64, cancel_noop);
                descriptor.dispose_result = Some(reentrant_dispose_callback);
                let child = typed_task_create_v1(executor, &raw const descriptor);
                if child.is_null() {
                    return false;
                }
                let child_frame = typed_task_frame_v1(child).cast::<u64>();
                if child_frame.is_null()
                    || typed_task_initialize_v1(child, 0) != TYPED_TASK_OK
                    || typed_task_publish_v1(executor, child) != TYPED_TASK_OK
                {
                    return false;
                }
                (*executor).runnable.retain(|candidate| *candidate != child);
                (*child).queued = false;
                child_frame.write(19);
                let typed = (*child).typed.as_mut().expect("typed child");
                typed.result_initialized = true;
                typed.root_state = typed.completed_root_state;
                (*child).status = TaskStatus::Completed;
                dispose_typed_result(&mut *executor, child).is_clean()
            })
        };
        unsafe { frame.cast::<u64>().write(91) };
        if roots_restored && unsafe { typed_task_publish_result_v1(task) } == TYPED_TASK_OK {
            TASK_COMPLETED
        } else {
            TASK_FAULTED
        }
    }

    #[test]
    fn nested_cancel_and_dispose_restore_outer_sync_and_typed_root_baselines() {
        REENTRANT_DISPOSE_CALLS.store(0, Ordering::SeqCst);
        for (resume, expected) in [
            (
                outer_aborts_child_with_live_roots as LoomTypedTaskCallback,
                73_u64,
            ),
            (
                outer_disposes_child_with_live_roots as LoomTypedTaskCallback,
                91_u64,
            ),
        ] {
            let (runtime, executor) = runtime_and_executor();
            let descriptor = descriptor(resume, cancel_noop);
            unsafe {
                let root = typed_task_create_v1(executor, &raw const descriptor);
                assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
                assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
                assert_eq!(executor_run(executor, root), TASK_COMPLETED);
                let mut result = 0_u64;
                assert_eq!(
                    typed_task_take_result_v1(
                        root,
                        (&raw mut result).cast(),
                        size_of::<u64>() as u64,
                        align_of::<u64>() as u64,
                    ),
                    TYPED_TASK_OK
                );
                assert_eq!(result, expected);
                destroy(runtime, executor);
            }
        }
        assert_eq!(REENTRANT_DISPOSE_CALLS.load(Ordering::SeqCst), 1);
    }

    const CLEANUP_LEGACY_SPAWN_DENIED: usize = 1 << 0;
    const CLEANUP_TYPED_SPAWN_DENIED: usize = 1 << 1;
    const CLEANUP_DELAYED_PUBLISH_DENIED: usize = 1 << 2;
    const CLEANUP_REGISTER_DENIED: usize = 1 << 3;
    const CLEANUP_SUSPEND_DENIED: usize = 1 << 4;
    const CLEANUP_JOIN_DENIED: usize = 1 << 5;
    const CLEANUP_CANCEL_DENIED: usize = 1 << 6;
    const CLEANUP_RUN_DENIED: usize = 1 << 7;
    const CLEANUP_ABORT_DENIED: usize = 1 << 8;
    const CLEANUP_ROOTS_ALLOWED: usize = 1 << 9;
    const CLEANUP_ALL_GUARDS: usize = (1 << 10) - 1;

    static DELAYED_TYPED_TASK: AtomicUsize = AtomicUsize::new(0);
    static CLEANUP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static CANCEL_CLEANUP_GUARDS: AtomicUsize = AtomicUsize::new(0);
    static DISPOSE_CLEANUP_GUARDS: AtomicUsize = AtomicUsize::new(0);
    static SELF_TAKE_STATUS: AtomicI32 = AtomicI32::new(i32::MIN);
    static SELF_TAKE_VALUE: AtomicU64 = AtomicU64::new(0);
    static MALICIOUS_DISPOSE_CALLS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn legacy_pending(_task: *mut LoomTask, _executor: *mut LoomExecutor) -> i32 {
        TASK_PENDING
    }

    unsafe fn exercise_cleanup_guards(
        task: *mut LoomTask,
        executor: *mut LoomExecutor,
        frame: *mut c_void,
    ) -> usize {
        let mut passed = 0;
        if unsafe { task_spawn(executor, Some(legacy_pending), 1, 0) }.is_null() {
            passed |= CLEANUP_LEGACY_SPAWN_DENIED;
        }
        let descriptor = descriptor(remain_pending, cancel_noop);
        if unsafe { typed_task_create_v1(executor, &raw const descriptor) }.is_null() {
            passed |= CLEANUP_TYPED_SPAWN_DENIED;
        }
        let delayed = DELAYED_TYPED_TASK.load(Ordering::SeqCst) as *mut LoomTask;
        if !delayed.is_null()
            && unsafe { typed_task_publish_v1(executor, delayed) } == TYPED_TASK_INVALID_ARGUMENT
        {
            passed |= CLEANUP_DELAYED_PUBLISH_DENIED;
        }
        let source = LoomWaitSource {
            abi_version: WAIT_ABI_VERSION,
            kind: WAIT_SOURCE_TIMER,
            handle: 0,
            interests: 0,
            reserved: 0,
            deadline_ns: u64::MAX,
        };
        let mut registration = LoomRegistration::default();
        if unsafe { executor_register(executor, &raw const source, frame, &raw mut registration) }
            == WAIT_INVALID_ARGUMENT
        {
            passed |= CLEANUP_REGISTER_DENIED;
        }
        if unsafe { task_suspend_wait(executor, task, &raw const source) } == WAIT_INVALID_ARGUMENT
        {
            passed |= CLEANUP_SUSPEND_DENIED;
        }
        if unsafe { task_prepare_join(executor, task, TASK_JOIN_ALL) } == WAIT_INVALID_ARGUMENT {
            passed |= CLEANUP_JOIN_DENIED;
        }
        if unsafe { typed_task_request_cancel_v1(executor, task) } == TYPED_TASK_INVALID_ARGUMENT {
            passed |= CLEANUP_CANCEL_DENIED;
        }
        if unsafe { executor_run(executor, task) } == WAIT_INVALID_ARGUMENT {
            passed |= CLEANUP_RUN_DENIED;
        }
        if !delayed.is_null()
            && unsafe { typed_task_abort_unpublished_v1(executor, delayed) }
                == TYPED_TASK_INVALID_ARGUMENT
        {
            passed |= CLEANUP_ABORT_DENIED;
        }
        if unsafe { with_sync_and_typed_roots(|| true) } {
            passed |= CLEANUP_ROOTS_ALLOWED;
        }
        passed
    }

    unsafe extern "C" fn adversarial_cancel(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        let passed = unsafe { exercise_cleanup_guards(task, executor, frame) };
        if unsafe { typed_task_set_root_state_v1(task, 0) } == TYPED_TASK_OK
            && unsafe { typed_task_is_cancel_requested_v1(task) } == 1
        {
            CANCEL_CLEANUP_GUARDS.store(passed, Ordering::SeqCst);
        }
        TASK_CANCELLED
    }

    unsafe extern "C" fn cancellation_mutates_topology(
        _task: *mut c_void,
        executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let executor = executor.cast::<LoomExecutor>();
        unsafe { (*(*executor).active_task).status = TaskStatus::Waiting };
        TASK_CANCELLED
    }

    unsafe extern "C" fn cancellation_returns_pending(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_PENDING
    }

    unsafe extern "C" fn cancellation_leaks_sync_root(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let bitmap = [1_u64];
        let descriptor = LoomGcRootDescriptor {
            abi_version: SHADOW_STACK_ABI_VERSION,
            flags: 0,
            slot_count: 1,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        let mut value = ValueSlot::default();
        let slots = [(&raw mut value).cast::<c_void>()];
        let mut root = LoomGcRootFrame {
            abi_version: SHADOW_STACK_ABI_VERSION,
            flags: 0,
            state: 0,
            descriptor: &raw const descriptor,
            slots: slots.as_ptr(),
            previous: ptr::null_mut(),
        };
        if unsafe { root_push_v1(&raw mut root) } == GC_OK {
            TASK_CANCELLED
        } else {
            TASK_FAULTED
        }
    }

    unsafe extern "C" fn cancellation_leaks_typed_root(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let bitmap = [1_u64];
        let descriptor = LoomGcTypedRootDescriptor {
            abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
            flags: 0,
            slot_count: 1,
            state_count: 1,
            live_bitmap_words: 1,
            live_bitmaps: bitmap.as_ptr(),
        };
        let mut value = ptr::null_mut::<c_void>();
        let slots = [(&raw mut value).cast::<c_void>()];
        let mut root = LoomGcTypedRootFrame {
            abi_version: TYPED_SHADOW_STACK_ABI_VERSION,
            flags: 0,
            state: 0,
            descriptor: &raw const descriptor,
            slots: slots.as_ptr(),
            previous: ptr::null_mut(),
        };
        if unsafe { typed_root_push_v1(&raw mut root) } == GC_OK {
            TASK_CANCELLED
        } else {
            TASK_FAULTED
        }
    }

    unsafe extern "C" fn cancellation_leaks_nested_activation(
        _task: *mut c_void,
        executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let executor = executor.cast::<LoomExecutor>();
        if unsafe { activate_runtime_v1((*executor).runtime_pointer()) } == GC_OK {
            TASK_CANCELLED
        } else {
            TASK_FAULTED
        }
    }

    unsafe fn assert_cleanup_activation_defect_is_recoverable(cancel: LoomTypedTaskCallback) {
        let (runtime, executor) = runtime_and_executor();
        let faulting_descriptor = descriptor(remain_pending, cancel);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const faulting_descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_FAULTED);
            assert_eq!(typed_task_status_v1(task), TASK_FAULTED);
            assert_eq!((*task).fault_code, "LOOM_RUNTIME_TYPED_CANCEL_ACTIVATION");
            assert!(active_runtime_pointer().is_null());
            assert_eq!((*runtime).active_depth.load(Ordering::Acquire), 0);
            assert!((*runtime).sync_root_top.is_null());
            assert_eq!((*runtime).sync_root_depth, 0);
            assert!((*runtime).typed_root_top.is_null());
            assert_eq!((*runtime).typed_root_depth, 0);

            // The same Runtime and executor must remain usable after the
            // malformed cleanup is isolated and converted into a Task fault.
            let follow_up_descriptor = descriptor(complete_u64, cancel_noop);
            let follow_up = typed_task_create_v1(executor, &raw const follow_up_descriptor);
            assert!(!follow_up.is_null());
            assert_eq!(typed_task_initialize_v1(follow_up, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, follow_up), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, follow_up), TASK_COMPLETED);
            let mut result = 0_u64;
            assert_eq!(
                typed_task_take_result_v1(
                    follow_up,
                    (&raw mut result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(result, 42);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn cleanup_sync_root_leak_faults_and_recovers_activation() {
        unsafe {
            assert_cleanup_activation_defect_is_recoverable(cancellation_leaks_sync_root);
        }
    }

    #[test]
    fn cleanup_typed_root_leak_faults_and_recovers_activation() {
        unsafe {
            assert_cleanup_activation_defect_is_recoverable(cancellation_leaks_typed_root);
        }
    }

    #[test]
    fn cleanup_nested_activation_leak_faults_and_recovers_activation() {
        unsafe {
            assert_cleanup_activation_defect_is_recoverable(cancellation_leaks_nested_activation);
        }
    }

    unsafe extern "C" fn adversarial_dispose_and_take(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        MALICIOUS_DISPOSE_CALLS.fetch_add(1, Ordering::SeqCst);
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        DISPOSE_CLEANUP_GUARDS.store(
            unsafe { exercise_cleanup_guards(task, executor, frame) },
            Ordering::SeqCst,
        );
        let mut stolen = u64::MAX;
        SELF_TAKE_STATUS.store(
            unsafe {
                typed_task_take_result_v1(
                    task,
                    (&raw mut stolen).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                )
            },
            Ordering::SeqCst,
        );
        SELF_TAKE_VALUE.store(stolen, Ordering::SeqCst);
        TASK_COMPLETED
    }

    #[test]
    fn cancellation_cleanup_cannot_create_work_or_change_wait_topology() {
        let _serial = CLEANUP_TEST_LOCK.lock().unwrap();
        CANCEL_CLEANUP_GUARDS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let delayed_descriptor = descriptor(remain_pending, cancel_noop);
        let root_descriptor = descriptor(remain_pending, adversarial_cancel);
        unsafe {
            let delayed = typed_task_create_v1(executor, &raw const delayed_descriptor);
            assert_eq!(typed_task_initialize_v1(delayed, 0), TYPED_TASK_OK);
            DELAYED_TYPED_TASK.store(delayed as usize, Ordering::SeqCst);

            let root = typed_task_create_v1(executor, &raw const root_descriptor);
            assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
            assert_eq!(typed_task_request_cancel_v1(executor, root), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, root), TASK_CANCELLED);
            assert_eq!(
                CANCEL_CLEANUP_GUARDS.load(Ordering::SeqCst),
                CLEANUP_ALL_GUARDS
            );
            assert_eq!(executor_live_tasks(executor), 2);
            assert!(!has_registrations(&*executor));
            assert_eq!(typed_task_status_v1(delayed), TASK_PENDING);
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, delayed),
                TYPED_TASK_OK
            );
            DELAYED_TYPED_TASK.store(0, Ordering::SeqCst);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn cancellation_does_not_suppress_topology_or_callback_protocol_defects() {
        for cancel in [
            cancellation_mutates_topology as LoomTypedTaskCallback,
            cancellation_returns_pending as LoomTypedTaskCallback,
        ] {
            let (runtime, executor) = runtime_and_executor();
            let descriptor = descriptor(remain_pending, cancel);
            unsafe {
                let root = typed_task_create_v1(executor, &raw const descriptor);
                assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
                assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
                assert_eq!(typed_task_request_cancel_v1(executor, root), TYPED_TASK_OK);
                assert_eq!(executor_run(executor, root), TASK_FAULTED);
                assert_eq!(typed_task_status_v1(root), TASK_FAULTED);
                let mut fault = LoomTypedTaskFaultView::default();
                assert_eq!(
                    typed_task_fault_view_v1(root, &raw mut fault),
                    TYPED_TASK_OK
                );
                destroy(runtime, executor);
            }
        }
    }

    #[test]
    fn root_disposer_cannot_take_its_result_or_reenter_scheduler_topology() {
        let _serial = CLEANUP_TEST_LOCK.lock().unwrap();
        DISPOSE_CLEANUP_GUARDS.store(0, Ordering::SeqCst);
        SELF_TAKE_STATUS.store(i32::MIN, Ordering::SeqCst);
        SELF_TAKE_VALUE.store(0, Ordering::SeqCst);
        MALICIOUS_DISPOSE_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let delayed_descriptor = descriptor(remain_pending, cancel_noop);
        let mut root_descriptor = descriptor(complete_u64, cancel_noop);
        root_descriptor.dispose_result = Some(adversarial_dispose_and_take);
        unsafe {
            let delayed = typed_task_create_v1(executor, &raw const delayed_descriptor);
            assert_eq!(typed_task_initialize_v1(delayed, 0), TYPED_TASK_OK);
            DELAYED_TYPED_TASK.store(delayed as usize, Ordering::SeqCst);

            let root = typed_task_create_v1(executor, &raw const root_descriptor);
            assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, root), TASK_COMPLETED);
            destroy(runtime, executor);
        }
        DELAYED_TYPED_TASK.store(0, Ordering::SeqCst);
        assert_eq!(
            DISPOSE_CLEANUP_GUARDS.load(Ordering::SeqCst),
            CLEANUP_ALL_GUARDS
        );
        assert_eq!(
            SELF_TAKE_STATUS.load(Ordering::SeqCst),
            TYPED_TASK_INVALID_ARGUMENT
        );
        assert_eq!(SELF_TAKE_VALUE.load(Ordering::SeqCst), u64::MAX);
        assert_eq!(MALICIOUS_DISPOSE_CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn disposal_is_exactly_once_never_runs_for_pending_and_cannot_suspend() {
        let _serial = CLEANUP_TEST_LOCK.lock().unwrap();
        DISPOSE_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let mut completed_descriptor = descriptor(complete_u64, cancel_noop);
        completed_descriptor.dispose_result = Some(count_dispose);
        unsafe {
            let completed = typed_task_create_v1(executor, &raw const completed_descriptor);
            assert_eq!(typed_task_initialize_v1(completed, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, completed), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, completed), TASK_COMPLETED);
            destroy(runtime, executor);
        }
        assert_eq!(DISPOSE_CALLS.load(Ordering::SeqCst), 1);

        DISPOSE_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let mut pending_descriptor = descriptor(remain_pending, cancel_noop);
        pending_descriptor.dispose_result = Some(count_dispose);
        unsafe {
            let pending = typed_task_create_v1(executor, &raw const pending_descriptor);
            assert_eq!(typed_task_initialize_v1(pending, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, pending), TYPED_TASK_OK);
            (*executor).runnable.clear();
            (*pending).queued = false;
            (*pending).status = TaskStatus::Waiting;
            destroy(runtime, executor);
        }
        assert_eq!(DISPOSE_CALLS.load(Ordering::SeqCst), 0);

        let (runtime, executor) = runtime_and_executor();
        let mut invalid_descriptor = descriptor(complete_u64, cancel_noop);
        invalid_descriptor.dispose_result = Some(pending_dispose);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const invalid_descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_COMPLETED);
            assert_eq!(
                dispose_typed_result(&mut *executor, task),
                CleanupOutcome::Defect
            );
            assert_eq!(DISPOSE_CALLS.load(Ordering::SeqCst), 1);
            assert!((*task).typed.as_ref().unwrap().result_disposed);
            assert!(!(*task).typed.as_ref().unwrap().result_initialized);
            assert_eq!((*task).typed.as_ref().unwrap().result_pointer().read(), 0);
            assert_eq!((*task).fault_code, "LOOM_RUNTIME_TYPED_DISPOSE_PENDING");
            assert_eq!(
                dispose_typed_result(&mut *executor, task),
                CleanupOutcome::Clean
            );
            assert_eq!(DISPOSE_CALLS.load(Ordering::SeqCst), 1);
            (*task).status = TaskStatus::Faulted;
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_faults_are_copied_bounded_and_first_fault_wins() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(complete_u64, cancel_noop);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            (*task).queued = false;
            (*executor).runnable.clear();
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
            let invalid_utf8 = [0xff_u8];
            assert_eq!(
                typed_task_record_fault_v1(
                    task,
                    invalid_utf8.as_ptr(),
                    1,
                    ptr::null(),
                    0,
                    ptr::null(),
                    0,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_task_record_fault_v1(
                    task,
                    ptr::null(),
                    TYPED_TASK_MAX_FAULT_TEXT_BYTES + 1,
                    ptr::null(),
                    0,
                    ptr::null(),
                    0,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            let (code, message, detail) = (b"TypedFault", b"first message", b"detail");
            assert_eq!(
                typed_task_record_fault_v1(
                    task,
                    code.as_ptr(),
                    code.len() as u64,
                    message.as_ptr(),
                    message.len() as u64,
                    detail.as_ptr(),
                    detail.len() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(
                typed_task_record_fault_v1(
                    task,
                    b"Later".as_ptr(),
                    5,
                    b"ignored".as_ptr(),
                    7,
                    ptr::null(),
                    0,
                ),
                TYPED_TASK_OK
            );
            (*executor).active_task = ptr::null_mut();
            (*task).status = TaskStatus::Faulted;

            let mut view = LoomTypedTaskFaultView::default();
            assert_eq!(typed_task_fault_view_v1(task, &raw mut view), TYPED_TASK_OK);
            let view_bytes = |view: LoomByteView| {
                if view.length == 0 {
                    &[][..]
                } else {
                    std::slice::from_raw_parts(
                        view.data,
                        usize::try_from(view.length).expect("bounded typed fault view"),
                    )
                }
            };
            assert_eq!(view_bytes(view.code), code);
            assert_eq!(view_bytes(view.message), message);
            assert_eq!(view_bytes(view.detail), detail);
            destroy(runtime, executor);
        }
    }

    #[repr(C)]
    struct RootFrame {
        active: *mut c_void,
        result: *mut c_void,
    }

    #[repr(C)]
    struct Leaf {
        marker: u64,
    }

    #[test]
    fn suspended_and_completed_rows_move_only_their_exact_typed_roots() {
        let (runtime, executor) = runtime_and_executor();
        let mut offsets = [
            offset_of!(RootFrame, active) as u64,
            offset_of!(RootFrame, result) as u64,
        ];
        let mut bitmaps = [1_u64, 2_u64];
        let descriptor = LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(remain_pending),
            cancel: Some(cancel_noop),
            dispose_result: None,
            frame_size: size_of::<RootFrame>() as u64,
            frame_align: align_of::<RootFrame>() as u64,
            result_offset: offset_of!(RootFrame, result) as u64,
            result_size: size_of::<*mut c_void>() as u64,
            result_align: align_of::<*mut c_void>() as u64,
            root_slot_count: 2,
            root_state_count: 2,
            root_bitmap_words: 1,
            root_offsets: offsets.as_ptr(),
            live_bitmaps: bitmaps.as_ptr(),
            completed_root_state: 1,
        };
        let leaf_descriptor = LoomGcObjectDescriptor {
            abi_version: TYPED_GC_ABI_VERSION,
            flags: 0,
            fixed_size: size_of::<Leaf>() as u64,
            object_align: align_of::<Leaf>() as u64,
            pointer_count: 0,
            pointer_offsets: ptr::null(),
        };
        unsafe {
            let task = typed_task_create_v1(executor, &raw const descriptor);
            let frame = typed_task_frame_v1(task).cast::<RootFrame>();
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            // Runtime owns copied metadata after create.
            offsets.fill(u64::MAX);
            bitmaps.fill(u64::MAX);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            (*executor).runnable.clear();
            (*task).queued = false;
            (*task).status = TaskStatus::Waiting;

            enter_executor(executor);
            assert_eq!(
                typed_alloc_v1(
                    &raw const leaf_descriptor,
                    size_of::<Leaf>() as u64,
                    &raw mut (*frame).active,
                ),
                GC_OK
            );
            assert_eq!(
                typed_alloc_v1(
                    &raw const leaf_descriptor,
                    size_of::<Leaf>() as u64,
                    &raw mut (*frame).result,
                ),
                GC_OK
            );
            leave_executor();
            let old_active = (*frame).active;
            let dead_result = (*frame).result;
            collect(&mut *executor);
            assert_ne!((*frame).active, old_active);
            assert_eq!((*frame).result, dead_result);
            assert_eq!((*executor).heap().typed_object_count(), 1);

            (*frame).result = ptr::null_mut();
            enter_executor(executor);
            assert_eq!(
                typed_alloc_v1(
                    &raw const leaf_descriptor,
                    size_of::<Leaf>() as u64,
                    &raw mut (*frame).result,
                ),
                GC_OK
            );
            leave_executor();
            let old_result = (*frame).result;
            (*task).typed.as_mut().unwrap().result_initialized = true;
            (*task).typed.as_mut().unwrap().root_state = 1;
            (*task).status = TaskStatus::Completed;
            collect(&mut *executor);
            assert_ne!((*frame).result, old_result);
            assert_eq!((*executor).heap().typed_object_count(), 1);
            assert!((*executor).heap().reclaimed >= 2);
            destroy(runtime, executor);
        }
    }

    #[repr(C)]
    struct ParentFrame {
        child: *mut LoomTask,
        state: u64,
        result: u64,
    }

    static JOIN_CANCEL_CHILD_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
    static JOIN_CANCEL_PARENT_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_join_child_cancel(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        JOIN_CANCEL_CHILD_CALLBACKS.fetch_add(1, Ordering::SeqCst);
        TASK_CANCELLED
    }

    unsafe extern "C" fn parent_inherits_join_cancel(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        let frame = frame.cast::<ParentFrame>();
        if unsafe { (*frame).state } == 0 {
            if unsafe { task_prepare_join(executor, task, TASK_JOIN_ALL) } != WAIT_OK {
                return TASK_FAULTED;
            }
            let descriptor = descriptor(remain_pending, count_join_child_cancel);
            let child = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
            if child.is_null()
                || unsafe { typed_task_initialize_v1(child, 0) } != TYPED_TASK_OK
                || unsafe { typed_task_publish_v1(executor, child) } != TYPED_TASK_OK
                || unsafe { task_add_join_child(executor, task, child) } != WAIT_OK
                || unsafe { typed_task_request_cancel_v1(executor, child) } != TYPED_TASK_OK
            {
                return TASK_FAULTED;
            }
            unsafe {
                (*frame).child = child;
                (*frame).state = 1;
            }
            return if unsafe { task_suspend_join(executor, task) } == 1 {
                TASK_PENDING
            } else {
                TASK_FAULTED
            };
        }
        let step = unsafe { task_join_step(task) };
        let valid = unsafe {
            step == TASK_CANCELLED
                && typed_task_status_v1((*frame).child) == TASK_CANCELLED
                && typed_task_is_cancel_requested_v1(task) == 0
                && !(*task).join_active
                && (*task).join_children.as_slice() == [(*frame).child]
        };
        if valid {
            JOIN_CANCEL_PARENT_OBSERVATIONS.fetch_add(1, Ordering::SeqCst);
            TASK_CANCELLED
        } else {
            TASK_FAULTED
        }
    }

    unsafe extern "C" fn return_unrequested_cancel(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_CANCELLED
    }

    fn parent_join_cancel_descriptor() -> LoomTypedCoroutineDescriptor {
        LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(parent_inherits_join_cancel),
            cancel: Some(cancel_noop),
            dispose_result: None,
            frame_size: size_of::<ParentFrame>() as u64,
            frame_align: align_of::<ParentFrame>() as u64,
            result_offset: offset_of!(ParentFrame, result) as u64,
            result_size: size_of::<u64>() as u64,
            result_align: align_of::<u64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        }
    }

    #[test]
    fn completed_child_join_authorizes_one_inherited_cancellation_step() {
        JOIN_CANCEL_CHILD_CALLBACKS.store(0, Ordering::SeqCst);
        JOIN_CANCEL_PARENT_OBSERVATIONS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let descriptor = parent_join_cancel_descriptor();
        unsafe {
            let parent = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(parent, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, parent), TASK_CANCELLED);
            assert_eq!(typed_task_status_v1(parent), TASK_CANCELLED);
            assert_eq!(typed_task_is_cancel_requested_v1(parent), 0);
            assert_eq!(JOIN_CANCEL_CHILD_CALLBACKS.load(Ordering::SeqCst), 1);
            assert_eq!(JOIN_CANCEL_PARENT_OBSERVATIONS.load(Ordering::SeqCst), 1);
            let mut fault = LoomTypedTaskFaultView::default();
            assert_eq!(
                typed_task_fault_view_v1(parent, &raw mut fault),
                TYPED_TASK_INVALID_ARGUMENT
            );
            destroy(runtime, executor);
        }
    }

    #[test]
    fn cancelled_without_a_completed_child_join_remains_a_runtime_defect() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(return_unrequested_cancel, cancel_noop);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_FAULTED);
            assert_eq!(typed_task_status_v1(task), TASK_FAULTED);
            assert_eq!((*task).fault_code, "LOOM_RUNTIME_TYPED_CANCEL_UNREQUESTED");
            destroy(runtime, executor);
        }
    }

    #[test]
    fn observed_join_cancel_expires_when_the_callback_returns_pending() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            activate_test_task(executor, task);

            (*task).status = TaskStatus::Waiting;
            (*task).join_active = true;
            (*task).join_step = TASK_CANCELLED;
            make_join_runnable(&mut *executor, task);
            activate_test_task(executor, task);

            assert_eq!(task_join_step(task), TASK_CANCELLED);
            assert!(!(*task).typed.as_ref().unwrap().join_completion_pending);
            assert!((*task).typed.as_ref().unwrap().join_cancel_authorized);
            assert_eq!(
                validate_typed_task_step(task, false, TASK_PENDING),
                TASK_PENDING
            );
            assert!(!(*task).typed.as_ref().unwrap().join_completion_pending);
            assert!(!(*task).typed.as_ref().unwrap().join_cancel_authorized);

            // The public join step remains readable for diagnostics and
            // legacy consumers, but it cannot mint another authorization.
            assert_eq!(task_join_step(task), TASK_CANCELLED);
            assert!(!(*task).typed.as_ref().unwrap().join_cancel_authorized);
            assert_eq!(
                validate_typed_task_step(task, false, TASK_CANCELLED),
                TASK_FAULTED
            );
            assert_eq!((*task).fault_code, "LOOM_RUNTIME_TYPED_CANCEL_UNREQUESTED");
            (*executor).active_task = ptr::null_mut();
            (*task).status = TaskStatus::Faulted;
            destroy(runtime, executor);
        }
    }

    unsafe extern "C" fn parent_join_and_take(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        let frame = frame.cast::<ParentFrame>();
        if unsafe { (*frame).state } == 0 {
            if unsafe { task_prepare_join(executor, task, TASK_JOIN_ALL) } != WAIT_OK {
                return TASK_FAULTED;
            }
            let descriptor = descriptor(complete_u64, cancel_noop);
            let child = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
            if child.is_null()
                || unsafe { typed_task_initialize_v1(child, 0) } != TYPED_TASK_OK
                || unsafe { typed_task_publish_v1(executor, child) } != TYPED_TASK_OK
                || unsafe { task_add_join_child(executor, task, child) } != WAIT_OK
            {
                return TASK_FAULTED;
            }
            unsafe {
                (*frame).child = child;
                (*frame).state = 1;
            }
            return if unsafe { task_suspend_join(executor, task) } == 1 {
                TASK_PENDING
            } else {
                TASK_FAULTED
            };
        }
        let mut child_result = 0_u64;
        if unsafe {
            typed_task_take_result_v1(
                (*frame).child,
                (&raw mut child_result).cast(),
                size_of::<u64>() as u64,
                align_of::<u64>() as u64,
            )
        } != TYPED_TASK_OK
        {
            return TASK_FAULTED;
        }
        unsafe { (*frame).result = child_result + 1 };
        if unsafe { typed_task_publish_result_v1(task) } == TYPED_TASK_OK {
            TASK_COMPLETED
        } else {
            TASK_FAULTED
        }
    }

    static STRUCTURED_DISPOSE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ANY_LOSER_DISPOSE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ANY_JOIN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    unsafe extern "C" fn count_structured_dispose(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        STRUCTURED_DISPOSE_CALLS.fetch_add(1, Ordering::SeqCst);
        TASK_COMPLETED
    }

    unsafe extern "C" fn count_any_loser_dispose(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        ANY_LOSER_DISPOSE_CALLS.fetch_add(1, Ordering::SeqCst);
        TASK_COMPLETED
    }

    #[test]
    fn typed_any_finalizes_every_loser_once_and_detaches_the_winner() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        ANY_LOSER_DISPOSE_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let parent_descriptor = descriptor(remain_pending, cancel_noop);
        let child_descriptor = descriptor(complete_u64, cancel_noop);
        let mut disposable_descriptor = descriptor(complete_u64, cancel_noop);
        disposable_descriptor.dispose_result = Some(count_any_loser_dispose);
        unsafe {
            let parent = create_initialized_typed_task(executor, &parent_descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);

            for round in 0_u64..2 {
                let expected_disposals = usize::try_from(round).unwrap() + 1;
                let winner = create_typed_u64_child(executor, &child_descriptor);
                let completed = create_typed_u64_child(executor, &disposable_descriptor);
                let faulted = create_typed_u64_child(executor, &child_descriptor);
                let cancelled = create_typed_u64_child(executor, &child_descriptor);
                assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
                for child in [winner, completed, faulted, cancelled] {
                    assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
                }
                complete_typed_u64_for_test(winner, 40 + round);
                complete_typed_u64_for_test(completed, 100 + round);
                (*faulted).status = TaskStatus::Faulted;
                (*cancelled).status = TaskStatus::Cancelled;
                (*parent).status = TaskStatus::Waiting;
                update_join(&mut *executor, parent);
                activate_test_task(executor, parent);

                assert_eq!(task_join_step(parent), TASK_COMPLETED);
                assert_eq!(task_join_winner(parent), 0);
                assert_eq!(
                    ANY_LOSER_DISPOSE_CALLS.load(Ordering::SeqCst),
                    expected_disposals
                );
                assert_eq!((*parent).owned_children.as_slice(), [winner]);
                assert_eq!((*parent).join_children.as_slice(), [winner]);
                assert!((*completed).typed.as_ref().unwrap().result_disposed);
                assert!((*executor).retired_tasks.contains(&completed));
                assert!((*executor).retired_tasks.contains(&faulted));
                assert!((*executor).retired_tasks.contains(&cancelled));

                // Reading the public step again cannot re-run finalization.
                assert_eq!(task_join_step(parent), TASK_COMPLETED);
                assert_eq!(
                    ANY_LOSER_DISPOSE_CALLS.load(Ordering::SeqCst),
                    expected_disposals
                );

                let mut result = 0_u64;
                assert_eq!(
                    typed_task_take_result_v1(
                        winner,
                        (&raw mut result).cast(),
                        size_of::<u64>() as u64,
                        align_of::<u64>() as u64,
                    ),
                    TYPED_TASK_OK
                );
                assert_eq!(result, 40 + round);
                assert!((*parent).owned_children.is_empty());
                assert!((*parent).join_children.is_empty());
                reap_retired_tasks(&mut *executor, parent);
            }
            assert_eq!(ANY_LOSER_DISPOSE_CALLS.load(Ordering::SeqCst), 2);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_race_keeps_an_arbitrary_faulted_winner_and_retires_every_loser() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        ANY_LOSER_DISPOSE_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let child_descriptor = descriptor(remain_pending, cancel_noop);
        let mut disposable_descriptor = descriptor(remain_pending, cancel_noop);
        disposable_descriptor.dispose_result = Some(count_any_loser_dispose);
        unsafe {
            let parent = create_initialized_typed_task(executor, &child_descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let cancelled_loser = create_typed_u64_child(executor, &child_descriptor);
            let winner = create_typed_u64_child(executor, &child_descriptor);
            let completed_loser = create_typed_u64_child(executor, &disposable_descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_RACE), WAIT_OK);
            for child in [cancelled_loser, winner, completed_loser] {
                assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            }

            record_primary_task_fault(
                &mut *winner,
                "RaceWinnerFault".into(),
                "the first terminal task faulted".into(),
                String::new(),
            );
            (*winner).status = TaskStatus::Faulted;
            complete_typed_u64_for_test(completed_loser, 99);
            (*parent).status = TaskStatus::Waiting;
            update_join(&mut *executor, parent);
            assert_eq!((*parent).join_winner, 1);
            assert!((*cancelled_loser).cancel_requested);
            assert!((*parent).status == TaskStatus::Waiting);

            (*cancelled_loser).status = TaskStatus::Cancelled;
            update_join(&mut *executor, parent);
            activate_test_task(executor, parent);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);
            assert_eq!(
                task_join_winner(parent),
                1,
                "winner keeps its input ordinal"
            );
            assert_eq!(ANY_LOSER_DISPOSE_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!((*parent).owned_children.as_slice(), [winner]);
            assert_eq!((*parent).join_children.as_slice(), [winner]);
            assert!((*completed_loser).typed.as_ref().unwrap().result_disposed);
            assert!((*executor).retired_tasks.contains(&cancelled_loser));
            assert!((*executor).retired_tasks.contains(&completed_loser));

            enter_executor(executor);
            let mut value = 41_u64;
            let mut code = ptr::null_mut();
            let mut message = ptr::null_mut();
            assert_eq!(
                typed_task_take_outcome_v1(
                    winner,
                    (&raw mut value).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                    &raw mut code,
                    &raw mut message,
                ),
                TASK_FAULTED
            );
            assert_eq!(value, 41);
            assert_eq!(crate::text::text_bytes(code).unwrap(), b"RaceWinnerFault");
            assert_eq!(
                crate::text::text_bytes(message).unwrap(),
                b"the first terminal task faulted"
            );
            leave_executor();
            assert!((*parent).owned_children.is_empty());
            assert!((*parent).join_children.is_empty());
            assert!((*executor).retired_tasks.contains(&winner));
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_race_loser_cleanup_fault_overrides_and_uses_reverse_input_order() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        let (runtime, executor) = runtime_and_executor();
        let parent_descriptor = descriptor(remain_pending, cancel_noop);
        let mut older_loser_descriptor = descriptor(remain_pending, cancel_noop);
        older_loser_descriptor.dispose_result = Some(pending_dispose);
        let winner_descriptor = descriptor(remain_pending, cancel_noop);
        let mut younger_loser_descriptor = descriptor(remain_pending, cancel_noop);
        younger_loser_descriptor.dispose_result = Some(fault_during_result_dispose);
        unsafe {
            let parent = create_initialized_typed_task(executor, &parent_descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let older_loser = create_typed_u64_child(executor, &older_loser_descriptor);
            let winner = create_typed_u64_child(executor, &winner_descriptor);
            let younger_loser = create_typed_u64_child(executor, &younger_loser_descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_RACE), WAIT_OK);
            for child in [older_loser, winner, younger_loser] {
                assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            }

            (*winner).status = TaskStatus::Cancelled;
            complete_typed_u64_for_test(younger_loser, 3);
            (*parent).status = TaskStatus::Waiting;
            update_join(&mut *executor, parent);
            assert_eq!((*parent).join_winner, 1);
            complete_typed_u64_for_test(older_loser, 2);
            update_join(&mut *executor, parent);
            activate_test_task(executor, parent);

            assert_eq!(task_join_step(parent), TASK_FAULTED);
            assert_eq!(task_join_winner(parent), 1);
            assert_eq!(
                (*parent).fault_code,
                "SuppressedDisposeCleanup",
                "the younger loser is cleaned first and remains primary"
            );
            assert_eq!((*parent).owned_children.as_slice(), [winner]);
            assert_eq!((*parent).join_children.as_slice(), [winner]);
            assert!((*older_loser).typed.as_ref().unwrap().result_disposed);
            assert!((*younger_loser).typed.as_ref().unwrap().result_disposed);
            assert!((*executor).retired_tasks.contains(&older_loser));
            assert!((*executor).retired_tasks.contains(&younger_loser));
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_any_loser_dispose_fault_overrides_success_and_is_inherited() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        let (runtime, executor) = runtime_and_executor();
        let parent_descriptor = descriptor(remain_pending, cancel_noop);
        let winner_descriptor = descriptor(complete_u64, cancel_noop);
        let mut loser_descriptor = descriptor(complete_u64, cancel_noop);
        loser_descriptor.dispose_result = Some(fault_during_result_dispose);
        unsafe {
            let parent = create_initialized_typed_task(executor, &parent_descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let winner = create_typed_u64_child(executor, &winner_descriptor);
            let loser = create_typed_u64_child(executor, &loser_descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, winner), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, loser), WAIT_OK);
            complete_typed_u64_for_test(winner, 42);
            complete_typed_u64_for_test(loser, 99);
            (*parent).status = TaskStatus::Waiting;
            update_join(&mut *executor, parent);
            activate_test_task(executor, parent);

            assert_eq!(task_join_step(parent), TASK_FAULTED);
            assert_eq!(task_join_winner(parent), 0);
            assert_eq!((*parent).fault_code, "SuppressedDisposeCleanup");
            assert_eq!((*parent).owned_children.as_slice(), [winner]);
            assert_eq!((*parent).join_children.as_slice(), [winner]);
            assert!((*loser).typed.as_ref().unwrap().result_disposed);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_any_all_failed_retires_the_complete_child_row_without_inheriting_one_child() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let parent = create_initialized_typed_task(executor, &descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let faulted = create_typed_u64_child(executor, &descriptor);
            let cancelled = create_typed_u64_child(executor, &descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, faulted), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, cancelled), WAIT_OK);
            (*faulted).status = TaskStatus::Faulted;
            (*cancelled).status = TaskStatus::Cancelled;
            (*parent).status = TaskStatus::Waiting;
            update_join(&mut *executor, parent);
            activate_test_task(executor, parent);

            assert_eq!(task_join_step(parent), TASK_FAULTED);
            assert_eq!(task_join_winner(parent), NO_JOIN_WINNER);
            assert!(!(*parent).primary_fault_recorded);
            assert!((*parent).owned_children.is_empty());
            assert!((*parent).join_children.is_empty());
            assert!((*executor).retired_tasks.contains(&faulted));
            assert!((*executor).retired_tasks.contains(&cancelled));
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_any_invalid_duplicate_topology_fails_before_changing_children() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, cancel_noop);
        unsafe {
            let parent = create_initialized_typed_task(executor, &descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let winner = create_typed_u64_child(executor, &descriptor);
            let nonterminal = create_typed_u64_child(executor, &descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, winner), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, nonterminal), WAIT_OK);
            complete_typed_u64_for_test(winner, 42);
            complete_typed_u64_for_test(nonterminal, 43);
            (*parent).join_children.push(winner);
            (*parent).status = TaskStatus::Waiting;
            (*parent).join_winner = 0;
            (*parent).join_step = TASK_COMPLETED;
            make_join_runnable(&mut *executor, parent);
            activate_test_task(executor, parent);
            let owned = (*parent).owned_children.clone();
            let joined = (*parent).join_children.clone();

            assert_eq!(task_join_step(parent), TASK_FAULTED);
            assert_eq!((*parent).fault_code, "LOOM_RUNTIME_TYPED_WINNER_FINALIZE");
            assert_eq!((*parent).owned_children, owned);
            assert_eq!((*parent).join_children, joined);
            assert!(!(*winner).typed.as_ref().unwrap().result_disposed);
            assert!((*executor).retired_tasks.is_empty());
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_any_loser_cleanup_primary_follows_reverse_input_order() {
        let _serial = ANY_JOIN_TEST_LOCK.lock().unwrap();
        let (runtime, executor) = runtime_and_executor();
        let parent_descriptor = descriptor(remain_pending, cancel_noop);
        let winner_descriptor = descriptor(complete_u64, cancel_noop);
        let mut later_defect_descriptor = descriptor(complete_u64, cancel_noop);
        later_defect_descriptor.dispose_result = Some(pending_dispose);
        let mut first_fault_descriptor = descriptor(complete_u64, cancel_noop);
        first_fault_descriptor.dispose_result = Some(fault_during_result_dispose);
        unsafe {
            let parent = create_initialized_typed_task(executor, &parent_descriptor);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            activate_test_task(executor, parent);
            let winner = create_typed_u64_child(executor, &winner_descriptor);
            let later_defect = create_typed_u64_child(executor, &later_defect_descriptor);
            let first_fault = create_typed_u64_child(executor, &first_fault_descriptor);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
            for child in [winner, later_defect, first_fault] {
                assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
                complete_typed_u64_for_test(child, 42);
            }
            (*parent).status = TaskStatus::Waiting;
            update_join(&mut *executor, parent);
            activate_test_task(executor, parent);

            assert_eq!(task_join_step(parent), TASK_FAULTED);
            assert_eq!(
                (*parent).fault_code,
                "SuppressedDisposeCleanup",
                "the first non-clean reverse-input loser must remain primary"
            );
            assert_eq!((*parent).owned_children.as_slice(), [winner]);
            assert_eq!((*parent).join_children.as_slice(), [winner]);
            assert!((*first_fault).typed.as_ref().unwrap().result_disposed);
            assert!((*later_defect).typed.as_ref().unwrap().result_disposed);
            destroy(runtime, executor);
        }
    }

    unsafe extern "C" fn parent_join_without_taking(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let task = task.cast::<LoomTask>();
        let executor = executor.cast::<LoomExecutor>();
        let frame = frame.cast::<ParentFrame>();
        if unsafe { (*frame).state } == 0 {
            if unsafe { task_prepare_join(executor, task, TASK_JOIN_ALL) } != WAIT_OK {
                return TASK_FAULTED;
            }
            let mut descriptor = descriptor(complete_u64, cancel_noop);
            descriptor.dispose_result = Some(count_structured_dispose);
            let child = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
            if child.is_null()
                || unsafe { typed_task_initialize_v1(child, 0) } != TYPED_TASK_OK
                || unsafe { typed_task_publish_v1(executor, child) } != TYPED_TASK_OK
                || unsafe { task_add_join_child(executor, task, child) } != WAIT_OK
            {
                return TASK_FAULTED;
            }
            unsafe {
                (*frame).child = child;
                (*frame).state = 1;
            }
            return if unsafe { task_suspend_join(executor, task) } == 1 {
                TASK_PENDING
            } else {
                TASK_FAULTED
            };
        }
        unsafe { (*frame).result = 7 };
        if unsafe { typed_task_publish_result_v1(task) } == TYPED_TASK_OK {
            TASK_COMPLETED
        } else {
            TASK_FAULTED
        }
    }

    #[test]
    fn taking_a_structured_child_result_detaches_and_reclaims_its_frame() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(parent_join_and_take),
            cancel: Some(cancel_noop),
            dispose_result: None,
            frame_size: size_of::<ParentFrame>() as u64,
            frame_align: align_of::<ParentFrame>() as u64,
            result_offset: offset_of!(ParentFrame, result) as u64,
            result_size: size_of::<u64>() as u64,
            result_align: align_of::<u64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        };
        unsafe {
            let parent = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(parent, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, parent), TASK_COMPLETED);
            assert_eq!(executor_live_tasks(executor), 2);
            assert_eq!((*executor).retired_tasks.len(), 1);
            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 1);
            assert_eq!(executor_tasks_reclaimed(executor), 1);
            let mut result = 0_u64;
            assert_eq!(
                typed_task_take_result_v1(
                    parent,
                    (&raw mut result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(result, 43);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn structured_completion_disposes_an_unconsumed_child_result_once() {
        STRUCTURED_DISPOSE_CALLS.store(0, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        let descriptor = LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(parent_join_without_taking),
            cancel: Some(cancel_noop),
            dispose_result: None,
            frame_size: size_of::<ParentFrame>() as u64,
            frame_align: align_of::<ParentFrame>() as u64,
            result_offset: offset_of!(ParentFrame, result) as u64,
            result_size: size_of::<u64>() as u64,
            result_align: align_of::<u64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        };
        unsafe {
            let parent = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(parent, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, parent), TASK_COMPLETED);
            assert_eq!(STRUCTURED_DISPOSE_CALLS.load(Ordering::SeqCst), 1);
            assert_eq!((*executor).retired_tasks.len(), 1);
            reap_retired_tasks(&mut *executor, parent);
            assert_eq!(executor_live_tasks(executor), 1);
            let mut result = 0_u64;
            assert_eq!(
                typed_task_take_result_v1(
                    parent,
                    (&raw mut result).cast(),
                    size_of::<u64>() as u64,
                    align_of::<u64>() as u64,
                ),
                TYPED_TASK_OK
            );
            assert_eq!(result, 7);
            destroy(runtime, executor);
        }
        assert_eq!(STRUCTURED_DISPOSE_CALLS.load(Ordering::SeqCst), 1);
    }

    static RETIRE_ORDER: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
    static RETIRE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static RETIRE_FAULT_STATUS: std::sync::atomic::AtomicI32 =
        std::sync::atomic::AtomicI32::new(i32::MIN);

    unsafe extern "C" fn log_retirement(
        _task: *mut c_void,
        _executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let marker = unsafe { frame.cast::<u64>().read() };
        RETIRE_ORDER.lock().unwrap().push(marker);
        TASK_CANCELLED
    }

    unsafe fn create_marked_initialized_task(
        executor: *mut LoomExecutor,
        descriptor: &LoomTypedCoroutineDescriptor,
        marker: u64,
    ) -> *mut LoomTask {
        let task = unsafe { create_initialized_typed_task(executor, descriptor) };
        unsafe { typed_task_frame_v1(task).cast::<u64>().write(marker) };
        task
    }

    unsafe fn adopted_cancel_tree(executor: *mut LoomExecutor) -> (*mut LoomTask, *mut LoomTask) {
        let descriptor = descriptor(remain_pending, log_retirement);
        let parent = unsafe { create_marked_initialized_task(executor, &descriptor, 0) };
        assert_eq!(
            unsafe { typed_task_publish_v1(executor, parent) },
            TYPED_TASK_OK
        );
        unsafe { activate_test_task(executor, parent) };

        let mut children = Vec::new();
        for marker in 1_u64..=3 {
            let child = unsafe { create_marked_initialized_task(executor, &descriptor, marker) };
            assert_eq!(
                unsafe { typed_task_publish_v1(executor, child) },
                TYPED_TASK_OK
            );
            children.push(child);
        }
        let composite = unsafe { create_marked_initialized_task(executor, &descriptor, 9) };
        assert_eq!(
            unsafe {
                typed_task_publish_adopting_v1(
                    executor,
                    composite,
                    children.as_ptr(),
                    children.len() as u64,
                )
            },
            TYPED_TASK_OK
        );
        (parent, composite)
    }

    #[test]
    fn adopted_composite_cancellation_propagates_in_reverse_input_order() {
        let _serial = RETIRE_TEST_LOCK.lock().unwrap();
        RETIRE_ORDER.lock().unwrap().clear();
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let (parent, composite) = adopted_cancel_tree(executor);
            assert_eq!((*composite).owned_children.len(), 3);
            (*parent).status = TaskStatus::Waiting;
            (*executor).active_task = ptr::null_mut();

            assert_eq!(
                typed_task_request_cancel_v1(executor, parent),
                TYPED_TASK_OK
            );
            assert_eq!(executor_run(executor, parent), TASK_CANCELLED);
            assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[3, 2, 1, 9, 0]);
            destroy(runtime, executor);
        }
        assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[3, 2, 1, 9, 0]);
        RETIRE_ORDER.lock().unwrap().clear();
    }

    #[test]
    fn adopted_subtrees_shutdown_before_their_composite_and_parent() {
        let _serial = RETIRE_TEST_LOCK.lock().unwrap();
        RETIRE_ORDER.lock().unwrap().clear();
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let (parent, _composite) = adopted_cancel_tree(executor);
            (*parent).status = TaskStatus::Waiting;
            (*executor).active_task = ptr::null_mut();
            destroy(runtime, executor);
        }
        assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[3, 2, 1, 9, 0]);
        RETIRE_ORDER.lock().unwrap().clear();
    }

    #[repr(C)]
    struct CancelOrderParentFrame {
        marker: u64,
        state: u64,
        result: u64,
    }

    unsafe extern "C" fn spawn_ordered_cancel_children(
        _task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let executor = executor.cast::<LoomExecutor>();
        let frame = frame.cast::<CancelOrderParentFrame>();
        if unsafe { (*frame).state } != 0 {
            return TASK_PENDING;
        }
        let descriptor = descriptor(remain_pending, log_retirement);
        for marker in 1_u64..=3 {
            let child = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
            if child.is_null() {
                return TASK_FAULTED;
            }
            unsafe { typed_task_frame_v1(child).cast::<u64>().write(marker) };
            if unsafe { typed_task_initialize_v1(child, 0) } != TYPED_TASK_OK
                || unsafe { typed_task_publish_v1(executor, child) } != TYPED_TASK_OK
            {
                return TASK_FAULTED;
            }
        }
        unsafe { (*frame).state = 1 };
        TASK_PENDING
    }

    fn cancel_order_parent_descriptor() -> LoomTypedCoroutineDescriptor {
        LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(spawn_ordered_cancel_children),
            cancel: Some(log_retirement),
            dispose_result: None,
            frame_size: size_of::<CancelOrderParentFrame>() as u64,
            frame_align: align_of::<CancelOrderParentFrame>() as u64,
            result_offset: offset_of!(CancelOrderParentFrame, result) as u64,
            result_size: size_of::<u64>() as u64,
            result_align: align_of::<u64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        }
    }

    unsafe extern "C" fn fault_retirement(
        task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let code = b"RetireFault";
        let message = b"retirement cleanup failed";
        let status = unsafe {
            typed_task_record_fault_v1(
                task.cast(),
                code.as_ptr(),
                code.len() as u64,
                message.as_ptr(),
                message.len() as u64,
                ptr::null(),
                0,
            )
        };
        RETIRE_FAULT_STATUS.store(status, Ordering::SeqCst);
        TASK_FAULTED
    }

    unsafe extern "C" fn fault_during_requested_cancel(
        task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let code = b"SuppressedCancelCleanup";
        let message = b"cancellation already owns the outcome";
        let _ = unsafe {
            typed_task_record_fault_v1(
                task.cast(),
                code.as_ptr(),
                code.len() as u64,
                message.as_ptr(),
                message.len() as u64,
                ptr::null(),
                0,
            )
        };
        TASK_FAULTED
    }

    unsafe extern "C" fn fault_during_result_dispose(
        task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        let code = b"SuppressedDisposeCleanup";
        let message = b"parent cancellation already owns the outcome";
        let _ = unsafe {
            typed_task_record_fault_v1(
                task.cast(),
                code.as_ptr(),
                code.len() as u64,
                message.as_ptr(),
                message.len() as u64,
                ptr::null(),
                0,
            )
        };
        TASK_FAULTED
    }

    #[repr(C)]
    struct OneChildParentFrame {
        state: u64,
        result: u64,
    }

    unsafe fn spawn_one_cleanup_child(
        executor: *mut LoomExecutor,
        frame: *mut OneChildParentFrame,
        result_dispose: Option<LoomTypedTaskCallback>,
    ) -> i32 {
        if unsafe { (*frame).state } != 0 {
            return TASK_PENDING;
        }
        let mut descriptor = if result_dispose.is_some() {
            descriptor(complete_u64, cancel_noop)
        } else {
            descriptor(remain_pending, fault_during_requested_cancel)
        };
        descriptor.dispose_result = result_dispose;
        let child = unsafe { typed_task_create_v1(executor, &raw const descriptor) };
        if child.is_null()
            || unsafe { typed_task_initialize_v1(child, 0) } != TYPED_TASK_OK
            || unsafe { typed_task_publish_v1(executor, child) } != TYPED_TASK_OK
        {
            return TASK_FAULTED;
        }
        unsafe { (*frame).state = 1 };
        TASK_PENDING
    }

    unsafe extern "C" fn spawn_cancel_fault_child(
        _task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        unsafe {
            spawn_one_cleanup_child(executor.cast(), frame.cast::<OneChildParentFrame>(), None)
        }
    }

    unsafe extern "C" fn spawn_dispose_fault_child(
        _task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        unsafe {
            spawn_one_cleanup_child(
                executor.cast(),
                frame.cast::<OneChildParentFrame>(),
                Some(fault_during_result_dispose),
            )
        }
    }

    unsafe extern "C" fn spawn_invalid_dispose_child(
        _task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        unsafe {
            spawn_one_cleanup_child(
                executor.cast(),
                frame.cast::<OneChildParentFrame>(),
                Some(pending_dispose),
            )
        }
    }

    fn one_child_parent_descriptor(resume: LoomTypedTaskCallback) -> LoomTypedCoroutineDescriptor {
        LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(resume),
            cancel: Some(cancel_noop),
            dispose_result: None,
            frame_size: size_of::<OneChildParentFrame>() as u64,
            frame_align: align_of::<OneChildParentFrame>() as u64,
            result_offset: offset_of!(OneChildParentFrame, result) as u64,
            result_size: size_of::<u64>() as u64,
            result_align: align_of::<u64>() as u64,
            root_slot_count: 0,
            root_state_count: 1,
            root_bitmap_words: 0,
            root_offsets: ptr::null(),
            live_bitmaps: ptr::null(),
            completed_root_state: 0,
        }
    }

    unsafe extern "C" fn invalid_retirement(
        _task: *mut c_void,
        _executor: *mut c_void,
        _frame: *mut c_void,
    ) -> i32 {
        TASK_PENDING
    }

    #[test]
    fn initialized_abort_and_executor_shutdown_retire_frames_once_in_lifo_order() {
        let _serial = RETIRE_TEST_LOCK.lock().unwrap();
        RETIRE_ORDER.lock().unwrap().clear();
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, log_retirement);
        unsafe {
            let uninitialized = typed_task_create_v1(executor, &raw const descriptor);
            typed_task_frame_v1(uninitialized).cast::<u64>().write(99);
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, uninitialized),
                TYPED_TASK_OK
            );
            assert!(RETIRE_ORDER.lock().unwrap().is_empty());

            let initialized = typed_task_create_v1(executor, &raw const descriptor);
            typed_task_frame_v1(initialized).cast::<u64>().write(3);
            assert_eq!(typed_task_initialize_v1(initialized, 0), TYPED_TASK_OK);
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, initialized),
                TYPED_TASK_OK
            );
            assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[3]);
            RETIRE_ORDER.lock().unwrap().clear();

            let parent = typed_task_create_v1(executor, &raw const descriptor);
            typed_task_frame_v1(parent).cast::<u64>().write(1);
            assert_eq!(typed_task_initialize_v1(parent, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, parent), TYPED_TASK_OK);
            (*parent).queued = false;
            (*parent).status = TaskStatus::Running;
            (*executor).runnable.clear();
            (*executor).active_task = parent;

            let child = typed_task_create_v1(executor, &raw const descriptor);
            typed_task_frame_v1(child).cast::<u64>().write(2);
            assert_eq!(typed_task_initialize_v1(child, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, child), TYPED_TASK_OK);
            (*child).queued = false;
            (*child).status = TaskStatus::Waiting;
            (*parent).status = TaskStatus::Waiting;
            (*executor).active_task = ptr::null_mut();
            (*executor).runnable.clear();

            destroy(runtime, executor);
        }
        assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[2, 1]);
    }

    #[test]
    fn structured_cancellation_runs_children_in_reverse_creation_order() {
        let _serial = RETIRE_TEST_LOCK.lock().unwrap();
        RETIRE_ORDER.lock().unwrap().clear();
        let (runtime, executor) = runtime_and_executor();
        let descriptor = cancel_order_parent_descriptor();
        unsafe {
            let root = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
            let children = prime_pending_parent(executor, root, spawn_ordered_cancel_children);
            assert_eq!(children.len(), 3);
            assert_eq!(typed_task_request_cancel_v1(executor, root), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, root), TASK_CANCELLED);
            assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[3, 2, 1, 0]);
            RETIRE_ORDER.lock().unwrap().clear();
            destroy(runtime, executor);
        }
        assert!(RETIRE_ORDER.lock().unwrap().is_empty());
    }

    #[test]
    fn abort_preserves_reverse_creation_order_for_remaining_frames() {
        let _serial = RETIRE_TEST_LOCK.lock().unwrap();
        RETIRE_ORDER.lock().unwrap().clear();
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, log_retirement);
        unsafe {
            let mut tasks = Vec::new();
            for marker in 1_u64..=4 {
                let task = typed_task_create_v1(executor, &raw const descriptor);
                typed_task_frame_v1(task).cast::<u64>().write(marker);
                assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
                tasks.push(task);
            }
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, tasks[1]),
                TYPED_TASK_OK
            );
            assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[2]);
            RETIRE_ORDER.lock().unwrap().clear();
            destroy(runtime, executor);
        }
        assert_eq!(&*RETIRE_ORDER.lock().unwrap(), &[4, 3, 1]);
    }

    #[test]
    fn abort_reports_faulted_and_invalid_non_suspending_cleanup() {
        RETIRE_FAULT_STATUS.store(i32::MIN, Ordering::SeqCst);
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let fault_descriptor = descriptor(remain_pending, fault_retirement);
            let faulted = typed_task_create_v1(executor, &raw const fault_descriptor);
            assert_eq!(typed_task_initialize_v1(faulted, 0), TYPED_TASK_OK);
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, faulted),
                TYPED_TASK_CLEANUP_FAULTED
            );
            assert_eq!(RETIRE_FAULT_STATUS.load(Ordering::SeqCst), TYPED_TASK_OK);

            let invalid_descriptor = descriptor(remain_pending, invalid_retirement);
            let invalid = typed_task_create_v1(executor, &raw const invalid_descriptor);
            assert_eq!(typed_task_initialize_v1(invalid, 0), TYPED_TASK_OK);
            assert_eq!(
                typed_task_abort_unpublished_v1(executor, invalid),
                TYPED_TASK_CLEANUP_FAULTED
            );
            assert_eq!(executor_live_tasks(executor), 0);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn requested_cancellation_suppresses_later_cleanup_faults() {
        let (runtime, executor) = runtime_and_executor();
        let descriptor = descriptor(remain_pending, fault_during_requested_cancel);
        unsafe {
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert_eq!(typed_task_status_v1(task), TASK_CANCELLED);
            let mut fault = LoomTypedTaskFaultView::default();
            assert_eq!(
                typed_task_fault_view_v1(task, &raw mut fault),
                TYPED_TASK_INVALID_ARGUMENT
            );
            destroy(runtime, executor);
        }
    }

    #[test]
    fn parent_cancellation_suppresses_child_cancel_and_dispose_faults() {
        for (resume, child_completed) in [
            (spawn_cancel_fault_child as LoomTypedTaskCallback, false),
            (spawn_dispose_fault_child as LoomTypedTaskCallback, true),
        ] {
            let (runtime, executor) = runtime_and_executor();
            let descriptor = one_child_parent_descriptor(resume);
            unsafe {
                let root = typed_task_create_v1(executor, &raw const descriptor);
                assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
                assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
                let children = prime_pending_parent(executor, root, resume);
                assert_eq!(children.len(), 1);
                if child_completed {
                    complete_typed_u64_for_test(children[0], 42);
                }
                assert_eq!(typed_task_request_cancel_v1(executor, root), TYPED_TASK_OK);
                assert_eq!(executor_run(executor, root), TASK_CANCELLED);
                assert_eq!(typed_task_status_v1(root), TASK_CANCELLED);
                let mut fault = LoomTypedTaskFaultView::default();
                assert_eq!(
                    typed_task_fault_view_v1(root, &raw mut fault),
                    TYPED_TASK_INVALID_ARGUMENT
                );
                destroy(runtime, executor);
            }
        }
    }

    #[test]
    fn parent_cancellation_does_not_suppress_child_dispose_protocol_defects() {
        let _serial = CLEANUP_TEST_LOCK.lock().unwrap();
        let (runtime, executor) = runtime_and_executor();
        let descriptor =
            one_child_parent_descriptor(spawn_invalid_dispose_child as LoomTypedTaskCallback);
        unsafe {
            let root = typed_task_create_v1(executor, &raw const descriptor);
            assert_eq!(typed_task_initialize_v1(root, 0), TYPED_TASK_OK);
            assert_eq!(typed_task_publish_v1(executor, root), TYPED_TASK_OK);
            let children = prime_pending_parent(executor, root, spawn_invalid_dispose_child);
            assert_eq!(children.len(), 1);
            complete_typed_u64_for_test(children[0], 42);
            assert_eq!(typed_task_request_cancel_v1(executor, root), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, root), TASK_FAULTED);
            assert_eq!(typed_task_status_v1(root), TASK_FAULTED);
            destroy(runtime, executor);
        }
    }
}
