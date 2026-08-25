use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex, OnceLock, mpsc};

use loom_runtime_abi::{
    FAULT_FORMAT_ENV, FAULT_FORMAT_JSON, FAULT_JSON_PREFIX, VALUE_SLOT_WORDS, VALUE_TAG_ENUM,
    VALUE_TAG_LIST, VALUE_TAG_RECORD, VALUE_TAG_TASK, VALUE_TAG_TUPLE,
};

use crate::gc::{active_runtime_pointer, collect, enter_executor, leave_executor};
use crate::reactor::{
    LoomExecutor, LoomReadyNotification, LoomRegistration, LoomWaitSource, cancel_for_task,
    has_registrations, pop_for_scheduler, register_for_task, wait_for_scheduler,
};
use crate::{
    COROUTINE_ABI_VERSION, TASK_CANCELLED, TASK_COMPLETED, TASK_FAULTED, TASK_JOIN_ALL,
    TASK_JOIN_ANY, TASK_JOIN_RACE, TASK_JOIN_SETTLED, TASK_PENDING, WAIT_ABI_VERSION,
    WAIT_INVALID_ARGUMENT, WAIT_NO_MEMORY, WAIT_OK, WAIT_SOURCE_FD, WAIT_SOURCE_TIMER,
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

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoomCoroutineDescriptor {
    pub abi_version: u32,
    pub flags: u32,
    pub resume: Option<LoomTaskResume>,
    pub cancel: Option<LoomTaskCancel>,
    pub trace: Option<LoomTaskTrace>,
    pub slot_count: u64,
    pub result_slot: u64,
    pub state_count: u64,
    pub live_bitmap_words: u64,
    pub live_bitmaps: *const u64,
}

#[repr(C)]
struct RuntimeWitnessNode {
    value: *mut c_void,
    next: *mut RuntimeWitnessNode,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TaskStatus {
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
        descriptor: OwnedFd,
    },
    FileWrite {
        descriptor: OwnedFd,
        bytes: Vec<u8>,
    },
    SocketConnect {
        host: String,
        port: u16,
    },
    SocketRead {
        descriptor: OwnedFd,
        bytes: Vec<u8>,
    },
    SocketWrite {
        descriptor: OwnedFd,
        bytes: Vec<u8>,
        offset: usize,
    },
}

pub(crate) enum BlockingResult {
    Resource {
        nominal: u64,
        descriptor: OwnedFd,
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

fn duplicate_descriptor(descriptor: i32) -> io::Result<OwnedFd> {
    // SAFETY: the scoped File/Socket value owns this live descriptor while its
    // method task is created. The worker receives an independent duplicate.
    unsafe { BorrowedFd::borrow_raw(descriptor) }.try_clone_to_owned()
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
    owned_result_resources: Vec<OwnedFd>,
    primary_fault_recorded: bool,
    fault_code: String,
    fault_message: String,
    fault_detail: String,
    witness_values: Vec<Box<[usize]>>,
    witness_nodes: Vec<Box<RuntimeWitnessNode>>,
    witness_clones: BTreeMap<usize, usize>,
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
        TaskStatus::Faulted | TaskStatus::Cancelled => return false,
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
    if task.is_null() {
        return;
    }
    if let Some(trace) = unsafe { (*task).descriptor.trace } {
        unsafe { trace(task, visitor, context) };
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
    if terminal(unsafe { (*task).status }) {
        return;
    }
    unsafe { (*task).cancel_requested = true };
    let registrations = unsafe { std::mem::take(&mut (*task).waits) };
    for registration in registrations {
        // SAFETY: each registration was created by this executor for task.
        let _ = unsafe { cancel_for_task(executor, &raw const registration) };
    }
    let children = unsafe { (*task).owned_children.clone() };
    for child in children {
        // SAFETY: structured child pointers remain live until executor drop.
        unsafe { request_cancel(executor, child) };
    }
    if unsafe { (*task).status } != TaskStatus::Running {
        unsafe {
            (*task).status = TaskStatus::Runnable;
            enqueue_task(executor, task);
        }
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
                unsafe { (*parent).join_step = terminal_step(failure) };
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
            let deferred = unsafe { (*owner).deferred_terminal };
            unsafe {
                (*owner).deferred_terminal = TASK_PENDING;
                complete_terminal(executor, owner, deferred);
            }
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
    if step != TASK_COMPLETED {
        // A cancelled or faulted I/O task cannot publish a resource result.
        // Drop both a readiness operation's private descriptor and any worker
        // result that raced with cancellation before making the task terminal.
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
        if pointer == executor.active_task || task.slots.is_empty() {
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
        IoOperation::FileRead { descriptor } => unsafe {
            suspend_blocking(task, executor, move || blocking_file_read(descriptor))
        },
        IoOperation::FileWrite { descriptor, bytes } => unsafe {
            suspend_blocking(task, executor, move || {
                blocking_file_write(descriptor, &bytes)
            })
        },
        IoOperation::SocketConnect { host, port } => unsafe {
            suspend_blocking(task, executor, move || blocking_socket_connect(&host, port))
        },
        IoOperation::SocketRead { descriptor, bytes } => unsafe {
            resume_socket_read(task, executor, descriptor, bytes)
        },
        IoOperation::SocketWrite {
            descriptor,
            bytes,
            offset,
        } => unsafe { resume_socket_write(task, executor, descriptor, bytes, offset) },
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
            descriptor: file.into(),
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

fn blocking_file_read(descriptor: OwnedFd) -> BlockingResult {
    let mut file = File::from(descriptor);
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

fn blocking_file_write(descriptor: OwnedFd, bytes: &[u8]) -> BlockingResult {
    let mut file = File::from(descriptor);
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
                descriptor: socket.into(),
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
        BlockingResult::Resource {
            nominal,
            descriptor,
        } => unsafe { store_resource_result(task, nominal, descriptor) },
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
    descriptor: OwnedFd,
    mut bytes: Vec<u8>,
) -> i32 {
    let raw_descriptor = descriptor.as_raw_fd();
    // SAFETY: the Socket value owns the descriptor; this temporary only borrows it.
    let mut socket = ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(raw_descriptor) });
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
                    IoOperation::SocketRead { descriptor, bytes },
                    raw_descriptor,
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
    descriptor: OwnedFd,
    bytes: Vec<u8>,
    mut offset: usize,
) -> i32 {
    if offset == bytes.len() {
        return unsafe { store_unit_result(task) };
    }
    let raw_descriptor = descriptor.as_raw_fd();
    // SAFETY: the Socket value owns the descriptor; this temporary only borrows it.
    let mut socket = ManuallyDrop::new(unsafe { TcpStream::from_raw_fd(raw_descriptor) });
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
                        descriptor,
                        bytes,
                        offset,
                    },
                    raw_descriptor,
                    crate::WAIT_WRITABLE,
                );
            },
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => unsafe { return fail_io(task, "SocketWriteFault", &error) },
        }
    }
}

unsafe fn store_resource_result(task: *mut LoomTask, nominal: u64, descriptor: OwnedFd) -> i32 {
    let node = crate::gc::allocate_value_node().cast::<ValueNode>();
    if node.is_null() {
        return unsafe { fail_message(task, "OutOfMemory", "resource result allocation failed") };
    }
    let mut raw = ValueSlot::default();
    raw.words[0] = 2;
    raw.words[3] = i64::from(descriptor.as_raw_fd()).cast_unsigned();
    unsafe {
        (*node).value = raw;
        (*node).next = ptr::null_mut();
    }
    let mut result = ValueSlot::default();
    result.words[0] = VALUE_TAG_RECORD;
    result.words[1] = nominal;
    result.words[2] = 1;
    result.words[4] = node as u64;
    unsafe { (*task).owned_result_resources.push(descriptor) };
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
        let payload = retain_result_node(unsafe { &mut *(*task).executor }, value, ptr::null_mut());
        let mut result = ValueSlot::default();
        result.words[0] = VALUE_TAG_ENUM;
        result.words[1] = RESULT_TYPE;
        result.words[2] = 0;
        result.words[3] = 1;
        result.words[4] = payload as u64;
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
    descriptor: i32,
    interests: u32,
) -> i32 {
    let source = LoomWaitSource {
        abi_version: WAIT_ABI_VERSION,
        kind: WAIT_SOURCE_FD,
        handle: i64::from(descriptor),
        interests,
        reserved: 0,
        deadline_ns: 0,
    };
    unsafe { (*task).io_operation = Some(operation) };
    if unsafe { task_suspend_wait(executor, task, &raw const source) } == WAIT_OK {
        TASK_PENDING
    } else {
        unsafe {
            fail_message(
                task,
                "IoWaitFault",
                "could not register descriptor readiness",
            )
        }
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
    let message_node =
        retain_result_node(unsafe { &mut *(*task).executor }, message, ptr::null_mut());
    let kind_node = retain_result_node(unsafe { &mut *(*task).executor }, kind_value, message_node);
    let mut error = ValueSlot::default();
    error.words[0] = VALUE_TAG_RECORD;
    error.words[1] = IO_ERROR_TYPE;
    error.words[2] = 2;
    error.words[4] = kind_node as u64;
    let error_node = retain_result_node(unsafe { &mut *(*task).executor }, error, ptr::null_mut());
    let mut result = ValueSlot::default();
    result.words[0] = VALUE_TAG_ENUM;
    result.words[1] = RESULT_TYPE;
    result.words[2] = 1;
    result.words[3] = 1;
    result.words[4] = error_node as u64;
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
    let Some(mut descriptor) = (unsafe { descriptor.as_ref() }).copied() else {
        return ptr::null_mut();
    };
    let (Ok(slot_count), Ok(result_slot), Some(_)) = (
        usize::try_from(descriptor.slot_count),
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
    if executor.is_null()
        || descriptor.abi_version != COROUTINE_ABI_VERSION
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
        witness_values: Vec::new(),
        witness_nodes: Vec::new(),
        witness_clones: BTreeMap::new(),
    });
    let pointer = &raw mut *task;
    executor_ref.tasks.push(task);
    if !owner.is_null() {
        unsafe { (*owner).owned_children.push(pointer) };
    }
    unsafe { enqueue_task(executor_ref, pointer) };
    pointer
}

unsafe fn clone_witness_tree(
    task: &mut LoomTask,
    source: *const c_void,
    field_count: usize,
) -> *mut c_void {
    if source.is_null() {
        return ptr::null_mut();
    }
    if let Some(cloned) = task.witness_clones.get(&(source as usize)).copied() {
        return cloned as *mut c_void;
    }
    let fields = unsafe { slice::from_raw_parts(source.cast::<usize>(), field_count) };
    let mut cloned = fields.to_vec().into_boxed_slice();
    cloned[0] = 0;
    let cloned_pointer = cloned.as_mut_ptr().cast::<c_void>();
    task.witness_values.push(cloned);
    task.witness_clones
        .insert(source as usize, cloned_pointer as usize);
    let arguments =
        unsafe { clone_witness_nodes(task, fields[0] as *const RuntimeWitnessNode, field_count) };
    unsafe { *cloned_pointer.cast::<usize>() = arguments as usize };
    cloned_pointer
}

unsafe fn clone_witness_nodes(
    task: &mut LoomTask,
    source: *const RuntimeWitnessNode,
    field_count: usize,
) -> *mut RuntimeWitnessNode {
    if source.is_null() {
        return ptr::null_mut();
    }
    let value = unsafe { clone_witness_tree(task, (*source).value, field_count) };
    let next = unsafe { clone_witness_nodes(task, (*source).next, field_count) };
    let mut node = Box::new(RuntimeWitnessNode { value, next });
    let pointer = &raw mut *node;
    task.witness_nodes.push(node);
    pointer
}

/// Deep-clones a compiler witness and its prerequisite proof tree into task
/// lifetime storage. Applied witnesses may originate in a caller stack frame,
/// while the coroutine can outlive that frame after its first suspension.
#[unsafe(export_name = "loom_task_clone_witness")]
pub unsafe extern "C" fn task_clone_witness(
    task: *mut LoomTask,
    source: *const c_void,
    field_count: u64,
) -> *mut c_void {
    const MAX_WITNESS_FIELDS: usize = 1 << 20;
    let Ok(field_count) = usize::try_from(field_count) else {
        return ptr::null_mut();
    };
    if task.is_null() || source.is_null() || field_count == 0 || field_count > MAX_WITNESS_FIELDS {
        return ptr::null_mut();
    }
    unsafe { clone_witness_tree(&mut *task, source, field_count) }
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

fn checked_fd(descriptor: i64) -> Option<i32> {
    i32::try_from(descriptor).ok().filter(|value| *value >= 0)
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
pub unsafe extern "C" fn file_read_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
) -> *mut LoomTask {
    let Some(descriptor) = checked_fd(descriptor) else {
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
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
    unsafe { spawn_io_task(executor, IoOperation::FileRead { descriptor }) }
}

#[unsafe(export_name = "loom_file_try_read_text")]
pub unsafe extern "C" fn file_try_read_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
) -> *mut LoomTask {
    let Some(descriptor) = checked_fd(descriptor) else {
        return unsafe {
            spawn_io_error_task(executor, 8, "FileReadFault", "file resource is closed")
        };
    };
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
        Err(error) => unsafe {
            return spawn_io_error_task(
                executor,
                io_error_kind(&error),
                "FileReadFault",
                error.to_string(),
            );
        },
    };
    unsafe { spawn_try_io_task(executor, IoOperation::FileRead { descriptor }) }
}

#[unsafe(export_name = "loom_file_write_text")]
pub unsafe extern "C" fn file_write_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let (Some(descriptor), Some(text)) =
        (checked_fd(descriptor), unsafe { copy_text(data, length) })
    else {
        return ptr::null_mut();
    };
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
                descriptor,
                bytes: text.into_bytes(),
            },
        )
    }
}

#[unsafe(export_name = "loom_file_try_write_text")]
pub unsafe extern "C" fn file_try_write_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let Some(descriptor) = checked_fd(descriptor) else {
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
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
                descriptor,
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
    descriptor: i64,
) -> *mut LoomTask {
    let Some(descriptor) = checked_fd(descriptor) else {
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
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
                descriptor,
                bytes: Vec::new(),
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_try_read_text")]
pub unsafe extern "C" fn socket_try_read_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
) -> *mut LoomTask {
    let Some(descriptor) = checked_fd(descriptor) else {
        return unsafe {
            spawn_io_error_task(executor, 8, "SocketReadFault", "socket resource is closed")
        };
    };
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
                descriptor,
                bytes: Vec::new(),
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_write_text")]
pub unsafe extern "C" fn socket_write_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let (Some(descriptor), Some(text)) =
        (checked_fd(descriptor), unsafe { copy_text(data, length) })
    else {
        return ptr::null_mut();
    };
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
                descriptor,
                bytes: text.into_bytes(),
                offset: 0,
            },
        )
    }
}

#[unsafe(export_name = "loom_socket_try_write_text")]
pub unsafe extern "C" fn socket_try_write_text(
    executor: *mut LoomExecutor,
    descriptor: i64,
    data: *const u8,
    length: u64,
) -> *mut LoomTask {
    let Some(descriptor) = checked_fd(descriptor) else {
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
    let descriptor = match duplicate_descriptor(descriptor) {
        Ok(descriptor) => descriptor,
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
                descriptor,
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
        || !matches!(unsafe { (*value).words[1] }, FILE_TYPE | SOCKET_TYPE)
        || unsafe { (*value).words[2] } != 1
    {
        return WAIT_INVALID_ARGUMENT;
    }
    let node = unsafe { (*value).words[4] as *mut ValueNode };
    if node.is_null() || unsafe { (*node).value.words[0] } != 2 {
        return WAIT_INVALID_ARGUMENT;
    }
    let descriptor = unsafe { (*node).value.words[3].cast_signed() };
    if descriptor < 0 {
        return WAIT_OK;
    }
    let Some(descriptor) = checked_fd(descriptor) else {
        return WAIT_INVALID_ARGUMENT;
    };
    let executor = unsafe { &mut *executor };
    let mut owned = None;
    for task in &mut executor.tasks {
        if let Some(index) = task
            .owned_result_resources
            .iter()
            .position(|candidate| candidate.as_raw_fd() == descriptor)
        {
            owned = Some(task.owned_result_resources.swap_remove(index));
            break;
        }
    }
    if let Some(owned) = owned {
        drop(owned);
    } else {
        // SAFETY: a well-formed externally transferred File/Socket value owns
        // its raw descriptor when it is no longer tracked by a runtime task.
        drop(unsafe { OwnedFd::from_raw_fd(descriptor) });
    }
    unsafe { (*node).value.words[3] = (-1_i64).cast_unsigned() };
    WAIT_OK
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
        || !matches!(copied.kind, WAIT_SOURCE_TIMER | WAIT_SOURCE_FD)
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

    let (Some(code), Some(message), Some(display), Some(detail)) = (
        unsafe { copy_text(arguments.code, arguments.code_length) },
        unsafe { copy_text(arguments.message, arguments.message_length) },
        unsafe { copy_text(arguments.display, arguments.display_length) },
        unsafe { copy_text(arguments.detail, arguments.detail_length) },
    ) else {
        return WAIT_INVALID_ARGUMENT;
    };
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
    let active_task = match resolve_fault_context(context) {
        Some(FaultContextTarget::Root) => ptr::null_mut(),
        Some(FaultContextTarget::Executor(executor)) => {
            // SAFETY: resolution matched the candidate against the active
            // runtime's live attachment before converting it to an executor.
            unsafe { (*executor).active_task }
        }
        None => return WAIT_INVALID_ARGUMENT,
    };
    unsafe { raise_fault_for_task_or_root(active_task, arguments) }
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
    if !executor_owns(executor_ref, parent)
        || mode > TASK_JOIN_RACE
        || executor_ref.active_task != parent
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
    if !executor_owns(executor_ref, parent)
        || !executor_owns(executor_ref, child)
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
    if !executor_owns(executor_ref, parent)
        || executor_ref.active_task != parent
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
    i32::from(unsafe { (*parent).status } != TaskStatus::Runnable)
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

#[unsafe(export_name = "loom_task_join_step")]
pub unsafe extern "C" fn task_join_step(parent: *const LoomTask) -> i32 {
    if parent.is_null() {
        TASK_FAULTED
    } else {
        unsafe { (*parent).join_step }
    }
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
    let executor = unsafe { &mut *(*parent).executor };
    let mut value = ValueSlot::default();
    value.words[0] = VALUE_TAG_ENUM;
    value.words[1] = TASK_OUTCOME_TYPE;
    match step {
        TASK_COMPLETED => {
            let result = unsafe { task_join_result(parent, index as u64).cast::<ValueSlot>() };
            if result.is_null() {
                return WAIT_INVALID_ARGUMENT;
            }
            value.words[2] = TASK_OUTCOME_COMPLETED;
            value.words[3] = 1;
            value.words[4] =
                retain_result_node(executor, unsafe { *result }, ptr::null_mut()) as u64;
        }
        TASK_CANCELLED => {
            value.words[2] = TASK_OUTCOME_CANCELLED;
        }
        _ => {
            let child = unsafe { &*(&(*parent).join_children)[index] };
            let message_bytes = if child.fault_message.is_empty() {
                TASK_FAULT_MESSAGE.as_bytes()
            } else {
                child.fault_message.as_bytes()
            };
            let code_bytes = if child.fault_code.is_empty() {
                TASK_FAULT_CODE.as_bytes()
            } else {
                child.fault_code.as_bytes()
            };
            let message = text_value(message_bytes);
            let message_node = retain_result_node(executor, message, ptr::null_mut());
            let code = text_value(code_bytes);
            let code_node = retain_result_node(executor, code, message_node);
            let mut fault = ValueSlot::default();
            fault.words[0] = VALUE_TAG_RECORD;
            fault.words[1] = TASK_FAULT_TYPE;
            fault.words[2] = 2;
            fault.words[4] = code_node as u64;
            let fault_node = retain_result_node(executor, fault, ptr::null_mut());
            value.words[2] = TASK_OUTCOME_FAULTED;
            value.words[3] = 1;
            value.words[4] = fault_node as u64;
        }
    }
    unsafe { destination.write(value) };
    WAIT_OK
}

fn text_value(bytes: &[u8]) -> ValueSlot {
    crate::gc::text_value(bytes).unwrap_or_else(|| std::process::abort())
}

fn retain_result_node(
    _executor: &mut LoomExecutor,
    value: ValueSlot,
    next: *mut ValueNode,
) -> *mut ValueNode {
    let node = crate::gc::allocate_value_node().cast::<ValueNode>();
    if node.is_null() {
        std::process::abort();
    }
    unsafe {
        (*node).value = value;
        (*node).next = next;
    }
    node
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
    let resources = unsafe { std::mem::take(&mut (*child).owned_result_resources) };
    unsafe { (*parent).owned_result_resources.extend(resources) };
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
    if parent.is_null() || destination.is_null() || shape > JOIN_RESULT_OUTCOME_LIST {
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

    let mut head = ptr::null_mut();
    for index in (0..count).rev() {
        let node = crate::gc::allocate_value_node().cast::<ValueNode>();
        if node.is_null() {
            return WAIT_NO_MEMORY;
        }
        unsafe {
            (*node).value = ValueSlot::default();
            (*node).next = head;
        }
        let status = if outcome {
            unsafe { write_outcome(parent, index, &raw mut (*node).value) }
        } else {
            let result = unsafe { task_join_result(parent, index as u64).cast::<ValueSlot>() };
            if result.is_null() {
                WAIT_INVALID_ARGUMENT
            } else {
                unsafe { (*node).value = *result };
                WAIT_OK
            }
        };
        if status != WAIT_OK {
            return status;
        }
        head = node;
    }
    let mut result = ValueSlot::default();
    result.words[0] = if matches!(shape, JOIN_RESULT_LIST | JOIN_RESULT_OUTCOME_LIST) {
        VALUE_TAG_LIST
    } else {
        VALUE_TAG_TUPLE
    };
    result.words[2] = count as u64;
    result.words[4] = head as u64;
    unsafe { destination.write(result) };
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
    let valid =
        executor_owns(unsafe { &*executor }, task) && unsafe { (*executor).active_task } == task;
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
    if executor.is_null() || !executor_owns(unsafe { &*executor }, task) {
        return WAIT_INVALID_ARGUMENT;
    }
    unsafe { request_cancel(&mut *executor, task) };
    WAIT_OK
}

#[unsafe(export_name = "loom_executor_run")]
pub unsafe extern "C" fn executor_run(executor: *mut LoomExecutor, root: *mut LoomTask) -> i32 {
    if executor.is_null()
        || !executor_owns(unsafe { &*executor }, root)
        || !unsafe { (*root).owner.is_null() }
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
            collect(executor_ref);
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
    use std::io::{self, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::reactor::{executor_create_for_runtime_v1, executor_destroy};
    use crate::runtime::{runtime_create_v1, runtime_destroy_v1};

    unsafe extern "C" fn complete_fixture(
        _task: *mut LoomTask,
        _executor: *mut LoomExecutor,
    ) -> i32 {
        TASK_COMPLETED
    }

    fn assert_peer_still_connected(peer: &mut UnixStream) {
        let mut byte = [0_u8; 1];
        let result = peer.read(&mut byte);
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    }

    fn assert_peer_closed(peer: &mut UnixStream) {
        let mut byte = [0_u8; 1];
        assert!(matches!(peer.read(&mut byte), Ok(0)));
    }

    #[test]
    fn pending_io_task_owns_a_duplicate_and_cancellation_closes_it() {
        let runtime = runtime_create_v1();
        assert!(!runtime.is_null());
        let executor = unsafe { executor_create_for_runtime_v1(runtime) };
        assert!(!executor.is_null());
        let (socket, mut peer) = UnixStream::pair().expect("create socket pair");
        peer.set_nonblocking(true).expect("make peer nonblocking");
        let text = b"not written";

        unsafe {
            let task = socket_write_text(
                executor,
                i64::from(socket.as_raw_fd()),
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
        let (winner_socket, mut winner_peer) = UnixStream::pair().expect("create winner pair");
        let (loser_socket, mut loser_peer) = UnixStream::pair().expect("create loser pair");
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
        let (left_socket, mut left_peer) = UnixStream::pair().expect("create left pair");
        let (right_socket, mut right_peer) = UnixStream::pair().expect("create right pair");
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
            assert_eq!(
                task_write_join_result(parent, destination.cast(), JOIN_RESULT_TUPLE),
                WAIT_OK
            );
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
}
