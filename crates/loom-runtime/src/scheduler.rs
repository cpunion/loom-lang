use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::{align_of, size_of};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::ptr::{self, NonNull};
use std::slice;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicU8, Ordering},
    mpsc,
};

#[cfg(test)]
use std::sync::atomic::AtomicBool;

use loom_runtime_abi::{
    FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX, GC_MAX_OBJECT_ALIGNMENT,
    GC_MAX_OBJECT_BYTES, GC_MAX_ROOT_BITMAP_WORDS, GC_MAX_ROOT_SLOTS, GC_MAX_ROOT_STATES, GC_OK,
    LoomByteView, LoomTypedCoroutineDescriptor, LoomTypedIoOutcome, LoomTypedIoRequest,
    LoomTypedTaskCallback, LoomTypedTaskFaultView, LoomWitnessInstance, TYPED_IO_ABI_VERSION,
    TYPED_IO_FAULT_CLASS_INVALID_PORT, TYPED_IO_FAULT_CLASS_OPERATION,
    TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE, TYPED_IO_INVALID_RESOURCE_TOKEN,
    TYPED_IO_OPERATION_FILE_CREATE, TYPED_IO_OPERATION_FILE_OPEN_READ,
    TYPED_IO_OPERATION_FILE_READ_TEXT, TYPED_IO_OPERATION_FILE_WRITE_TEXT,
    TYPED_IO_OPERATION_SOCKET_CONNECT, TYPED_IO_OPERATION_SOCKET_READ_TEXT,
    TYPED_IO_OPERATION_SOCKET_WRITE_TEXT, TYPED_IO_OUTCOME_ERROR, TYPED_IO_OUTCOME_RESOURCE,
    TYPED_IO_OUTCOME_TEXT, TYPED_IO_OUTCOME_UNIT, TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT,
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
use crate::platform::{INVALID_RESOURCE_TOKEN, OwnedResource, socket_handle_bits};
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
const TASK_FAULT_TYPE: u64 = 4;
const TASK_OUTCOME_TYPE: u64 = 5;
const TASK_OUTCOME_COMPLETED: u64 = 0;
const TASK_OUTCOME_FAULTED: u64 = 1;
const TASK_OUTCOME_CANCELLED: u64 = 2;
const TASK_FAULT_CODE: &str = "TaskFault";
const TASK_FAULT_MESSAGE: &str = "task execution failed";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoResourceKind {
    File,
    Socket,
}

impl IoResourceKind {
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

    fn live_non_result_root_offset(&self, cell: *mut *mut c_void) -> Option<usize> {
        let address = cell as usize;
        let frame_start = self.frame.as_ptr() as usize;
        let frame_end = frame_start.checked_add(self.frame_layout.size())?;
        let cell_end = address.checked_add(size_of::<*mut c_void>())?;
        if address < frame_start
            || cell_end > frame_end
            || !address.is_multiple_of(align_of::<*mut c_void>())
        {
            return None;
        }
        let offset = address.checked_sub(frame_start)?;
        let result_end = self.result_offset.checked_add(self.result_size)?;
        if offset < result_end && offset.checked_add(size_of::<*mut c_void>())? > self.result_offset
        {
            return None;
        }
        self.root_offsets
            .iter()
            .position(|candidate| *candidate == offset)
            .filter(|index| self.root_is_live(self.root_state, *index))
            .map(|_| offset)
    }

    fn has_typed_io_root_shape(&self) -> bool {
        if self.root_offsets.is_empty()
            || self.root_state_count != 2
            || self.completed_root_state != 1
            || self.root_bitmap_words != 1
        {
            return false;
        }
        let pointer_size = size_of::<*mut c_void>();
        let Some(result_end) = self.result_offset.checked_add(self.result_size) else {
            return false;
        };
        let inside_result = |offset: usize| {
            offset >= self.result_offset
                && offset
                    .checked_add(pointer_size)
                    .is_some_and(|end| end <= result_end)
        };
        let mut running_roots = 0;
        for (index, offset) in self.root_offsets.iter().copied().enumerate() {
            let running = self.root_is_live(0, index);
            let completed = self.root_is_live(1, index);
            if running == completed {
                return false;
            }
            if running {
                if inside_result(offset) {
                    return false;
                }
                running_roots += 1;
            } else if !inside_result(offset) {
                return false;
            }
        }
        running_roots == 1
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypedIoOperation {
    FileOpenRead,
    FileCreate,
    FileReadText,
    FileWriteText,
    SocketConnect,
    SocketReadText,
    SocketWriteText,
}

impl TypedIoOperation {
    const fn from_abi(operation: u32) -> Option<Self> {
        match operation {
            TYPED_IO_OPERATION_FILE_OPEN_READ => Some(Self::FileOpenRead),
            TYPED_IO_OPERATION_FILE_CREATE => Some(Self::FileCreate),
            TYPED_IO_OPERATION_FILE_READ_TEXT => Some(Self::FileReadText),
            TYPED_IO_OPERATION_FILE_WRITE_TEXT => Some(Self::FileWriteText),
            TYPED_IO_OPERATION_SOCKET_CONNECT => Some(Self::SocketConnect),
            TYPED_IO_OPERATION_SOCKET_READ_TEXT => Some(Self::SocketReadText),
            TYPED_IO_OPERATION_SOCKET_WRITE_TEXT => Some(Self::SocketWriteText),
            _ => None,
        }
    }

    const fn resource_kind(self) -> Option<IoResourceKind> {
        match self {
            Self::FileOpenRead | Self::FileCreate => Some(IoResourceKind::File),
            Self::SocketConnect => Some(IoResourceKind::Socket),
            Self::FileReadText
            | Self::FileWriteText
            | Self::SocketReadText
            | Self::SocketWriteText => None,
        }
    }

    const fn expects_text(self) -> bool {
        matches!(self, Self::FileReadText | Self::SocketReadText)
    }

    const fn expects_unit(self) -> bool {
        matches!(self, Self::FileWriteText | Self::SocketWriteText)
    }
}

pub(crate) enum BlockingResult {
    Resource {
        kind: IoResourceKind,
        resource: OwnedResource,
    },
    Text(Vec<u8>),
    Unit,
    Fault {
        class: IoFaultClass,
        kind: u32,
        message: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IoFaultClass {
    Operation,
    InvalidPort,
    SocketResolve,
}

impl IoFaultClass {
    const fn abi_payload(self) -> u64 {
        match self {
            Self::Operation => TYPED_IO_FAULT_CLASS_OPERATION,
            Self::InvalidPort => TYPED_IO_FAULT_CLASS_INVALID_PORT,
            Self::SocketResolve => TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE,
        }
    }
}

enum IoProgress {
    Ready(BlockingResult),
    Suspend {
        operation: IoOperation,
        handle: i64,
        interests: u32,
    },
}

pub(crate) struct WorkerCompletion {
    pub(crate) task: usize,
    pub(crate) registration: LoomRegistration,
    pub(crate) result: Option<BlockingResult>,
}

struct BlockingWait {
    registration: LoomRegistration,
    state: Arc<AtomicU8>,
    submission: Arc<Mutex<Option<BlockingJob>>>,
}

type BlockingJob = Box<dyn FnOnce() + Send + 'static>;

const BLOCKING_QUEUED: u8 = 0;
const BLOCKING_STARTED: u8 = 1;
const BLOCKING_CANCELLED_QUEUED: u8 = 2;
const BLOCKING_CANCELLED_STARTED: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingCancellation {
    Queued,
    Started,
}

fn take_blocking_submission(submission: &Mutex<Option<BlockingJob>>) -> Option<BlockingJob> {
    let Ok(mut submission) = submission.lock() else {
        std::process::abort();
    };
    submission.take()
}

fn cancel_blocking_work(
    state: &AtomicU8,
    submission: &Mutex<Option<BlockingJob>>,
) -> BlockingCancellation {
    loop {
        match state.load(Ordering::Acquire) {
            BLOCKING_QUEUED => {
                if state
                    .compare_exchange(
                        BLOCKING_QUEUED,
                        BLOCKING_CANCELLED_QUEUED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    drop(take_blocking_submission(submission));
                    return BlockingCancellation::Queued;
                }
            }
            BLOCKING_STARTED => {
                if state
                    .compare_exchange(
                        BLOCKING_STARTED,
                        BLOCKING_CANCELLED_STARTED,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    return BlockingCancellation::Started;
                }
            }
            BLOCKING_CANCELLED_QUEUED => {
                drop(take_blocking_submission(submission));
                return BlockingCancellation::Queued;
            }
            BLOCKING_CANCELLED_STARTED => return BlockingCancellation::Started,
            _ => std::process::abort(),
        }
    }
}

fn run_blocking_submission(state: &AtomicU8, submission: &Mutex<Option<BlockingJob>>) -> bool {
    if state
        .compare_exchange(
            BLOCKING_QUEUED,
            BLOCKING_STARTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }
    let Some(submission) = take_blocking_submission(submission) else {
        std::process::abort();
    };
    submission();
    true
}

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
    blocking_wait: Option<BlockingWait>,
    io_fallible: bool,
    typed_io_operation: Option<TypedIoOperation>,
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
    let blocking = unsafe {
        (*task).blocking_wait.as_ref().map(|wait| {
            (
                wait.registration,
                cancel_blocking_work(&wait.state, &wait.submission)
                    == BlockingCancellation::Started,
            )
        })
    };
    let registrations = unsafe { std::mem::take(&mut (*task).waits) };
    let mut retained_blocking_wait = Vec::new();
    for registration in registrations {
        if blocking == Some((registration, true)) {
            retained_blocking_wait.push(registration);
            continue;
        }
        // SAFETY: each registration was created by this executor for task.
        let _ = unsafe { cancel_for_task(executor, &raw const registration) };
    }
    let blocking_pending = !retained_blocking_wait.is_empty();
    unsafe {
        (*task).waits = retained_blocking_wait;
        if !blocking_pending {
            (*task).blocking_wait = None;
        }
    }
    if unsafe { (*task).status } != TaskStatus::Running && !blocking_pending {
        // Among children which can run their cancellation callback now, queue
        // order favors newer children. A child draining started I/O becomes
        // runnable only at completion, so no cross-sibling cleanup completion
        // order is promised.
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

/// Cancels and drains every submitted blocking job before executor-owned Task
/// storage or reactor state can be released.
///
/// Queued jobs are atomically claimed by cancellation and need no worker
/// completion before the Task or executor may retire. Jobs which already
/// started are not asynchronously interrupted; executor teardown waits only
/// for those completions and discards their results. A worker never retains a
/// live Task reference, but draining started work here also guarantees that
/// teardown itself is a structured side-effect boundary.
pub(crate) unsafe fn drain_blocking_work_before_executor_drop(executor: &mut LoomExecutor) {
    let waits = executor
        .tasks
        .iter_mut()
        .filter_map(|task| {
            let pointer = (&raw mut **task).cast::<LoomTask>();
            let wait = task.blocking_wait.as_ref()?;
            Some((
                pointer,
                wait.registration,
                Arc::clone(&wait.state),
                Arc::clone(&wait.submission),
            ))
        })
        .collect::<Vec<_>>();
    if waits.is_empty() {
        return;
    }

    let mut pending = Vec::new();
    for (task, registration, state, submission) in waits {
        let _ = unsafe { cancel_for_task(executor, ptr::from_ref(&registration)) };
        unsafe { (*task).waits.retain(|candidate| *candidate != registration) };
        if cancel_blocking_work(&state, &submission) == BlockingCancellation::Started {
            pending.push((task, registration));
        } else {
            unsafe { (*task).blocking_wait = None };
        }
    }
    if pending.is_empty() {
        return;
    }

    let mut remaining = pending;
    while !remaining.is_empty() {
        let completion = {
            let Some(worker) = executor.worker.as_ref() else {
                std::process::abort();
            };
            let Ok(completion) = worker.receiver.recv() else {
                std::process::abort();
            };
            completion
        };
        let task = completion.task as *mut LoomTask;
        let Some(index) = remaining.iter().position(|(candidate, registration)| {
            *candidate == task && *registration == completion.registration
        }) else {
            continue;
        };
        remaining.swap_remove(index);
        if executor_owns(executor, task)
            && unsafe {
                (*task)
                    .blocking_wait
                    .as_ref()
                    .is_some_and(|wait| wait.registration == completion.registration)
            }
        {
            unsafe { (*task).blocking_wait = None };
        }
        drop(completion.result);
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
        if unsafe { (*task).waits.is_empty() } {
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
        let blocking_matches = unsafe {
            (*task)
                .blocking_wait
                .as_ref()
                .is_some_and(|wait| wait.registration == completion.registration)
        };
        if !blocking_matches {
            continue;
        }
        unsafe {
            (*task).blocking_wait = None;
            if !(*task).cancel_requested {
                (*task).blocking_result = completion.result;
            }
        }
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
/// value or universal-value envelope.
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

fn typed_io_operation_matches(expected: TypedIoOperation, operation: &IoOperation) -> bool {
    matches!(
        (expected, operation),
        (
            TypedIoOperation::FileOpenRead,
            IoOperation::FileOpen { create: false, .. }
        ) | (
            TypedIoOperation::FileCreate,
            IoOperation::FileOpen { create: true, .. }
        ) | (TypedIoOperation::FileReadText, IoOperation::FileRead { .. })
            | (
                TypedIoOperation::FileWriteText,
                IoOperation::FileWrite { .. }
            )
            | (
                TypedIoOperation::SocketConnect,
                IoOperation::SocketConnect { .. }
            )
            | (
                TypedIoOperation::SocketReadText,
                IoOperation::SocketRead { .. }
            )
            | (
                TypedIoOperation::SocketWriteText,
                IoOperation::SocketWrite { .. }
            )
    )
}

unsafe fn validate_typed_io_poll(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
    scratch_text: *mut *mut c_void,
    outcome: *mut LoomTypedIoOutcome,
) -> Option<TypedIoOperation> {
    if task.is_null()
        || executor.is_null()
        || scratch_text.is_null()
        || outcome.is_null()
        || !(outcome as usize).is_multiple_of(align_of::<LoomTypedIoOutcome>())
        || unsafe { (*executor).cleanup_active() }
        || !executor_owns(unsafe { &*executor }, task)
        || unsafe { (*executor).active_task } != task
        || unsafe { (*task).executor } != executor
        || unsafe { (*task).status } != TaskStatus::Running
        || unsafe { (*task).cancel_requested }
        || !unsafe { (*task).waits.is_empty() }
        || !unsafe { (*task).io_fallible }
        || !unsafe { (*task).wait_leaf }
        || crate::gc::active_runtime_pointer() != unsafe { (*executor).runtime_pointer() }
    {
        return None;
    }
    let typed = unsafe { (*task).typed.as_ref() }?;
    if !typed.initialized
        || !typed.published
        || typed.result_initialized
        || typed.result_taken
        || typed.result_disposed
        || typed.root_state != 0
        || typed.live_non_result_root_offset(scratch_text).is_none()
        || unsafe { !(*scratch_text).is_null() }
    {
        return None;
    }
    let frame_start = typed.frame.as_ptr() as usize;
    let frame_end = frame_start.checked_add(typed.frame_layout.size())?;
    let outcome_start = outcome as usize;
    let outcome_end = outcome_start.checked_add(size_of::<LoomTypedIoOutcome>())?;
    if outcome_start < frame_end && frame_start < outcome_end {
        return None;
    }
    unsafe { (*task).typed_io_operation }
}

unsafe fn typed_io_publish_text(
    task: *mut LoomTask,
    bytes: &[u8],
    scratch_text: *mut *mut c_void,
) -> Result<(), i32> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_UTF8",
                "typed I/O attempted to publish invalid UTF-8 Text",
            )
        });
    };
    let Ok(scalar_length) = u64::try_from(text.chars().count()) else {
        return Err(unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_TEXT_SIZE",
                "typed I/O Text scalar length overflowed",
            )
        });
    };
    let status = unsafe { crate::text::allocate_typed_text(bytes, scalar_length, scratch_text) };
    if status == crate::GC_OK {
        Ok(())
    } else {
        Err(unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_TEXT",
                "typed I/O could not publish managed Text",
            )
        })
    }
}

unsafe fn finish_typed_io_resource(
    task: *mut LoomTask,
    expected: TypedIoOperation,
    outcome: *mut LoomTypedIoOutcome,
    kind: IoResourceKind,
    resource: OwnedResource,
) -> i32 {
    let Some(expected_kind) = expected.resource_kind() else {
        return unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_RESULT",
                "typed I/O operation produced an unexpected resource",
            )
        };
    };
    let resource_matches = expected_kind == kind && resource.is_file() == kind.is_file();
    let token = resource.token().cast_unsigned();
    if !resource_matches || token == TYPED_IO_INVALID_RESOURCE_TOKEN {
        return unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_RESOURCE",
                "typed I/O operation produced an invalid resource",
            )
        };
    }
    // The concrete RAII owner enters the Task ledger before generated code can
    // observe its capability token. Every non-completed terminal path clears
    // this ledger before the Task can be reaped.
    unsafe { (*task).owned_result_resources.push(resource) };
    unsafe {
        outcome.write(LoomTypedIoOutcome {
            kind: TYPED_IO_OUTCOME_RESOURCE,
            detail: 0,
            payload: token,
        });
    }
    TASK_COMPLETED
}

unsafe fn finish_typed_io_error(
    task: *mut LoomTask,
    scratch_text: *mut *mut c_void,
    outcome: *mut LoomTypedIoOutcome,
    class: IoFaultClass,
    kind: u32,
    message: &str,
) -> i32 {
    if kind > 9 {
        return unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_ERROR_KIND",
                "typed I/O produced an invalid IoErrorKind",
            )
        };
    }
    if let Err(step) = unsafe { typed_io_publish_text(task, message.as_bytes(), scratch_text) } {
        return step;
    }
    unsafe {
        outcome.write(LoomTypedIoOutcome {
            kind: TYPED_IO_OUTCOME_ERROR,
            detail: kind,
            payload: class.abi_payload(),
        });
    }
    TASK_COMPLETED
}

unsafe fn finish_typed_io_result(
    task: *mut LoomTask,
    expected: TypedIoOperation,
    scratch_text: *mut *mut c_void,
    outcome: *mut LoomTypedIoOutcome,
    result: BlockingResult,
) -> i32 {
    match result {
        BlockingResult::Resource { kind, resource } => unsafe {
            finish_typed_io_resource(task, expected, outcome, kind, resource)
        },
        BlockingResult::Text(bytes) => {
            if !expected.expects_text() {
                return unsafe {
                    fail_message(
                        task,
                        "LOOM_RUNTIME_TYPED_IO_RESULT",
                        "typed I/O operation produced unexpected Text",
                    )
                };
            }
            if std::str::from_utf8(&bytes).is_err() {
                return unsafe {
                    finish_typed_io_error(
                        task,
                        scratch_text,
                        outcome,
                        IoFaultClass::Operation,
                        3,
                        "I/O bytes are not valid UTF-8 Text",
                    )
                };
            }
            if let Err(step) = unsafe { typed_io_publish_text(task, &bytes, scratch_text) } {
                return step;
            }
            unsafe {
                outcome.write(LoomTypedIoOutcome {
                    kind: TYPED_IO_OUTCOME_TEXT,
                    detail: 0,
                    payload: 0,
                });
            }
            TASK_COMPLETED
        }
        BlockingResult::Unit => {
            if !expected.expects_unit() {
                return unsafe {
                    fail_message(
                        task,
                        "LOOM_RUNTIME_TYPED_IO_RESULT",
                        "typed I/O operation produced unexpected Unit",
                    )
                };
            }
            unsafe {
                outcome.write(LoomTypedIoOutcome {
                    kind: TYPED_IO_OUTCOME_UNIT,
                    detail: 0,
                    payload: 0,
                });
            }
            TASK_COMPLETED
        }
        BlockingResult::Fault {
            class,
            kind,
            message,
        } => unsafe { finish_typed_io_error(task, scratch_text, outcome, class, kind, &message) },
    }
}

unsafe fn finish_typed_io_progress(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
    expected: TypedIoOperation,
    scratch_text: *mut *mut c_void,
    outcome: *mut LoomTypedIoOutcome,
    progress: IoProgress,
) -> i32 {
    match progress {
        IoProgress::Ready(result) => unsafe {
            finish_typed_io_result(task, expected, scratch_text, outcome, result)
        },
        IoProgress::Suspend {
            operation,
            handle,
            interests,
        } => unsafe { suspend_io(task, executor, operation, handle, interests) },
    }
}

/// Advances the active typed I/O Task without knowing the target-layout
/// `Result`. Managed Text is published only into the compiler-declared scratch
/// root; all remaining completion data is pointer-free.
#[unsafe(export_name = "loom_typed_io_poll_v1")]
pub unsafe extern "C" fn typed_io_poll_v1(
    task: *mut c_void,
    executor: *mut c_void,
    scratch_text: *mut *mut c_void,
    outcome: *mut LoomTypedIoOutcome,
) -> i32 {
    let task = task.cast::<LoomTask>();
    let executor = executor.cast::<LoomExecutor>();
    let Some(expected) = (unsafe { validate_typed_io_poll(task, executor, scratch_text, outcome) })
    else {
        return TASK_FAULTED;
    };
    unsafe { outcome.write(LoomTypedIoOutcome::default()) };
    if let Some(result) = unsafe { (*task).blocking_result.take() } {
        return unsafe { finish_typed_io_result(task, expected, scratch_text, outcome, result) };
    }
    let Some(operation) = (unsafe { (*task).io_operation.take() }) else {
        return unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_COMPLETION",
                "typed I/O Task resumed without an operation or completion",
            )
        };
    };
    if !typed_io_operation_matches(expected, &operation) {
        return unsafe {
            fail_message(
                task,
                "LOOM_RUNTIME_TYPED_IO_OPERATION",
                "typed I/O Task operation does not match its request",
            )
        };
    }
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
            finish_typed_io_progress(
                task,
                executor,
                expected,
                scratch_text,
                outcome,
                advance_socket_read(socket, bytes),
            )
        },
        IoOperation::SocketWrite {
            socket,
            bytes,
            offset,
        } => unsafe {
            finish_typed_io_progress(
                task,
                executor,
                expected,
                scratch_text,
                outcome,
                advance_socket_write(socket, bytes, offset),
            )
        },
    }
}

/// Exact non-suspending cleanup callback for typed I/O leaf descriptors.
#[unsafe(export_name = "loom_typed_io_cancel_v1")]
pub unsafe extern "C" fn typed_io_cancel_v1(
    task: *mut c_void,
    executor: *mut c_void,
    frame: *mut c_void,
) -> i32 {
    let task = task.cast::<LoomTask>();
    let executor = executor.cast::<LoomExecutor>();
    if task.is_null()
        || executor.is_null()
        || frame.is_null()
        || !executor_owns(unsafe { &*executor }, task)
        || unsafe { (*task).executor } != executor
        || unsafe { (*executor).active_task } != task
        || unsafe { (*task).status } != TaskStatus::Running
        || !unsafe { (*task).cancel_requested }
        || !unsafe { (*task).io_fallible }
        || unsafe { (*task).typed_io_operation.is_none() }
        || unsafe {
            (*task)
                .typed
                .as_ref()
                .is_none_or(|typed| !typed.initialized || typed.frame_pointer() != frame)
        }
    {
        return TASK_FAULTED;
    }
    TASK_CANCELLED
}

unsafe fn suspend_blocking<F>(task: *mut LoomTask, executor: *mut LoomExecutor, work: F) -> i32
where
    F: FnOnce() -> BlockingResult + Send + 'static,
{
    if unsafe { (*task).blocking_wait.is_some() } {
        return unsafe {
            fail_message(
                task,
                "IoWaitFault",
                "I/O task already owns a blocking completion",
            )
        };
    }
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
    let state = Arc::new(AtomicU8::new(BLOCKING_QUEUED));
    let submission_state = Arc::clone(&state);
    let submission: BlockingJob = Box::new(move || {
        let result = work();
        let result = (submission_state.load(Ordering::Acquire) != BLOCKING_CANCELLED_STARTED)
            .then_some(result);
        let _ = sender.send(WorkerCompletion {
            task: task_address,
            registration,
            result,
        });
        let _ = poller.notify();
    });
    let submission = Arc::new(Mutex::new(Some(submission)));
    unsafe {
        (*task).blocking_wait = Some(BlockingWait {
            registration,
            state: Arc::clone(&state),
            submission: Arc::clone(&submission),
        });
    }
    let job: BlockingJob = Box::new(move || {
        let _ = run_blocking_submission(&state, &submission);
    });
    match blocking_pool().try_send(job) {
        Ok(()) => TASK_PENDING,
        Err(mpsc::TrySendError::Full(_) | mpsc::TrySendError::Disconnected(_)) => {
            let _ = unsafe { cancel_for_task(&raw mut *executor, &raw const registration) };
            unsafe {
                (*task).waits.retain(|candidate| *candidate != registration);
                (*task).blocking_wait = None;
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
            kind: IoResourceKind::File,
            resource: file.into(),
        },
        Err(error) => BlockingResult::Fault {
            class: IoFaultClass::Operation,
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

fn blocking_file_read(mut file: File) -> BlockingResult {
    let mut bytes = Vec::new();
    match file.read_to_end(&mut bytes) {
        Ok(_) => BlockingResult::Text(bytes),
        Err(error) => BlockingResult::Fault {
            class: IoFaultClass::Operation,
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

fn blocking_file_write(mut file: File, bytes: &[u8]) -> BlockingResult {
    match file.write_all(bytes) {
        Ok(()) => BlockingResult::Unit,
        Err(error) => BlockingResult::Fault {
            class: IoFaultClass::Operation,
            kind: io_error_kind(&error),
            message: error.to_string(),
        },
    }
}

fn blocking_socket_connect(host: &str, port: u16) -> BlockingResult {
    let addresses = match (host, port).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(error) => {
            return BlockingResult::Fault {
                class: IoFaultClass::SocketResolve,
                kind: io_error_kind(&error),
                message: error.to_string(),
            };
        }
    };
    blocking_socket_connect_addresses(addresses)
}

fn blocking_socket_connect_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> BlockingResult {
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect(address) {
            Ok(socket) => match socket.set_nonblocking(true) {
                Ok(()) => {
                    return BlockingResult::Resource {
                        kind: IoResourceKind::Socket,
                        resource: socket.into(),
                    };
                }
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(error),
        }
    }
    let Some(error) = last_error else {
        return BlockingResult::Fault {
            class: IoFaultClass::SocketResolve,
            kind: 9,
            message: "host resolved to no addresses".into(),
        };
    };
    BlockingResult::Fault {
        class: IoFaultClass::Operation,
        kind: io_error_kind(&error),
        message: error.to_string(),
    }
}

fn advance_socket_read(mut socket: TcpStream, mut bytes: Vec<u8>) -> IoProgress {
    let handle = socket_handle_bits(&socket);
    let mut chunk = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        match socket.read(&mut chunk) {
            Ok(0) => {
                return IoProgress::Ready(BlockingResult::Text(bytes));
            }
            Ok(length) => bytes.extend_from_slice(&chunk[..length]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return IoProgress::Suspend {
                    operation: IoOperation::SocketRead { socket, bytes },
                    handle,
                    interests: crate::WAIT_READABLE,
                };
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return IoProgress::Ready(BlockingResult::Fault {
                    class: IoFaultClass::Operation,
                    kind: io_error_kind(&error),
                    message: error.to_string(),
                });
            }
        }
    }
}

fn advance_socket_write(mut socket: TcpStream, bytes: Vec<u8>, mut offset: usize) -> IoProgress {
    if offset == bytes.len() {
        return IoProgress::Ready(BlockingResult::Unit);
    }
    let handle = socket_handle_bits(&socket);
    loop {
        if offset == bytes.len() {
            return IoProgress::Ready(BlockingResult::Unit);
        }
        match socket.write(&bytes[offset..]) {
            Ok(0) => {
                return IoProgress::Ready(BlockingResult::Fault {
                    class: IoFaultClass::Operation,
                    kind: 9,
                    message: "socket accepted zero bytes".into(),
                });
            }
            Ok(written) => {
                offset += written;
                if offset == bytes.len() {
                    return IoProgress::Ready(BlockingResult::Unit);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return IoProgress::Suspend {
                    operation: IoOperation::SocketWrite {
                        socket,
                        bytes,
                        offset,
                    },
                    handle,
                    interests: crate::WAIT_WRITABLE,
                };
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                return IoProgress::Ready(BlockingResult::Fault {
                    class: IoFaultClass::Operation,
                    kind: io_error_kind(&error),
                    message: error.to_string(),
                });
            }
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

unsafe fn fail_message(task: *mut LoomTask, code: &str, message: &str) -> i32 {
    if !task.is_null() {
        // SAFETY: the caller supplied a live task owned by its executor.
        unsafe {
            record_primary_task_fault(&mut *task, code.into(), message.into(), String::new());
        }
    }
    TASK_FAULTED
}

fn empty_universal_descriptor() -> LoomCoroutineDescriptor {
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
        descriptor: empty_universal_descriptor(),
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
        blocking_wait: None,
        io_fallible: false,
        typed_io_operation: None,
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
        blocking_wait: None,
        io_fallible: false,
        typed_io_operation: None,
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

#[cfg(test)]
unsafe extern "C" fn resume_slow_blocking_fixture(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
) -> i32 {
    if unsafe { (*task).cancel_requested } {
        return TASK_CANCELLED;
    }
    if unsafe { (*task).blocking_result.take().is_some() } {
        return TASK_COMPLETED;
    }
    unsafe {
        suspend_blocking(task, executor, || {
            SLOW_BLOCKING_STARTED.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(200));
            SLOW_BLOCKING_FINISHED.store(true, Ordering::SeqCst);
            BlockingResult::Unit
        })
    }
}

#[cfg(test)]
static SLOW_BLOCKING_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static SLOW_BLOCKING_FINISHED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static SLOW_BLOCKING_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) unsafe fn spawn_slow_blocking_fixture(executor: *mut LoomExecutor) -> *mut LoomTask {
    unsafe { task_spawn(executor, Some(resume_slow_blocking_fixture), 1, 0) }
}

#[cfg(test)]
pub(crate) fn reset_slow_blocking_fixture() {
    SLOW_BLOCKING_STARTED.store(false, Ordering::SeqCst);
    SLOW_BLOCKING_FINISHED.store(false, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) fn slow_blocking_fixture_state() -> (bool, bool) {
    (
        SLOW_BLOCKING_STARTED.load(Ordering::SeqCst),
        SLOW_BLOCKING_FINISHED.load(Ordering::SeqCst),
    )
}

#[cfg(test)]
pub(crate) fn lock_slow_blocking_fixture() -> std::sync::MutexGuard<'static, ()> {
    SLOW_BLOCKING_TEST_LOCK.lock().unwrap()
}

#[cfg(test)]
unsafe extern "C" fn resume_controlled_blocking_fixture(
    task: *mut LoomTask,
    executor: *mut LoomExecutor,
) -> i32 {
    if unsafe { (*task).cancel_requested } {
        return TASK_CANCELLED;
    }
    if unsafe { (*task).blocking_result.take().is_some() } {
        return TASK_COMPLETED;
    }
    unsafe {
        suspend_blocking(task, executor, || {
            CONTROLLED_BLOCKING_STARTED.store(true, Ordering::Release);
            while !CONTROLLED_BLOCKING_RELEASED.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            CONTROLLED_BLOCKING_FINISHED.store(true, Ordering::Release);
            BlockingResult::Unit
        })
    }
}

#[cfg(test)]
static CONTROLLED_BLOCKING_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static CONTROLLED_BLOCKING_RELEASED: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
static CONTROLLED_BLOCKING_FINISHED: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
fn reset_controlled_blocking_fixture() {
    CONTROLLED_BLOCKING_STARTED.store(false, Ordering::Release);
    CONTROLLED_BLOCKING_RELEASED.store(false, Ordering::Release);
    CONTROLLED_BLOCKING_FINISHED.store(false, Ordering::Release);
}

unsafe fn copy_text(data: *const u8, length: u64) -> Option<String> {
    let length = usize::try_from(length).ok()?;
    if length > isize::MAX as usize || data.is_null() && length != 0 {
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

enum PreparedTypedIo {
    Operation(IoOperation),
    Immediate(BlockingResult),
}

fn typed_io_failure(class: IoFaultClass, kind: u32, message: impl Into<String>) -> PreparedTypedIo {
    PreparedTypedIo::Immediate(BlockingResult::Fault {
        class,
        kind,
        message: message.into(),
    })
}

fn canonical_empty_view(view: LoomByteView) -> bool {
    view.data.is_null() && view.length == 0
}

unsafe fn prepare_typed_io_request(
    executor: *mut LoomExecutor,
    request: LoomTypedIoRequest,
) -> Option<(TypedIoOperation, PreparedTypedIo)> {
    if request.abi_version != TYPED_IO_ABI_VERSION {
        return None;
    }
    let operation = TypedIoOperation::from_abi(request.operation)?;
    let prepared = match operation {
        TypedIoOperation::FileOpenRead
        | TypedIoOperation::FileCreate
        | TypedIoOperation::FileReadText
        | TypedIoOperation::FileWriteText => {
            unsafe { prepare_typed_file_request(executor, operation, request) }?
        }
        TypedIoOperation::SocketConnect
        | TypedIoOperation::SocketReadText
        | TypedIoOperation::SocketWriteText => {
            unsafe { prepare_typed_socket_request(executor, operation, request) }?
        }
    };
    Some((operation, prepared))
}

unsafe fn prepare_typed_file_request(
    executor: *mut LoomExecutor,
    operation: TypedIoOperation,
    request: LoomTypedIoRequest,
) -> Option<PreparedTypedIo> {
    let no_resource = request.resource_token == TYPED_IO_INVALID_RESOURCE_TOKEN;
    let token = request.resource_token.cast_signed();
    Some(match operation {
        TypedIoOperation::FileOpenRead | TypedIoOperation::FileCreate => {
            if !no_resource || request.auxiliary != 0 {
                return None;
            }
            let path = unsafe { copy_text(request.argument.data, request.argument.length) }?;
            PreparedTypedIo::Operation(IoOperation::FileOpen {
                path,
                create: operation == TypedIoOperation::FileCreate,
            })
        }
        TypedIoOperation::FileReadText => {
            if !canonical_empty_view(request.argument) || request.auxiliary != 0 {
                return None;
            }
            if no_resource {
                typed_io_failure(IoFaultClass::Operation, 8, "file resource is closed")
            } else {
                match unsafe { clone_active_file(executor, token) } {
                    Ok(file) => PreparedTypedIo::Operation(IoOperation::FileRead { file }),
                    Err(ResourceAccessError::Host(error)) => typed_io_failure(
                        IoFaultClass::Operation,
                        io_error_kind(&error),
                        error.to_string(),
                    ),
                    Err(ResourceAccessError::InvalidOwnership) => return None,
                }
            }
        }
        TypedIoOperation::FileWriteText => {
            if request.auxiliary != 0 {
                return None;
            }
            let text = unsafe { copy_text(request.argument.data, request.argument.length) }?;
            if no_resource {
                typed_io_failure(IoFaultClass::Operation, 8, "file resource is closed")
            } else {
                match unsafe { clone_active_file(executor, token) } {
                    Ok(file) => PreparedTypedIo::Operation(IoOperation::FileWrite {
                        file,
                        bytes: text.into_bytes(),
                    }),
                    Err(ResourceAccessError::Host(error)) => typed_io_failure(
                        IoFaultClass::Operation,
                        io_error_kind(&error),
                        error.to_string(),
                    ),
                    Err(ResourceAccessError::InvalidOwnership) => return None,
                }
            }
        }
        TypedIoOperation::SocketConnect
        | TypedIoOperation::SocketReadText
        | TypedIoOperation::SocketWriteText => return None,
    })
}

unsafe fn prepare_typed_socket_request(
    executor: *mut LoomExecutor,
    operation: TypedIoOperation,
    request: LoomTypedIoRequest,
) -> Option<PreparedTypedIo> {
    let no_resource = request.resource_token == TYPED_IO_INVALID_RESOURCE_TOKEN;
    let token = request.resource_token.cast_signed();
    Some(match operation {
        TypedIoOperation::SocketConnect => {
            if !no_resource {
                return None;
            }
            let host = unsafe { copy_text(request.argument.data, request.argument.length) }?;
            match u16::try_from(request.auxiliary) {
                Ok(port) => PreparedTypedIo::Operation(IoOperation::SocketConnect { host, port }),
                Err(_) => typed_io_failure(
                    IoFaultClass::InvalidPort,
                    3,
                    "socket port must be in 0..=65535",
                ),
            }
        }
        TypedIoOperation::SocketReadText => {
            if !canonical_empty_view(request.argument) || request.auxiliary != 0 {
                return None;
            }
            if no_resource {
                typed_io_failure(IoFaultClass::Operation, 8, "socket resource is closed")
            } else {
                match unsafe { clone_active_socket(executor, token) } {
                    Ok(socket) => PreparedTypedIo::Operation(IoOperation::SocketRead {
                        socket,
                        bytes: Vec::new(),
                    }),
                    Err(ResourceAccessError::Host(error)) => typed_io_failure(
                        IoFaultClass::Operation,
                        io_error_kind(&error),
                        error.to_string(),
                    ),
                    Err(ResourceAccessError::InvalidOwnership) => return None,
                }
            }
        }
        TypedIoOperation::SocketWriteText => {
            if request.auxiliary != 0 {
                return None;
            }
            let text = unsafe { copy_text(request.argument.data, request.argument.length) }?;
            if no_resource {
                typed_io_failure(IoFaultClass::Operation, 8, "socket resource is closed")
            } else {
                match unsafe { clone_active_socket(executor, token) } {
                    Ok(socket) => PreparedTypedIo::Operation(IoOperation::SocketWrite {
                        socket,
                        bytes: text.into_bytes(),
                        offset: 0,
                    }),
                    Err(ResourceAccessError::Host(error)) => typed_io_failure(
                        IoFaultClass::Operation,
                        io_error_kind(&error),
                        error.to_string(),
                    ),
                    Err(ResourceAccessError::InvalidOwnership) => return None,
                }
            }
        }
        TypedIoOperation::FileOpenRead
        | TypedIoOperation::FileCreate
        | TypedIoOperation::FileReadText
        | TypedIoOperation::FileWriteText => return None,
    })
}

/// Creates and publishes one typed I/O leaf Task.
///
/// Every borrowed Text is copied and every input resource is duplicated before
/// this call returns. Ordinary host and closed-resource failures are stored as
/// immediate I/O outcomes; null is reserved for an invalid ABI call or typed
/// Task allocation/publication failure.
#[unsafe(export_name = "loom_typed_io_task_create_v1")]
pub unsafe extern "C" fn typed_io_task_create_v1(
    executor: *mut LoomExecutor,
    descriptor: *const LoomTypedCoroutineDescriptor,
    request: *const LoomTypedIoRequest,
) -> *mut LoomTask {
    if executor.is_null() || unsafe { (*executor).cleanup_active() } || !is_aligned_for(request) {
        return ptr::null_mut();
    }
    // SAFETY: the ABI requires one aligned readable request for this call. It
    // is copied before any Task or operation storage can retain state.
    let request = unsafe { *request };
    let Some((operation, prepared)) = (unsafe { prepare_typed_io_request(executor, request) })
    else {
        return ptr::null_mut();
    };
    let task = unsafe { typed_task_create_v1(executor, descriptor) };
    if task.is_null() {
        return ptr::null_mut();
    }
    let root_shape_valid = unsafe {
        (*task)
            .typed
            .as_ref()
            .is_some_and(TypedTaskStorage::has_typed_io_root_shape)
    };
    if !root_shape_valid {
        let _ = unsafe { typed_task_abort_unpublished_v1(executor, task) };
        return ptr::null_mut();
    }
    unsafe {
        (*task).typed_io_operation = Some(operation);
        (*task).io_fallible = true;
        (*task).wait_leaf = true;
        match prepared {
            PreparedTypedIo::Operation(io) => (*task).io_operation = Some(io),
            PreparedTypedIo::Immediate(result) => (*task).blocking_result = Some(result),
        }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CloseResourceError {
    InvalidOwnership,
}

enum ResourceAccessError {
    InvalidOwnership,
    Host(io::Error),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResourceAccess {
    Operation,
    Close,
}

fn select_tracked_resource(
    candidates: impl IntoIterator<Item = (usize, usize, bool)>,
) -> Result<Option<(usize, usize)>, CloseResourceError> {
    let mut selected = None;
    for (task_index, resource_index, kind_matches) in candidates {
        if selected
            .replace((task_index, resource_index, kind_matches))
            .is_some()
        {
            return Err(CloseResourceError::InvalidOwnership);
        }
    }
    match selected {
        Some((task_index, resource_index, true)) => Ok(Some((task_index, resource_index))),
        Some((_, _, false)) => Err(CloseResourceError::InvalidOwnership),
        None => Ok(None),
    }
}

fn active_resource_location(
    executor: &LoomExecutor,
    token: i64,
    kind: IoResourceKind,
    access: ResourceAccess,
) -> Result<(usize, usize), CloseResourceError> {
    let active = executor.active_task;
    if active.is_null() || !executor_owns(executor, active) {
        return Err(CloseResourceError::InvalidOwnership);
    }
    let task = unsafe { &*active };
    let authorized = match access {
        ResourceAccess::Operation => {
            task.status == TaskStatus::Running
                && !task.cancel_requested
                && !executor.cleanup_active()
        }
        ResourceAccess::Close => {
            executor.cleanup_active()
                || (task.status == TaskStatus::Running && !task.cancel_requested)
        }
    };
    if !authorized {
        return Err(CloseResourceError::InvalidOwnership);
    }
    let candidates = executor
        .tasks
        .iter()
        .enumerate()
        .flat_map(|(task_index, task)| {
            task.owned_result_resources.iter().enumerate().filter_map(
                move |(resource_index, candidate)| {
                    (candidate.token() == token).then_some((
                        task_index,
                        resource_index,
                        candidate.is_file() == kind.is_file(),
                    ))
                },
            )
        });
    let location =
        select_tracked_resource(candidates)?.ok_or(CloseResourceError::InvalidOwnership)?;
    if !ptr::eq::<LoomTask>(&raw const *executor.tasks[location.0], active) {
        return Err(CloseResourceError::InvalidOwnership);
    }
    Ok(location)
}

unsafe fn clone_active_file(
    executor: *mut LoomExecutor,
    token: i64,
) -> Result<File, ResourceAccessError> {
    let Some(executor) = (unsafe { executor.as_ref() }) else {
        return Err(ResourceAccessError::InvalidOwnership);
    };
    let (task_index, resource_index) = active_resource_location(
        executor,
        token,
        IoResourceKind::File,
        ResourceAccess::Operation,
    )
    .map_err(|_| ResourceAccessError::InvalidOwnership)?;
    executor.tasks[task_index].owned_result_resources[resource_index]
        .try_clone_file()
        .map_err(ResourceAccessError::Host)
}

unsafe fn clone_active_socket(
    executor: *mut LoomExecutor,
    token: i64,
) -> Result<TcpStream, ResourceAccessError> {
    let Some(executor) = (unsafe { executor.as_ref() }) else {
        return Err(ResourceAccessError::InvalidOwnership);
    };
    let (task_index, resource_index) = active_resource_location(
        executor,
        token,
        IoResourceKind::Socket,
        ResourceAccess::Operation,
    )
    .map_err(|_| ResourceAccessError::InvalidOwnership)?;
    executor.tasks[task_index].owned_result_resources[resource_index]
        .try_clone_socket()
        .map_err(ResourceAccessError::Host)
}

fn close_resource_token(
    executor: &mut LoomExecutor,
    token: i64,
    kind: IoResourceKind,
) -> Result<(), CloseResourceError> {
    let (task_index, resource_index) =
        active_resource_location(executor, token, kind, ResourceAccess::Close)?;
    drop(
        executor.tasks[task_index]
            .owned_result_resources
            .swap_remove(resource_index),
    );
    Ok(())
}

/// Closes one exact typed File/Socket record in place.
///
/// The current generated-code interval must already own `executor`. This call
/// performs no scheduling and never constructs a universal value envelope. The
/// capability token must have one exact owner in the active Task. Normal code
/// may close only from a running, non-cancelled Task; compiler-generated
/// cancellation and result-disposal callbacks may close during the executor's
/// guarded cleanup phase. Untracked, stale, sibling-owned, opposite-kind, and
/// duplicate entries fail before any close or ownership mutation.
#[unsafe(export_name = "loom_typed_resource_close_v1")]
pub unsafe extern "C" fn typed_resource_close_v1(
    executor: *mut LoomExecutor,
    kind: u32,
    token: *mut i64,
) -> i32 {
    if executor.is_null() || token.is_null() || !(token as usize).is_multiple_of(align_of::<i64>())
    {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    }
    let Some(kind) = IoResourceKind::from_typed_kind(kind) else {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    };
    // SAFETY: the executor pointer is borrowed for this call. The active
    // runtime and attachment checks prove this is the live generated-code
    // interval and serialize ledger mutation.
    let executor = unsafe { &mut *executor };
    let runtime = executor.runtime_pointer();
    if runtime.is_null()
        || crate::gc::active_runtime_pointer() != runtime
        || !unsafe { (*runtime).is_attached_executor(ptr::from_mut(executor).cast::<c_void>()) }
    {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    }
    let current = unsafe { *token };
    if current == INVALID_RESOURCE_TOKEN {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    }
    if close_resource_token(executor, current, kind).is_err() {
        return TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT;
    }
    unsafe { *token = INVALID_RESOURCE_TOKEN };
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
            let cancelling = unsafe { (*task).cancel_requested };
            let resume = if cancelling {
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
            let cleanup = if cancelling {
                CleanupPhaseGuard::enter(unsafe { &mut *executor })
            } else {
                None
            };
            if cancelling && cleanup.is_none() {
                unsafe {
                    record_primary_task_fault(
                        &mut *task,
                        "LOOM_RUNTIME_CLEANUP_DEPTH".into(),
                        "coroutine cancellation exceeded the cleanup nesting limit".into(),
                        String::new(),
                    );
                }
                TASK_FAULTED
            } else {
                enter_executor(executor);
                let step = unsafe { resume(task, executor) };
                leave_executor();
                drop(cleanup);
                step
            }
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
mod typed_io_tests {
    use std::ffi::c_void;
    use std::fs::{self, File};
    use std::io::{Read, Write};
    use std::mem::{MaybeUninit, align_of, offset_of, size_of};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::ptr;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct TestIoResult {
        outcome: LoomTypedIoOutcome,
        text: *mut c_void,
    }

    impl Default for TestIoResult {
        fn default() -> Self {
            Self {
                outcome: LoomTypedIoOutcome::default(),
                text: ptr::null_mut(),
            }
        }
    }

    #[repr(C)]
    struct TestIoFrame {
        result: TestIoResult,
        scratch_text: *mut c_void,
    }

    unsafe extern "C" fn resume_typed_io_fixture(
        task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let frame = frame.cast::<TestIoFrame>();
        let mut outcome = LoomTypedIoOutcome::default();
        let step = unsafe {
            typed_io_poll_v1(
                task,
                executor,
                &raw mut (*frame).scratch_text,
                &raw mut outcome,
            )
        };
        if step != TASK_COMPLETED {
            return step;
        }
        unsafe {
            (*frame).result = TestIoResult {
                outcome,
                text: (*frame).scratch_text,
            };
            (*frame).scratch_text = ptr::null_mut();
        }
        if unsafe { typed_task_publish_result_v1(task.cast()) } == TYPED_TASK_OK {
            TASK_COMPLETED
        } else {
            TASK_FAULTED
        }
    }

    unsafe fn create_typed_io_task(
        executor: *mut LoomExecutor,
        request: &LoomTypedIoRequest,
    ) -> *mut LoomTask {
        let roots = [
            (offset_of!(TestIoFrame, result) + offset_of!(TestIoResult, text)) as u64,
            offset_of!(TestIoFrame, scratch_text) as u64,
        ];
        // State zero keeps only the out-of-result scratch Text alive. State
        // one is the exact completed result row and keeps only result.text.
        let live_bitmaps = [2_u64, 1_u64];
        let descriptor = typed_io_descriptor(&roots, &live_bitmaps, 2, 1);
        unsafe { typed_io_task_create_v1(executor, &raw const descriptor, ptr::from_ref(request)) }
    }

    unsafe extern "C" fn complete_resource_owner_fixture(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_COMPLETED
    }

    unsafe fn activate_resource_owner(executor: *mut LoomExecutor) -> *mut LoomTask {
        assert!(unsafe { (*executor).active_task.is_null() });
        let owner = unsafe { task_spawn(executor, Some(complete_resource_owner_fixture), 1, 0) };
        assert!(!owner.is_null());
        let index = unsafe {
            (*executor)
                .runnable
                .iter()
                .position(|candidate| *candidate == owner)
                .expect("resource owner must be runnable")
        };
        assert_eq!(unsafe { (*executor).runnable.remove(index) }, Some(owner));
        unsafe {
            (*owner).queued = false;
            (*owner).status = TaskStatus::Running;
            (*executor).active_task = owner;
        }
        owner
    }

    unsafe fn finish_resource_owner(executor: *mut LoomExecutor, owner: *mut LoomTask) {
        assert_eq!(unsafe { (*executor).active_task }, owner);
        let children = unsafe { std::mem::take(&mut (*owner).owned_children) };
        for child in children {
            assert_eq!(unsafe { (*child).owner }, owner);
            unsafe { (*child).owner = ptr::null_mut() };
        }
        unsafe {
            (*executor).active_task = ptr::null_mut();
            complete_terminal(&mut *executor, owner, TASK_CANCELLED);
        }
    }

    unsafe fn with_active_resource<T>(
        executor: *mut LoomExecutor,
        resource: OwnedResource,
        action: impl FnOnce(i64) -> T,
    ) -> T {
        let token = resource.token();
        let owner = unsafe { activate_resource_owner(executor) };
        unsafe { (*owner).owned_result_resources.push(resource) };
        enter_executor(executor);
        let result = action(token);
        leave_executor();
        unsafe { finish_resource_owner(executor, owner) };
        result
    }

    fn typed_io_descriptor(
        roots: &[u64],
        live_bitmaps: &[u64],
        root_state_count: u64,
        completed_root_state: u64,
    ) -> LoomTypedCoroutineDescriptor {
        LoomTypedCoroutineDescriptor {
            abi_version: TYPED_TASK_ABI_VERSION,
            flags: 0,
            resume: Some(resume_typed_io_fixture),
            cancel: Some(typed_io_cancel_v1),
            dispose_result: None,
            frame_size: size_of::<TestIoFrame>() as u64,
            frame_align: align_of::<TestIoFrame>() as u64,
            result_offset: offset_of!(TestIoFrame, result) as u64,
            result_size: size_of::<TestIoResult>() as u64,
            result_align: align_of::<TestIoResult>() as u64,
            root_slot_count: roots.len() as u64,
            root_state_count,
            root_bitmap_words: u64::from(!roots.is_empty()),
            root_offsets: if roots.is_empty() {
                ptr::null()
            } else {
                roots.as_ptr()
            },
            live_bitmaps: if live_bitmaps.is_empty() {
                ptr::null()
            } else {
                live_bitmaps.as_ptr()
            },
            completed_root_state,
        }
    }

    unsafe fn assert_typed_io_descriptor_rejected(
        executor: *mut LoomExecutor,
        descriptor: &LoomTypedCoroutineDescriptor,
        request: &LoomTypedIoRequest,
    ) {
        assert!(
            unsafe {
                typed_io_task_create_v1(executor, ptr::from_ref(descriptor), ptr::from_ref(request))
            }
            .is_null()
        );
        assert_eq!(unsafe { executor_live_tasks(executor) }, 0);
    }

    fn request(
        operation: u32,
        resource_token: u64,
        argument: &[u8],
        auxiliary: i64,
    ) -> LoomTypedIoRequest {
        LoomTypedIoRequest {
            abi_version: TYPED_IO_ABI_VERSION,
            operation,
            resource_token,
            argument: LoomByteView {
                data: if argument.is_empty() {
                    ptr::null()
                } else {
                    argument.as_ptr()
                },
                length: argument.len() as u64,
            },
            auxiliary,
        }
    }

    fn no_argument_request(
        operation: u32,
        resource_token: u64,
        auxiliary: i64,
    ) -> LoomTypedIoRequest {
        request(operation, resource_token, &[], auxiliary)
    }

    fn runtime_and_executor() -> (*mut LoomRuntime, *mut LoomExecutor) {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        (runtime, executor)
    }

    unsafe fn destroy(runtime: *mut LoomRuntime, executor: *mut LoomExecutor) {
        unsafe {
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    unsafe fn run_and_take(executor: *mut LoomExecutor, task: *mut LoomTask) -> TestIoResult {
        assert!(!task.is_null());
        assert_eq!(unsafe { executor_run(executor, task) }, TASK_COMPLETED);
        let mut result = MaybeUninit::<TestIoResult>::uninit();
        assert_eq!(
            unsafe {
                typed_task_take_result_v1(
                    task,
                    result.as_mut_ptr().cast(),
                    size_of::<TestIoResult>() as u64,
                    align_of::<TestIoResult>() as u64,
                )
            },
            TYPED_TASK_OK
        );
        unsafe { result.assume_init() }
    }

    fn unique_path(stem: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "loom-{stem}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn typed_file_try_operations_publish_exact_outcomes_and_snapshot_inputs() {
        let (runtime, executor) = runtime_and_executor();
        let path = unique_path("typed-io-file");
        let path_text = path.to_string_lossy().into_owned();
        unsafe {
            let create_request = request(
                TYPED_IO_OPERATION_FILE_CREATE,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                path_text.as_bytes(),
                0,
            );
            let create = create_typed_io_task(executor, &create_request);
            let created = run_and_take(executor, create);
            assert_eq!(created.outcome.kind, TYPED_IO_OUTCOME_RESOURCE);
            assert_ne!(created.outcome.payload, TYPED_IO_INVALID_RESOURCE_TOKEN);
            assert_eq!((*create).owned_result_resources.len(), 1);
            destroy(runtime, executor);
        }

        let (runtime, executor) = runtime_and_executor();
        let file = File::options().read(true).write(true).open(&path).unwrap();
        let resource = OwnedResource::from(file);
        let mut source = b"snapshotted text".to_vec();
        unsafe {
            let write = with_active_resource(executor, resource, |token| {
                let write_request = request(
                    TYPED_IO_OPERATION_FILE_WRITE_TEXT,
                    token.cast_unsigned(),
                    &source,
                    0,
                );
                create_typed_io_task(executor, &write_request)
            });
            source.fill(b'x');
            let written = run_and_take(executor, write);
            assert_eq!(written.outcome.kind, TYPED_IO_OUTCOME_UNIT);
            assert_eq!(written.outcome.payload, 0);
            assert!(written.text.is_null());

            let file = File::open(&path).unwrap();
            let resource = OwnedResource::from(file);
            let read = with_active_resource(executor, resource, |token| {
                let read_request = no_argument_request(
                    TYPED_IO_OPERATION_FILE_READ_TEXT,
                    token.cast_unsigned(),
                    0,
                );
                create_typed_io_task(executor, &read_request)
            });
            let read = run_and_take(executor, read);
            assert_eq!(read.outcome.kind, TYPED_IO_OUTCOME_TEXT);
            assert_eq!(read.outcome.payload, 0);
            assert_eq!(
                crate::text::text_bytes(read.text).unwrap(),
                b"snapshotted text"
            );

            let open_request = request(
                TYPED_IO_OPERATION_FILE_OPEN_READ,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                path_text.as_bytes(),
                0,
            );
            let opened_task = create_typed_io_task(executor, &open_request);
            let opened = run_and_take(executor, opened_task);
            assert_eq!(opened.outcome.kind, TYPED_IO_OUTCOME_RESOURCE);
            assert_eq!((*opened_task).owned_result_resources.len(), 1);
            destroy(runtime, executor);
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn typed_file_try_open_failure_and_closed_resource_are_error_outcomes() {
        let (runtime, executor) = runtime_and_executor();
        let missing = unique_path("typed-io-missing");
        let missing = missing.to_string_lossy().into_owned();
        unsafe {
            let open_request = request(
                TYPED_IO_OPERATION_FILE_OPEN_READ,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                missing.as_bytes(),
                0,
            );
            let open = create_typed_io_task(executor, &open_request);
            assert!(!open.is_null());
            let failed = run_and_take(executor, open);
            assert_eq!(failed.outcome.kind, TYPED_IO_OUTCOME_ERROR);
            assert_eq!(failed.outcome.detail, 0);
            assert_eq!(failed.outcome.payload, TYPED_IO_FAULT_CLASS_OPERATION);
            assert!(!crate::text::text_bytes(failed.text).unwrap().is_empty());

            let closed_request = no_argument_request(
                TYPED_IO_OPERATION_FILE_READ_TEXT,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                0,
            );
            let closed = create_typed_io_task(executor, &closed_request);
            assert!(!closed.is_null());
            let closed = run_and_take(executor, closed);
            assert_eq!(closed.outcome.kind, TYPED_IO_OUTCOME_ERROR);
            assert_eq!(closed.outcome.detail, 8);
            assert_eq!(closed.outcome.payload, TYPED_IO_FAULT_CLASS_OPERATION);
            assert_eq!(
                crate::text::text_bytes(closed.text).unwrap(),
                b"file resource is closed"
            );
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_completed_text_remains_an_exact_root_across_moving_collection() {
        let (runtime, executor) = runtime_and_executor();
        let closed_request = no_argument_request(
            TYPED_IO_OPERATION_FILE_READ_TEXT,
            TYPED_IO_INVALID_RESOURCE_TOKEN,
            0,
        );
        unsafe {
            let task = create_typed_io_task(executor, &closed_request);
            assert_eq!(executor_run(executor, task), TASK_COMPLETED);
            let frame = (*task)
                .typed
                .as_ref()
                .unwrap()
                .frame_pointer()
                .cast::<TestIoFrame>();
            assert_eq!(
                crate::text::text_bytes((*frame).result.text).unwrap(),
                b"file resource is closed"
            );
            crate::gc::enter_executor(executor);
            crate::gc::collect(&mut *executor);
            crate::gc::leave_executor();
            assert_eq!(
                crate::text::text_bytes((*frame).result.text).unwrap(),
                b"file resource is closed"
            );
            let taken = run_and_take(executor, task);
            assert_eq!(taken.outcome.kind, TYPED_IO_OUTCOME_ERROR);
            destroy(runtime, executor);
        }
    }

    fn connected_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        client.set_nonblocking(true).unwrap();
        (client, server)
    }

    #[test]
    fn typed_socket_try_connect_read_and_write_use_the_shared_reactor_path() {
        let (runtime, executor) = runtime_and_executor();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = i64::from(listener.local_addr().unwrap().port());
        let accept = std::thread::spawn(move || listener.accept().unwrap().0);
        unsafe {
            let connect_request = request(
                TYPED_IO_OPERATION_SOCKET_CONNECT,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                b"127.0.0.1",
                port,
            );
            let connect = create_typed_io_task(executor, &connect_request);
            let connected = run_and_take(executor, connect);
            assert_eq!(connected.outcome.kind, TYPED_IO_OUTCOME_RESOURCE);
            assert_eq!((*connect).owned_result_resources.len(), 1);
            drop(accept.join().unwrap());

            let (client, mut server) = connected_pair();
            let mut source = b"socket snapshot".to_vec();
            let write = with_active_resource(executor, client.into(), |token| {
                let write_request = request(
                    TYPED_IO_OPERATION_SOCKET_WRITE_TEXT,
                    token.cast_unsigned(),
                    &source,
                    0,
                );
                create_typed_io_task(executor, &write_request)
            });
            source.fill(b'x');
            let written = run_and_take(executor, write);
            assert_eq!(written.outcome.kind, TYPED_IO_OUTCOME_UNIT);
            let mut received = vec![0_u8; b"socket snapshot".len()];
            server.read_exact(&mut received).unwrap();
            assert_eq!(received, b"socket snapshot");

            let (client, mut server) = connected_pair();
            server.write_all(b"socket read").unwrap();
            server.shutdown(Shutdown::Write).unwrap();
            let read = with_active_resource(executor, client.into(), |token| {
                let read_request = no_argument_request(
                    TYPED_IO_OPERATION_SOCKET_READ_TEXT,
                    token.cast_unsigned(),
                    0,
                );
                create_typed_io_task(executor, &read_request)
            });
            let read = run_and_take(executor, read);
            assert_eq!(read.outcome.kind, TYPED_IO_OUTCOME_TEXT);
            assert_eq!(crate::text::text_bytes(read.text).unwrap(), b"socket read");
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_socket_connect_preserves_specific_fault_classes() {
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let invalid_port_request = request(
                TYPED_IO_OPERATION_SOCKET_CONNECT,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                b"localhost",
                -1,
            );
            let invalid_port = run_and_take(
                executor,
                create_typed_io_task(executor, &invalid_port_request),
            );
            assert_eq!(invalid_port.outcome.kind, TYPED_IO_OUTCOME_ERROR);
            assert_eq!(invalid_port.outcome.detail, 3);
            assert_eq!(
                invalid_port.outcome.payload,
                TYPED_IO_FAULT_CLASS_INVALID_PORT
            );

            // An embedded NUL is valid Text but cannot cross the host resolver
            // boundary, providing a deterministic resolution failure without
            // depending on DNS or network availability.
            let resolve_request = request(
                TYPED_IO_OPERATION_SOCKET_CONNECT,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                b"invalid\0host",
                80,
            );
            let resolve = run_and_take(executor, create_typed_io_task(executor, &resolve_request));
            assert_eq!(resolve.outcome.kind, TYPED_IO_OUTCOME_ERROR);
            assert_eq!(resolve.outcome.payload, TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE);

            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_socket_read_cancellation_releases_registration_and_private_clone() {
        let (runtime, executor) = runtime_and_executor();
        let (client, _server) = connected_pair();
        unsafe {
            let task = with_active_resource(executor, client.into(), |token| {
                let read_request = no_argument_request(
                    TYPED_IO_OPERATION_SOCKET_READ_TEXT,
                    token.cast_unsigned(),
                    0,
                );
                create_typed_io_task(executor, &read_request)
            });
            assert!(!task.is_null());
            (*executor).runnable.clear();
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
            assert_eq!(run_typed_task_step(executor, task), TASK_PENDING);
            (*executor).active_task = ptr::null_mut();
            assert!((*task).status == TaskStatus::Waiting);
            assert_eq!((*task).waits.len(), 1);
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert!((*task).waits.is_empty());
            assert!((*task).io_operation.is_none());
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_rejects_a_sibling_resource_token() {
        let (runtime, executor) = runtime_and_executor();
        let (socket, _peer) = connected_pair();
        unsafe {
            let sibling = task_spawn(executor, Some(complete_resource_owner_fixture), 1, 0);
            assert!(!sibling.is_null());
            let resource = OwnedResource::from(socket);
            let token = resource.token();
            (*sibling).owned_result_resources.push(resource);

            let owner = activate_resource_owner(executor);
            let request = no_argument_request(
                TYPED_IO_OPERATION_SOCKET_READ_TEXT,
                token.cast_unsigned(),
                0,
            );
            enter_executor(executor);
            assert!(create_typed_io_task(executor, &request).is_null());
            leave_executor();
            assert!((*owner).owned_result_resources.is_empty());
            assert_eq!((*sibling).owned_result_resources.len(), 1);

            finish_resource_owner(executor, owner);
            complete_terminal(&mut *executor, sibling, TASK_CANCELLED);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_rejects_an_untracked_resource_token() {
        let (runtime, executor) = runtime_and_executor();
        let (socket, _peer) = connected_pair();
        let resource = OwnedResource::from(socket);
        let token = resource.token();
        unsafe {
            let owner = activate_resource_owner(executor);
            let request = no_argument_request(
                TYPED_IO_OPERATION_SOCKET_READ_TEXT,
                token.cast_unsigned(),
                0,
            );
            enter_executor(executor);
            assert!(create_typed_io_task(executor, &request).is_null());
            leave_executor();
            assert!((*owner).owned_result_resources.is_empty());

            finish_resource_owner(executor, owner);
            drop(resource);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_rejects_a_stale_resource_token() {
        let (runtime, executor) = runtime_and_executor();
        let (socket, _peer) = connected_pair();
        unsafe {
            let owner = activate_resource_owner(executor);
            let resource = OwnedResource::from(socket);
            let token = resource.token();
            (*owner).owned_result_resources.push(resource);
            (*owner).owned_result_resources.clear();

            let request = no_argument_request(
                TYPED_IO_OPERATION_SOCKET_READ_TEXT,
                token.cast_unsigned(),
                0,
            );
            enter_executor(executor);
            assert!(create_typed_io_task(executor, &request).is_null());
            leave_executor();
            assert!((*owner).owned_result_resources.is_empty());

            finish_resource_owner(executor, owner);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_rejects_a_wrong_kind_resource_token() {
        let (runtime, executor) = runtime_and_executor();
        let (socket, _peer) = connected_pair();
        unsafe {
            let owner = activate_resource_owner(executor);
            let resource = OwnedResource::from(socket);
            let token = resource.token();
            (*owner).owned_result_resources.push(resource);

            let request =
                no_argument_request(TYPED_IO_OPERATION_FILE_READ_TEXT, token.cast_unsigned(), 0);
            enter_executor(executor);
            assert!(create_typed_io_task(executor, &request).is_null());
            leave_executor();
            assert_eq!((*owner).owned_result_resources.len(), 1);

            finish_resource_owner(executor, owner);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_rejects_noncanonical_requests() {
        let (runtime, executor) = runtime_and_executor();
        let bad_version = LoomTypedIoRequest {
            abi_version: TYPED_IO_ABI_VERSION + 1,
            ..no_argument_request(
                TYPED_IO_OPERATION_FILE_READ_TEXT,
                TYPED_IO_INVALID_RESOURCE_TOKEN,
                0,
            )
        };
        unsafe {
            assert!(create_typed_io_task(executor, &bad_version).is_null());
            let malformed = [
                no_argument_request(u32::MAX, TYPED_IO_INVALID_RESOURCE_TOKEN, 0),
                request(TYPED_IO_OPERATION_FILE_OPEN_READ, 0, b"path", 0),
                request(
                    TYPED_IO_OPERATION_FILE_READ_TEXT,
                    TYPED_IO_INVALID_RESOURCE_TOKEN,
                    b"unexpected",
                    0,
                ),
                request(
                    TYPED_IO_OPERATION_FILE_WRITE_TEXT,
                    TYPED_IO_INVALID_RESOURCE_TOKEN,
                    b"text",
                    1,
                ),
                request(TYPED_IO_OPERATION_SOCKET_CONNECT, 0, b"localhost", 80),
                no_argument_request(
                    TYPED_IO_OPERATION_SOCKET_READ_TEXT,
                    TYPED_IO_INVALID_RESOURCE_TOKEN,
                    1,
                ),
            ];
            for malformed in &malformed {
                assert!(create_typed_io_task(executor, malformed).is_null());
            }
            assert_eq!(executor_live_tasks(executor), 0);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_rejects_noncanonical_root_shapes() {
        let (runtime, executor) = runtime_and_executor();
        let request = no_argument_request(
            TYPED_IO_OPERATION_FILE_READ_TEXT,
            TYPED_IO_INVALID_RESOURCE_TOKEN,
            0,
        );
        unsafe {
            let no_roots = typed_io_descriptor(&[], &[], 1, 0);
            assert_typed_io_descriptor_rejected(executor, &no_roots, &request);

            let roots = [
                (offset_of!(TestIoFrame, result) + offset_of!(TestIoResult, text)) as u64,
                offset_of!(TestIoFrame, scratch_text) as u64,
            ];
            let canonical_bitmaps = [2_u64, 1_u64];
            let canonical = typed_io_descriptor(&roots, &canonical_bitmaps, 2, 1);

            let no_completed_root = [2_u64, 0_u64];
            assert_typed_io_descriptor_rejected(
                executor,
                &LoomTypedCoroutineDescriptor {
                    live_bitmaps: no_completed_root.as_ptr(),
                    ..canonical
                },
                &request,
            );

            let extra_completed_root = [2_u64, 3_u64];
            assert_typed_io_descriptor_rejected(
                executor,
                &LoomTypedCoroutineDescriptor {
                    live_bitmaps: extra_completed_root.as_ptr(),
                    ..canonical
                },
                &request,
            );

            let scratch_as_completed_root = [2_u64, 2_u64];
            assert_typed_io_descriptor_rejected(
                executor,
                &LoomTypedCoroutineDescriptor {
                    live_bitmaps: scratch_as_completed_root.as_ptr(),
                    ..canonical
                },
                &request,
            );

            let extra_state = [2_u64, 1_u64, 0_u64];
            assert_typed_io_descriptor_rejected(
                executor,
                &LoomTypedCoroutineDescriptor {
                    root_state_count: 3,
                    live_bitmaps: extra_state.as_ptr(),
                    ..canonical
                },
                &request,
            );

            let extra_roots = [0_u64, roots[0], roots[1]];
            let extra_root_bitmaps = [4_u64, 2_u64];
            assert_typed_io_descriptor_rejected(
                executor,
                &LoomTypedCoroutineDescriptor {
                    root_slot_count: extra_roots.len() as u64,
                    root_offsets: extra_roots.as_ptr(),
                    live_bitmaps: extra_root_bitmaps.as_ptr(),
                    ..canonical
                },
                &request,
            );

            let wrong_completed_state = [1_u64, 2_u64];
            assert_typed_io_descriptor_rejected(
                executor,
                &LoomTypedCoroutineDescriptor {
                    live_bitmaps: wrong_completed_state.as_ptr(),
                    completed_root_state: 0,
                    ..canonical
                },
                &request,
            );

            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_accepts_no_completed_result_roots() {
        let (runtime, executor) = runtime_and_executor();
        let request = no_argument_request(
            TYPED_IO_OPERATION_FILE_READ_TEXT,
            TYPED_IO_INVALID_RESOURCE_TOKEN,
            0,
        );
        let roots = [offset_of!(TestIoFrame, scratch_text) as u64];
        // Fault-mode Task[T] needs the running scratch Text root, while a
        // direct File/Socket/Unit result may contain no managed pointer cells.
        let live_bitmaps = [1_u64, 0_u64];
        let descriptor = typed_io_descriptor(&roots, &live_bitmaps, 2, 1);
        unsafe {
            let task = typed_io_task_create_v1(
                executor,
                ptr::from_ref(&descriptor),
                ptr::from_ref(&request),
            );
            assert!(!task.is_null());
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_factory_accepts_multiple_completed_result_roots() {
        let (runtime, executor) = runtime_and_executor();
        let request = no_argument_request(
            TYPED_IO_OPERATION_FILE_READ_TEXT,
            TYPED_IO_INVALID_RESOURCE_TOKEN,
            0,
        );
        let roots = [
            (offset_of!(TestIoFrame, result)
                + offset_of!(TestIoResult, outcome)
                + offset_of!(LoomTypedIoOutcome, payload)) as u64,
            (offset_of!(TestIoFrame, result) + offset_of!(TestIoResult, text)) as u64,
            offset_of!(TestIoFrame, scratch_text) as u64,
        ];
        // Result[Text, IoError] may use distinct managed cells for its success
        // Text and error message. The running state keeps only scratch Text;
        // the completed state keeps both cells inside the exact result.
        let live_bitmaps = [4_u64, 3_u64];
        let descriptor = typed_io_descriptor(&roots, &live_bitmaps, 2, 1);
        unsafe {
            let task = typed_io_task_create_v1(
                executor,
                ptr::from_ref(&descriptor),
                ptr::from_ref(&request),
            );
            assert!(!task.is_null());
            assert_eq!(typed_task_request_cancel_v1(executor, task), TYPED_TASK_OK);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            destroy(runtime, executor);
        }
    }

    #[test]
    fn typed_io_poll_rejects_an_outcome_overlapping_the_task_frame() {
        let (runtime, executor) = runtime_and_executor();
        let request = no_argument_request(
            TYPED_IO_OPERATION_FILE_READ_TEXT,
            TYPED_IO_INVALID_RESOURCE_TOKEN,
            0,
        );
        unsafe {
            let task = create_typed_io_task(executor, &request);
            assert!(!task.is_null());
            (*executor).runnable.clear();
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
            let frame = (*task)
                .typed
                .as_ref()
                .unwrap()
                .frame_pointer()
                .cast::<TestIoFrame>();
            (*frame).result.outcome.kind = u32::MAX;
            crate::gc::enter_executor(executor);
            assert_eq!(
                typed_io_poll_v1(
                    task.cast(),
                    executor.cast(),
                    &raw mut (*frame).scratch_text,
                    &raw mut (*frame).result.outcome,
                ),
                TASK_FAULTED
            );
            crate::gc::leave_executor();
            assert_eq!((*frame).result.outcome.kind, u32::MAX);
            assert!((*frame).scratch_text.is_null());
            (*executor).active_task = ptr::null_mut();
            destroy(runtime, executor);
        }
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

    unsafe extern "C" fn typed_close_socket_on_cancel_fixture(
        _task: *mut c_void,
        executor: *mut c_void,
        frame: *mut c_void,
    ) -> i32 {
        let status = unsafe {
            typed_resource_close_v1(executor.cast(), TYPED_RESOURCE_KIND_SOCKET, frame.cast())
        };
        if status == TYPED_RESOURCE_CLOSE_OK {
            TASK_CANCELLED
        } else {
            TASK_FAULTED
        }
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

    unsafe fn activate_test_task(executor: *mut LoomExecutor, task: *mut LoomTask) {
        let index = unsafe {
            (*executor)
                .runnable
                .iter()
                .position(|candidate| *candidate == task)
                .expect("test fixture must be runnable")
        };
        assert_eq!(unsafe { (*executor).runnable.remove(index) }, Some(task));
        unsafe {
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
        }
    }

    unsafe fn finish_resource_owner(executor: *mut LoomExecutor, owner: *mut LoomTask) {
        assert_eq!(unsafe { (*executor).active_task }, owner);
        let children = unsafe { std::mem::take(&mut (*owner).owned_children) };
        for child in children {
            assert_eq!(unsafe { (*child).owner }, owner);
            unsafe { (*child).owner = ptr::null_mut() };
        }
        unsafe {
            (*executor).active_task = ptr::null_mut();
            complete_terminal(&mut *executor, owner, TASK_CANCELLED);
        }
    }

    unsafe fn with_active_resource<T>(
        executor: *mut LoomExecutor,
        resource: OwnedResource,
        action: impl FnOnce(*mut LoomTask, i64) -> T,
    ) -> T {
        assert!(unsafe { (*executor).active_task.is_null() });
        let owner = unsafe { task_spawn(executor, Some(complete_fixture), 1, 0) };
        assert!(!owner.is_null());
        unsafe { activate_test_task(executor, owner) };
        let token = resource.token();
        unsafe { (*owner).owned_result_resources.push(resource) };
        enter_executor(executor);
        let result = action(owner, token);
        leave_executor();
        unsafe { finish_resource_owner(executor, owner) };
        result
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
        let handle = resource.token();
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
        let handle = resource.token();
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
    fn tracked_resource_selection_rejects_duplicate_tokens_across_kinds() {
        assert_eq!(select_tracked_resource([]), Ok(None));
        assert_eq!(
            select_tracked_resource([(0, 0, false)]),
            Err(CloseResourceError::InvalidOwnership)
        );
        assert_eq!(
            select_tracked_resource([(0, 0, false), (1, 2, true)]),
            Err(CloseResourceError::InvalidOwnership)
        );
        assert_eq!(
            select_tracked_resource([(0, 0, true), (1, 2, true)]),
            Err(CloseResourceError::InvalidOwnership)
        );
    }

    #[test]
    fn typed_resource_close_rejects_invalid_or_inactive_boundaries() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let mut handle = INVALID_RESOURCE_TOKEN;
        unsafe {
            assert_eq!(
                typed_resource_close_v1(ptr::null_mut(), TYPED_RESOURCE_KIND_FILE, &raw mut handle,),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_FILE, ptr::null_mut()),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_resource_close_v1(executor, 0, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_FILE, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    pub(super) fn socket_pair() -> io::Result<(TcpStream, TcpStream)> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let client = TcpStream::connect(address)?;
        let (server, _) = listener.accept()?;
        Ok((client, server))
    }

    #[test]
    fn blocking_socket_connect_tries_every_resolved_address() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("bind fallback connection address");
        let live = listener.local_addr().expect("read fallback address");
        let refused = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));

        match blocking_socket_connect_addresses([refused, live]) {
            BlockingResult::Resource { kind, resource } => {
                assert_eq!(kind, IoResourceKind::Socket);
                assert!(!resource.is_file());
            }
            _ => panic!("later resolved address was not attempted"),
        }
        match blocking_socket_connect_addresses([]) {
            BlockingResult::Fault {
                class,
                kind,
                message,
            } => {
                assert_eq!(class, IoFaultClass::SocketResolve);
                assert_eq!(kind, 9);
                assert_eq!(message, "host resolved to no addresses");
            }
            _ => panic!("empty address set did not report resolution failure"),
        }
        match blocking_socket_connect_addresses([refused]) {
            BlockingResult::Fault { class, message, .. } => {
                assert_eq!(class, IoFaultClass::Operation);
                assert!(!message.is_empty());
            }
            _ => panic!("failed address did not report connection failure"),
        }
    }

    #[test]
    fn typed_resource_close_rejects_an_untracked_socket() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let resource = OwnedResource::from(socket);
        let mut handle = resource.token();
        let original = handle;

        unsafe {
            let owner = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!owner.is_null());
            activate_test_task(executor, owner);
            enter_executor(executor);
            assert_eq!(
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            assert_eq!(handle, original);
            assert!((*owner).owned_result_resources.is_empty());
            assert_peer_still_connected(&mut peer);
            leave_executor();
            finish_resource_owner(executor, owner);
            drop(resource);
            assert_peer_closed(&mut peer);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_rejects_a_live_raw_socket_handle_as_a_token() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create raw-handle socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let mut raw_handle = crate::platform::socket_handle_bits(&socket);
        let original = raw_handle;

        unsafe {
            let owner = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!owner.is_null());
            activate_test_task(executor, owner);
            enter_executor(executor);
            assert_eq!(
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut raw_handle,),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            leave_executor();
            assert_eq!(raw_handle, original);
            assert!((*owner).owned_result_resources.is_empty());
            assert_peer_still_connected(&mut peer);

            finish_resource_owner(executor, owner);
            drop(socket);
            assert_peer_closed(&mut peer);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_rejects_a_sibling_token() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create sibling socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let sibling = task_spawn(executor, Some(complete_fixture), 1, 0);
            let owner = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!sibling.is_null() && !owner.is_null());
            let resource = OwnedResource::from(socket);
            let mut token = resource.token();
            (*sibling).owned_result_resources.push(resource);
            activate_test_task(executor, owner);

            enter_executor(executor);
            assert_eq!(
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut token),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            leave_executor();
            assert_eq!((*sibling).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            finish_resource_owner(executor, owner);
            complete_terminal(&mut *executor, sibling, TASK_CANCELLED);
            assert_peer_closed(&mut peer);
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_rejects_a_stale_token() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create stale socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let owner = task_spawn(executor, Some(complete_fixture), 1, 0);
            assert!(!owner.is_null());
            activate_test_task(executor, owner);
            let resource = OwnedResource::from(socket);
            let mut token = resource.token();
            let original = token;
            (*owner).owned_result_resources.push(resource);
            (*owner).owned_result_resources.clear();
            assert_peer_closed(&mut peer);

            enter_executor(executor);
            assert_eq!(
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut token),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            leave_executor();
            assert_eq!(token, original);
            assert!((*owner).owned_result_resources.is_empty());

            finish_resource_owner(executor, owner);
            executor_destroy(executor);
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
        let mut handle = INVALID_RESOURCE_TOKEN;

        unsafe {
            with_active_resource(executor, socket.into(), |task, token| {
                handle = token;
                assert_eq!((*task).owned_result_resources.len(), 1);
                assert_eq!(
                    typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle,),
                    TYPED_RESOURCE_CLOSE_OK
                );
                assert_eq!(handle, INVALID_RESOURCE_TOKEN);
                assert!((*task).owned_result_resources.is_empty());
                assert_peer_closed(&mut peer);
                assert_eq!(
                    typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle,),
                    TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
                );
            });
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn typed_resource_close_is_authorized_only_inside_cancellation_cleanup() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = socket_pair().expect("create cancellation cleanup socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        unsafe {
            let mut descriptor = typed_resource_descriptor();
            descriptor.cancel = Some(typed_close_socket_on_cancel_fixture);
            let task = typed_task_create_v1(executor, &raw const descriptor);
            assert!(!task.is_null());
            assert_eq!(typed_task_initialize_v1(task, 0), TYPED_TASK_OK);

            let resource = OwnedResource::from(socket);
            let token = resource.token();
            (*task)
                .typed
                .as_mut()
                .expect("typed cancellation fixture")
                .frame_pointer()
                .cast::<i64>()
                .write(token);
            (*task).owned_result_resources.push(resource);
            assert_eq!(typed_task_publish_v1(executor, task), TYPED_TASK_OK);

            activate_test_task(executor, task);
            (*task).cancel_requested = true;
            enter_executor(executor);
            assert_eq!(
                typed_resource_close_v1(
                    executor,
                    TYPED_RESOURCE_KIND_SOCKET,
                    (*task)
                        .typed
                        .as_mut()
                        .expect("typed cancellation fixture")
                        .frame_pointer()
                        .cast(),
                ),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            leave_executor();
            assert_eq!((*task).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            (*executor).active_task = ptr::null_mut();
            (*task).status = TaskStatus::Runnable;
            (*task).queued = true;
            (*executor).runnable.push_front(task);
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            assert!((*task).owned_result_resources.is_empty());
            assert_peer_closed(&mut peer);

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
        let mut handle = INVALID_RESOURCE_TOKEN;

        unsafe {
            with_active_resource(executor, socket.into(), |task, token| {
                handle = token;
                assert_eq!(
                    typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_FILE, &raw mut handle),
                    TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
                );
                assert_eq!(handle, token);
                assert_eq!((*task).owned_result_resources.len(), 1);
                assert_peer_still_connected(&mut peer);

                assert_eq!(
                    typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle,),
                    TYPED_RESOURCE_CLOSE_OK
                );
                assert_eq!(handle, INVALID_RESOURCE_TOKEN);
                assert!((*task).owned_result_resources.is_empty());
                assert_peer_closed(&mut peer);
            });
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
            let mut handle = INVALID_RESOURCE_TOKEN;

            assert_eq!(
                typed_task_take_result_v1(
                    root,
                    (&raw mut handle).cast(),
                    size_of::<u32>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
                TYPED_RESOURCE_CLOSE_INVALID_ARGUMENT
            );
            leave_executor();
            assert_eq!(handle, expected);
            assert_eq!((*root).owned_result_resources.len(), 1);
            assert_peer_still_connected(&mut peer);

            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
            assert_peer_closed(&mut peer);
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
            let mut handle = INVALID_RESOURCE_TOKEN;
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
            activate_test_task(executor, parent);
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

            let mut handle = INVALID_RESOURCE_TOKEN;
            assert_eq!(
                typed_task_take_result_v1(
                    sibling,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
            activate_test_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            let child = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            let expected = complete_typed_resource(executor, child, socket.into());

            let mut handle = INVALID_RESOURCE_TOKEN;
            assert_eq!(
                typed_task_take_result_v1(
                    child,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
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
            activate_test_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            let left = create_typed_resource_task(executor);
            let right = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, left), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, right), WAIT_OK);
            let expected_left = complete_typed_resource(executor, left, left_socket.into());
            let expected_right = complete_typed_resource(executor, right, right_socket.into());

            assert_eq!(task_suspend_join(executor, parent), 0);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);
            let mut left_handle = INVALID_RESOURCE_TOKEN;
            let mut right_handle = INVALID_RESOURCE_TOKEN;
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
                    typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, handle),
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
            activate_test_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ALL), WAIT_OK);
            let child = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, child), WAIT_OK);
            complete_typed_resource(executor, child, socket.into());
            assert_eq!(task_suspend_join(executor, parent), 0);
            assert_eq!(task_join_step(parent), TASK_COMPLETED);

            let mut handle = INVALID_RESOURCE_TOKEN;
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
            activate_test_task(executor, parent);
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
            let mut handle = INVALID_RESOURCE_TOKEN;
            assert_eq!(
                typed_task_take_result_v1(
                    completed,
                    (&raw mut handle).cast(),
                    size_of::<i64>() as u64,
                    align_of::<i64>() as u64,
                ),
                TYPED_TASK_INVALID_ARGUMENT
            );
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
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
            activate_test_task(executor, parent);
            assert_eq!(task_prepare_join(executor, parent, TASK_JOIN_ANY), WAIT_OK);
            let winner = create_typed_resource_task(executor);
            let loser = create_typed_resource_task(executor);
            assert_eq!(task_add_join_child(executor, parent, winner), WAIT_OK);
            assert_eq!(task_add_join_child(executor, parent, loser), WAIT_OK);
            let expected = complete_typed_resource(executor, winner, winner_socket.into());
            complete_typed_resource(executor, loser, loser_socket.into());

            assert_eq!(task_suspend_join(executor, parent), 0);
            let mut handle = INVALID_RESOURCE_TOKEN;
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
            assert_eq!(handle, INVALID_RESOURCE_TOKEN);
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
                typed_resource_close_v1(executor, TYPED_RESOURCE_KIND_SOCKET, &raw mut handle),
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
            activate_test_task(executor, parent);
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

    pub(super) fn assert_peer_closed(peer: &mut TcpStream) {
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

    #[test]
    fn cancelled_queued_blocking_work_is_not_invoked() {
        struct DropMarker(Arc<AtomicBool>);

        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let state = AtomicU8::new(BLOCKING_QUEUED);
        let invoked = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let submission: Mutex<Option<BlockingJob>> = Mutex::new(Some(Box::new({
            let invoked = Arc::clone(&invoked);
            let marker = DropMarker(Arc::clone(&released));
            move || {
                drop(marker);
                invoked.store(true, Ordering::SeqCst);
            }
        })));
        assert_eq!(
            cancel_blocking_work(&state, &submission),
            BlockingCancellation::Queued
        );
        assert!(submission.lock().unwrap().is_none());
        assert!(released.load(Ordering::Acquire));
        assert!(!run_blocking_submission(&state, &submission));
        assert!(!invoked.load(Ordering::SeqCst));
    }

    #[test]
    fn queued_blocking_task_cancellation_skips_the_host_operation() {
        let _serial = lock_slow_blocking_fixture();
        reset_controlled_blocking_fixture();
        let blockers_started = Arc::new(AtomicUsize::new(0));
        let release_blockers = Arc::new(AtomicBool::new(false));
        for _ in 0..4 {
            let blockers_started = Arc::clone(&blockers_started);
            let release_blockers = Arc::clone(&release_blockers);
            blocking_pool()
                .send(Box::new(move || {
                    blockers_started.fetch_add(1, Ordering::Release);
                    while !release_blockers.load(Ordering::Acquire) {
                        std::thread::yield_now();
                    }
                }))
                .unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while blockers_started.load(Ordering::Acquire) != 4 && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        assert_eq!(blockers_started.load(Ordering::Acquire), 4);
        let (socket, mut peer) = super::resource_ownership_tests::socket_pair()
            .expect("create queued cancellation socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");

        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        unsafe {
            let task = task_spawn(executor, Some(resume_controlled_blocking_fixture), 1, 0);
            assert!(!task.is_null());
            (*executor).runnable.clear();
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
            enter_executor(executor);
            assert_eq!(
                suspend_blocking(task, executor, move || {
                    CONTROLLED_BLOCKING_STARTED.store(true, Ordering::Release);
                    drop(socket);
                    CONTROLLED_BLOCKING_FINISHED.store(true, Ordering::Release);
                    BlockingResult::Unit
                }),
                TASK_PENDING
            );
            leave_executor();
            (*executor).active_task = ptr::null_mut();
            assert!((*task).status == TaskStatus::Waiting);
            assert_eq!(task_cancel(executor, task), WAIT_OK);
            super::resource_ownership_tests::assert_peer_closed(&mut peer);

            let timed_out = Arc::new(AtomicBool::new(false));
            let watchdog_timed_out = Arc::clone(&timed_out);
            let watchdog_release = Arc::clone(&release_blockers);
            let (finished_sender, finished_receiver) = mpsc::channel();
            let watchdog = std::thread::spawn(move || {
                if finished_receiver
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .is_err()
                {
                    watchdog_timed_out.store(true, Ordering::Release);
                    watchdog_release.store(true, Ordering::Release);
                }
            });
            assert_eq!(executor_run(executor, task), TASK_CANCELLED);
            let _ = finished_sender.send(());
            release_blockers.store(true, Ordering::Release);
            watchdog.join().unwrap();
            assert!(
                !timed_out.load(Ordering::Acquire),
                "queued cancellation waited for unrelated blocking workers"
            );
            assert!(!CONTROLLED_BLOCKING_STARTED.load(Ordering::Acquire));
            assert!(!CONTROLLED_BLOCKING_FINISHED.load(Ordering::Acquire));
            executor_destroy(executor);
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

    #[test]
    fn executor_shutdown_drains_started_blocking_work() {
        let _serial = lock_slow_blocking_fixture();
        reset_controlled_blocking_fixture();
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());

        unsafe {
            let task = task_spawn(executor, Some(resume_controlled_blocking_fixture), 1, 0);
            assert!(!task.is_null());
            (*executor).runnable.clear();
            (*task).queued = false;
            (*task).status = TaskStatus::Running;
            (*executor).active_task = task;
            enter_executor(executor);
            assert_eq!(
                resume_controlled_blocking_fixture(task, executor),
                TASK_PENDING
            );
            leave_executor();
            (*executor).active_task = ptr::null_mut();
            assert!((*task).status == TaskStatus::Waiting);

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !CONTROLLED_BLOCKING_STARTED.load(Ordering::Acquire)
                && std::time::Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            assert!(CONTROLLED_BLOCKING_STARTED.load(Ordering::Acquire));
            assert!(!CONTROLLED_BLOCKING_FINISHED.load(Ordering::Acquire));

            let releaser = std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(50));
                CONTROLLED_BLOCKING_RELEASED.store(true, Ordering::Release);
            });
            let shutdown_started = std::time::Instant::now();
            executor_destroy(executor);
            let shutdown_elapsed = shutdown_started.elapsed();
            releaser.join().unwrap();
            assert!(shutdown_elapsed >= std::time::Duration::from_millis(25));
            assert!(CONTROLLED_BLOCKING_FINISHED.load(Ordering::Acquire));
            assert_eq!(runtime_destroy_v1(runtime), WAIT_OK);
        }
    }

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

    unsafe extern "C" fn universal_complete(
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
    fn universal_composite_consumes_an_already_completed_child_inline() {
        let (runtime, executor) = runtime_and_executor();
        unsafe {
            let owner = task_spawn(executor, Some(universal_complete), 1, 0);
            activate_test_task(executor, owner);
            let child = task_spawn(executor, Some(universal_complete), 1, 0);
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

    const CLEANUP_UNIVERSAL_SPAWN_DENIED: usize = 1 << 0;
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

    unsafe extern "C" fn universal_pending(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_PENDING
    }

    unsafe fn exercise_cleanup_guards(
        task: *mut LoomTask,
        executor: *mut LoomExecutor,
        frame: *mut c_void,
    ) -> usize {
        let mut passed = 0;
        if unsafe { task_spawn(executor, Some(universal_pending), 1, 0) }.is_null() {
            passed |= CLEANUP_UNIVERSAL_SPAWN_DENIED;
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
            // universal consumers, but it cannot mint another authorization.
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
    fn immediately_runnable_child_cancellation_uses_reverse_creation_order() {
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
