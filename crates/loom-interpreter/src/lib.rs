//! Deterministic interpreter for checked loom MIR.

/// Interpreter backend version included in persistent compiler cache keys.
pub const BACKEND_VERSION: &str = env!("CARGO_PKG_VERSION");

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use loom_core::Span;
use loom_core::runtime_fault::{
    ARTIFACT_PROOF_REJECTED_FAULT_CODE, ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE,
    INTEGER_OVERFLOW_FAULT_CODE, INTEGER_OVERFLOW_FAULT_MESSAGE, INVALID_DURATION_FAULT_CODE,
    INVALID_DURATION_FAULT_MESSAGE, INVALID_SLEEP_DURATION_FAULT_CODE,
    INVALID_SLEEP_DURATION_FAULT_MESSAGE, LOG_WRITE_FAULT_CODE, LOG_WRITE_FAULT_MESSAGE,
    SLEEP_DURATION_OVERFLOW_FAULT_CODE, SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
    STDOUT_WRITE_FAULT_CODE, STDOUT_WRITE_FAULT_MESSAGE,
};
use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, CheckedProgram, Constant, ConstructionMode,
    Contract, ContractArm, ContractExpr, ContractExprKind, ContractValue, Expr, ExprKind, Function,
    FunctionId, LocalId, MatchArm, Pattern, Place, Program, Receiver, RequirementId,
    ScopedDisposal, Statement, StatementKind, TaskJoinMode, Type, TypeDefKind, TypeId, UnaryOp,
    VariantId, WitnessId, WitnessRef, disclosure_type_summary,
};
use serde::{Deserialize, Serialize};

const DEFAULT_FUEL: u64 = 1_000_000;
const DEFAULT_MAX_DEPTH: u32 = 256;
const SOCKET_IO_BUDGET: usize = 64 * 1024;
const SOCKET_REACTOR_SLICE: Duration = Duration::from_millis(10);

type HostIoJob = Box<dyn FnOnce() + Send + 'static>;

fn host_io_pool() -> &'static mpsc::SyncSender<HostIoJob> {
    static POOL: OnceLock<mpsc::SyncSender<HostIoJob>> = OnceLock::new();
    POOL.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel::<HostIoJob>(256);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..4 {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("loom-interpreter-io-{index}"))
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
                .expect("create bounded interpreter I/O worker");
        }
        sender
    })
}

#[cfg(test)]
mod task_all_cancellation_tests {
    use super::*;

    fn task_id(value: &Value) -> u64 {
        let Value::Task { id } = value else {
            panic!("fixture must create a Task")
        };
        *id
    }

    #[test]
    fn cancelled_all_child_propagates_cancellation_instead_of_shortening_the_tuple() {
        let program = Program::default()
            .into_checked()
            .expect("empty checked-MIR fixture");
        let mut interpreter = Interpreter::new(&program);
        let span = Span::default();
        let first = task_id(
            &interpreter
                .spawn_terminal_task(Ok(Value::Int { value: 1 }), span)
                .expect("spawn first child"),
        );
        let cancelled = task_id(
            &interpreter
                .spawn_terminal_task(Ok(Value::Int { value: 2 }), span)
                .expect("spawn cancelled child"),
        );
        interpreter
            .tasks
            .get_mut(&cancelled)
            .expect("cancelled child exists")
            .status = TaskStatus::Cancelled;
        let parent = task_id(
            &interpreter
                .spawn_terminal_task(Ok(Value::Unit), span)
                .expect("spawn parent"),
        );
        {
            let parent_task = interpreter.tasks.get_mut(&parent).expect("parent exists");
            parent_task.status = TaskStatus::Runnable;
            parent_task.awaiting_state = Some(1);
            parent_task.children = vec![first, cancelled];
            parent_task.join_mode = TaskJoinMode::All;
            parent_task.join_combined = true;
        }
        interpreter.active_root = Some(parent);
        interpreter.active_task = Some(parent);
        let awaited = Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Await {
                state: 1,
                task: Box::new(Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(Constant::Unit),
                    ty: Type::Unit,
                    span,
                }),
            },
            ty: Type::Tuple(vec![Type::Int, Type::Int]),
            span,
        };

        assert!(matches!(
            interpreter
                .poll_await(parent, u64::MAX, &awaited)
                .expect("poll all join"),
            AwaitPoll::Cancelled
        ));
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExecutionLimits {
    pub fuel: u64,
    pub max_call_depth: u32,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            fuel: DEFAULT_FUEL,
            max_call_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Value {
    Unit,
    Tuple {
        elements: Vec<Value>,
    },
    List {
        elements: Vec<Value>,
    },
    Bool {
        value: bool,
    },
    Int {
        value: i64,
    },
    Float {
        value: f64,
    },
    Text {
        value: String,
    },
    /// Immutable UTF-8-independent byte storage. Unlike `Text`, indexing is
    /// defined in bytes and the payload need not be valid UTF-8.
    Bytes {
        value: Vec<u8>,
    },
    Record {
        ty: TypeId,
        fields: Vec<Value>,
    },
    Enum {
        ty: TypeId,
        variant: VariantId,
        payload: Vec<Value>,
    },
    Refined {
        ty: TypeId,
        value: Box<Value>,
    },
    ConstraintError {
        value: ConstraintError,
    },
    DynView {
        value: Box<Value>,
        writeback: Option<Location>,
        witness: RuntimeWitness,
        mutable: bool,
        token: u32,
    },
    /// Compiler-internal managed task handle. Checked source cannot observe,
    /// store, compare, or return this value.
    Task {
        id: u64,
    },
    TaskJoin {
        mode: TaskJoinMode,
        tasks: Vec<u64>,
        dynamic: bool,
    },
    TaskOutcome {
        outcome: TaskOutcomeValue,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum TaskOutcomeValue {
    Completed(Box<Value>),
    Faulted,
    Cancelled,
}

impl Clone for Value {
    fn clone(&self) -> Self {
        clone_value(self, false)
    }
}

/// A fully resolved conformance proof carried by an executing frame or view.
///
/// MIR witness parameters have already been replaced by their caller proofs;
/// `arguments` therefore contains exactly the prerequisite proofs required by
/// a conditional conformance implementation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWitness {
    definition: WitnessId,
    arguments: Vec<RuntimeWitness>,
}

impl Value {
    #[must_use]
    pub fn summary(&self) -> String {
        let mut output = String::new();
        self.write_summary(&mut output, 0);
        if output.chars().count() > 256 {
            output.chars().take(253).collect::<String>() + "..."
        } else {
            output
        }
    }

    fn write_summary(&self, output: &mut String, _depth: u8) {
        match self {
            Self::Unit => output.push_str("Unit"),
            Self::Tuple { .. } => output.push_str("Tuple"),
            Self::List { .. } => output.push_str("List"),
            Self::Bool { .. } => output.push_str("Bool"),
            Self::Int { .. } => output.push_str("Int"),
            Self::Float { .. } => output.push_str("Float"),
            Self::Text { .. } => output.push_str("Text"),
            Self::Bytes { .. } => output.push_str("Bytes"),
            Self::Record { ty, .. } | Self::Enum { ty, .. } | Self::Refined { ty, .. } => {
                let _ = write!(output, "type#{}", ty.0);
            }
            Self::ConstraintError { .. } => output.push_str("ConstraintError"),
            Self::DynView { .. } => output.push_str("dyn"),
            Self::Task { .. } | Self::TaskJoin { .. } => output.push_str("Task"),
            Self::TaskOutcome { .. } => output.push_str("TaskOutcome"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConstraintError {
    pub target_type: String,
    pub code: String,
    pub predicate: String,
    pub path: Vec<String>,
    pub value_summary: String,
    pub contract_span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractFaultKind {
    Precondition,
    Postcondition,
    Invariant,
    Assertion,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractFault {
    pub code: String,
    pub category: ContractFaultKind,
    pub message: String,
    pub contract_span: Span,
    pub blame_span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeFault {
    pub code: String,
    pub message: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InterpreterDefect {
    pub code: String,
    pub message: String,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum ExecutionFailure {
    Contract { fault: ContractFault },
    Runtime { fault: RuntimeFault },
    Defect { defect: InterpreterDefect },
}

impl From<ContractFault> for ExecutionFailure {
    fn from(fault: ContractFault) -> Self {
        Self::Contract { fault }
    }
}

impl From<RuntimeFault> for ExecutionFailure {
    fn from(fault: RuntimeFault) -> Self {
        if fault.code.starts_with("LOOM_RUNTIME_") {
            Self::Defect {
                defect: InterpreterDefect {
                    code: "InterpreterDefect".into(),
                    message: fault.message,
                    span: fault.span,
                },
            }
        } else {
            Self::Runtime { fault }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub name: String,
    pub status: TestStatus,
    pub value: Option<Value>,
    pub failure: Option<ExecutionFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Location {
    frame: u64,
    local: LocalId,
    projection: Vec<u32>,
}

#[derive(Clone, Debug)]
enum Slot {
    Empty,
    Value(Value),
    Alias(Location),
    Moved,
}

#[derive(Clone, Debug)]
struct Frame {
    slots: Vec<Slot>,
    witnesses: Vec<RuntimeWitness>,
}

#[derive(Clone, Debug)]
enum TaskStatus {
    Runnable,
    Waiting,
    Completed(Value),
    Failed(ExecutionFailure),
    Cancelled,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
struct ManagedTask {
    function: FunctionId,
    frame: u64,
    parent: Option<u64>,
    children: Vec<u64>,
    cursor: usize,
    awaiting_state: Option<u32>,
    cleanups: Vec<RuntimeCleanup>,
    status: TaskStatus,
    queued: bool,
    marked: bool,
    /// Present only for the compiler-known `Task.sleep` leaf task.
    timer_deadline: Option<Instant>,
    host_io: bool,
    contract_state: Option<AsyncContractState>,
    join_mode: TaskJoinMode,
    join_dynamic: bool,
    join_combined: bool,
    join_winner: Option<usize>,
    cancel_requested: bool,
}

#[derive(Clone, Debug)]
enum RuntimeCleanup {
    Deferred(Block),
    Scoped {
        local: LocalId,
        disposal: ScopedDisposal,
        span: Span,
    },
}

#[derive(Clone, Debug)]
struct AsyncContractState {
    call_site: Span,
    entered: bool,
    old_receiver: Option<Value>,
    old_arguments: Vec<Option<Value>>,
}

enum HostIoValue {
    Value(Value),
    File(std::fs::File),
    Socket(TcpStream),
}

struct HostIoCompletion {
    task: u64,
    span: Span,
    outcome: HostIoCompletionOutcome,
}

enum HostIoCompletionOutcome {
    Infallible(Result<HostIoValue, ExecutionFailure>),
    Fallible(Result<HostIoValue, HostIoError>),
}

struct HostIoError {
    kind: u32,
    message: String,
}

enum SocketIoOperation {
    Read { bytes: Vec<u8> },
    Write { bytes: Vec<u8>, offset: usize },
}

struct PendingSocketIo {
    socket: TcpStream,
    registration: loom_runtime::WaitToken,
    operation: SocketIoOperation,
    fallible: bool,
    span: Span,
}

enum SocketIoPoll {
    Pending,
    Completed(Value),
    Failed(std::io::Error),
}

impl SocketIoOperation {
    const fn interests(&self) -> u32 {
        match self {
            Self::Read { .. } => loom_runtime::WAIT_READABLE,
            Self::Write { .. } => loom_runtime::WAIT_WRITABLE,
        }
    }

    const fn fault_code(&self) -> &'static str {
        match self {
            Self::Read { .. } => "SocketReadFault",
            Self::Write { .. } => "SocketWriteFault",
        }
    }
}

enum JsonConversionFailure {
    Invalid(ExecutionFailure),
    DepthLimit,
}

impl From<ExecutionFailure> for JsonConversionFailure {
    fn from(value: ExecutionFailure) -> Self {
        Self::Invalid(value)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GcStats {
    pub allocations: u64,
    pub collections: u64,
    pub reclaimed: u64,
    pub live: u64,
}

enum TaskPoll {
    Pending,
    Completed,
    Failed,
}

enum AwaitPoll {
    Pending,
    Ready(Value),
    Failed(ExecutionFailure),
    Control(EvalAbort),
    Cancelled,
}

enum AwaitDestination {
    Ignore,
    Local(LocalId),
    Scoped(LocalId, ScopedDisposal),
    Tuple(Vec<LocalId>),
    Return,
}

#[derive(Clone, Debug)]
enum BoundArgument {
    Value(Value),
    Alias(Location),
}

enum EvalAbort {
    Failure(ExecutionFailure),
    Return(Box<Value>),
    Break,
    Continue,
    Cancelled,
}

struct SyncCall {
    function: FunctionId,
    arguments: Vec<BoundArgument>,
    witnesses: Vec<RuntimeWitness>,
    span: Span,
}

type SyncResume<'program, T> = Box<
    dyn FnOnce(&mut Interpreter<'program>, Result<Value, ExecutionFailure>) -> SyncStep<'program, T>
        + 'program,
>;

enum SyncStep<'program, T> {
    Complete(Result<T, EvalAbort>),
    Call {
        request: SyncCall,
        resume: SyncResume<'program, T>,
    },
}

impl<'program, T: 'program> SyncStep<'program, T> {
    fn complete(value: T) -> Self {
        Self::Complete(Ok(value))
    }

    fn fail(abort: impl Into<EvalAbort>) -> Self {
        Self::Complete(Err(abort.into()))
    }

    fn then<U: 'program>(
        self,
        interpreter: &mut Interpreter<'program>,
        continuation: impl FnOnce(
            &mut Interpreter<'program>,
            Result<T, EvalAbort>,
        ) -> SyncStep<'program, U>
        + 'program,
    ) -> SyncStep<'program, U> {
        match self {
            Self::Complete(outcome) => continuation(interpreter, outcome),
            Self::Call { request, resume } => SyncStep::Call {
                request,
                resume: Box::new(move |interpreter, outcome| {
                    resume(interpreter, outcome).then(interpreter, continuation)
                }),
            },
        }
    }

    fn and_then<U: 'program>(
        self,
        interpreter: &mut Interpreter<'program>,
        continuation: impl FnOnce(&mut Interpreter<'program>, T) -> SyncStep<'program, U> + 'program,
    ) -> SyncStep<'program, U> {
        self.then(interpreter, move |interpreter, outcome| match outcome {
            Ok(value) => continuation(interpreter, value),
            Err(abort) => SyncStep::Complete(Err(abort)),
        })
    }
}

impl SyncStep<'_, Value> {
    fn call(request: SyncCall) -> Self {
        Self::Call {
            request,
            resume: Box::new(|_, outcome| match outcome {
                Ok(value) => Self::complete(value),
                Err(failure) => Self::fail(failure),
            }),
        }
    }
}

enum BoundInvocation<'program> {
    Complete(Value),
    Sync {
        frame: u64,
        function: &'program Function,
        call_site: Span,
    },
}

enum SyncCleanup<'program> {
    Deferred(&'program Block),
    Scoped {
        local: LocalId,
        disposal: &'program ScopedDisposal,
        span: Span,
    },
}

impl From<ExecutionFailure> for EvalAbort {
    fn from(value: ExecutionFailure) -> Self {
        Self::Failure(value)
    }
}

impl From<RuntimeFault> for EvalAbort {
    fn from(value: RuntimeFault) -> Self {
        Self::Failure(value.into())
    }
}

impl From<ContractFault> for EvalAbort {
    fn from(value: ContractFault) -> Self {
        Self::Failure(value.into())
    }
}

struct ContractContext<'a> {
    receiver: Option<&'a Value>,
    result: Option<&'a Value>,
    arguments: &'a [Value],
    old_receiver: Option<&'a Value>,
    old_arguments: &'a [Option<Value>],
    bindings: &'a [Value],
}

#[derive(Clone, Debug, Default)]
struct OldSnapshotNeeds {
    receiver: bool,
    arguments: Vec<bool>,
}

pub struct Interpreter<'program> {
    program: &'program Program,
    frames: BTreeMap<u64, Frame>,
    next_frame: u64,
    fuel_limit: u64,
    fuel: u64,
    max_call_depth: u32,
    call_depth: u32,
    tasks: BTreeMap<u64, ManagedTask>,
    ready: VecDeque<u64>,
    next_task: u64,
    active_task: Option<u64>,
    active_root: Option<u64>,
    gc_stats: GcStats,
    process_arguments: Vec<String>,
    files: BTreeMap<u64, std::fs::File>,
    sockets: BTreeMap<u64, TcpStream>,
    next_resource: u64,
    host_io_sender: mpsc::Sender<HostIoCompletion>,
    host_io_receiver: mpsc::Receiver<HostIoCompletion>,
    socket_reactor: Option<loom_runtime::WaitSet>,
    socket_io: BTreeMap<u64, PendingSocketIo>,
}

impl<'program> Interpreter<'program> {
    #[must_use]
    pub fn new(program: &'program CheckedProgram) -> Self {
        Self::with_limits(program, ExecutionLimits::default())
    }

    #[must_use]
    pub fn with_limits(program: &'program CheckedProgram, limits: ExecutionLimits) -> Self {
        let (host_io_sender, host_io_receiver) = mpsc::channel();
        Self {
            program: program.as_program(),
            frames: BTreeMap::new(),
            next_frame: 0,
            fuel_limit: limits.fuel,
            fuel: limits.fuel,
            max_call_depth: limits.max_call_depth,
            call_depth: 0,
            tasks: BTreeMap::new(),
            ready: VecDeque::new(),
            next_task: 0,
            active_task: None,
            active_root: None,
            gc_stats: GcStats::default(),
            process_arguments: Vec::new(),
            files: BTreeMap::new(),
            sockets: BTreeMap::new(),
            next_resource: 0,
            host_io_sender,
            host_io_receiver,
            socket_reactor: loom_runtime::WaitSet::new().ok(),
            socket_io: BTreeMap::new(),
        }
    }

    fn reset_socket_executor(&mut self) {
        self.socket_io.clear();
        self.socket_reactor = loom_runtime::WaitSet::new().ok();
    }

    /// Supplies arguments visible through `std.process.arguments`.
    /// The executable path is deliberately excluded.
    #[must_use]
    pub fn with_process_arguments(mut self, arguments: Vec<String>) -> Self {
        self.process_arguments = arguments;
        self
    }

    /// Invokes a checked MIR function.
    ///
    /// # Errors
    ///
    /// Returns a structured contract or runtime failure. Language-level
    /// `Result.Err` remains a successful [`Value`] on the normal channel.
    pub fn invoke(
        &mut self,
        function: FunctionId,
        arguments: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, ExecutionFailure> {
        if self.call_depth == 0 {
            let (host_io_sender, host_io_receiver) = mpsc::channel();
            self.host_io_sender = host_io_sender;
            self.host_io_receiver = host_io_receiver;
            self.reset_socket_executor();
            self.frames.clear();
            self.tasks.clear();
            self.ready.clear();
            self.next_frame = 0;
            self.next_task = 0;
            self.active_task = None;
            self.active_root = None;
            self.gc_stats = GcStats::default();
            self.files.clear();
            self.sockets.clear();
            self.next_resource = 0;
            self.fuel = self.fuel_limit;
        }
        let value = self.invoke_bound(
            function,
            arguments.into_iter().map(BoundArgument::Value).collect(),
            Vec::new(),
            call_site,
        )?;
        if let Value::Task { id } = value {
            self.run_task(id)
        } else {
            Ok(value)
        }
    }

    #[must_use]
    pub const fn gc_stats(&self) -> GcStats {
        self.gc_stats
    }

    pub fn run_tests(&mut self) -> Vec<TestResult> {
        self.program
            .tests
            .clone()
            .into_iter()
            .map(|id| {
                let Some(function) = self.program.function(id) else {
                    return TestResult {
                        name: format!("function#{}", id.0),
                        status: TestStatus::Failed,
                        value: None,
                        failure: Some(
                            self.runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "test function does not exist",
                                Span::default(),
                            )
                            .into(),
                        ),
                    };
                };
                let name = function.name.clone();
                let span = function.span;
                match self.invoke(id, Vec::new(), span) {
                    Ok(value) if test_value_passed(&value) => TestResult {
                        name,
                        status: TestStatus::Passed,
                        value: Some(value),
                        failure: None,
                    },
                    Ok(value) => TestResult {
                        name,
                        status: TestStatus::Failed,
                        value: Some(value),
                        failure: None,
                    },
                    Err(failure) => TestResult {
                        name,
                        status: TestStatus::Failed,
                        value: None,
                        failure: Some(failure),
                    },
                }
            })
            .collect()
    }

    fn run_task(&mut self, root: u64) -> Result<Value, ExecutionFailure> {
        self.active_root = Some(root);
        loop {
            self.drain_host_io_completions();
            match self.tasks.get(&root).map(|task| task.status.clone()) {
                Some(TaskStatus::Completed(value)) => {
                    self.active_task = None;
                    self.active_root = None;
                    self.collect_tasks(None);
                    return Ok(value);
                }
                Some(TaskStatus::Failed(failure)) => {
                    self.active_task = None;
                    self.active_root = None;
                    self.collect_tasks(None);
                    return Err(failure);
                }
                Some(TaskStatus::Cancelled) => {
                    self.active_task = None;
                    self.active_root = None;
                    self.collect_tasks(None);
                    return Err(self
                        .runtime_fault("TaskCancelled", "root task was cancelled", Span::default())
                        .into());
                }
                Some(TaskStatus::Runnable | TaskStatus::Waiting) => {}
                None => {
                    return Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "root task was collected before completion",
                            Span::default(),
                        )
                        .into());
                }
            }

            let Some(current) = self.ready.pop_front() else {
                if self.wait_for_work() {
                    continue;
                }
                return Err(self
                    .runtime_fault(
                        "AsyncDeadlock",
                        "no runnable task can satisfy the root task's wait",
                        Span::default(),
                    )
                    .into());
            };
            let Some(task) = self.tasks.get_mut(&current) else {
                continue;
            };
            task.queued = false;
            if !matches!(task.status, TaskStatus::Runnable) {
                continue;
            }

            self.active_task = Some(current);
            let poll = self.resume_task(current)?;
            self.active_task = None;
            if matches!(poll, TaskPoll::Completed | TaskPoll::Failed) {
                self.wake_parent(current);
            }
        }
    }

    fn wait_for_work(&mut self) -> bool {
        if self.drain_host_io_completions()
            || self.poll_socket_io(Some(Duration::ZERO))
            || !self.ready.is_empty()
        {
            return true;
        }
        let deadline = self
            .tasks
            .values()
            .filter(|task| {
                task.timer_deadline.is_some() && matches!(task.status, TaskStatus::Waiting)
            })
            .filter_map(|task| task.timer_deadline)
            .min();
        let host_io_pending = self
            .tasks
            .values()
            .any(|task| task.host_io && matches!(task.status, TaskStatus::Waiting));
        let socket_io_pending = !self.socket_io.is_empty();
        if deadline.is_none() && !host_io_pending && !socket_io_pending {
            return false;
        }

        if host_io_pending && socket_io_pending {
            let until_deadline = deadline.map_or(SOCKET_REACTOR_SLICE, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            let timeout = until_deadline.min(SOCKET_REACTOR_SLICE);
            if let Ok(completion) = self.host_io_receiver.recv_timeout(timeout) {
                self.finish_host_io_completion(completion);
                self.drain_host_io_completions();
            }
            self.poll_socket_io(Some(Duration::ZERO));
        } else if host_io_pending {
            let completion = deadline.map_or_else(
                || self.host_io_receiver.recv().ok(),
                |deadline| {
                    self.host_io_receiver
                        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                        .ok()
                },
            );
            if let Some(completion) = completion {
                self.finish_host_io_completion(completion);
                self.drain_host_io_completions();
            }
        } else if socket_io_pending {
            let timeout =
                deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            self.poll_socket_io(timeout);
        } else if let Some(deadline) = deadline {
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        }

        let now = Instant::now();
        let ready = self
            .tasks
            .iter()
            .filter_map(|(id, task)| {
                (matches!(task.status, TaskStatus::Waiting)
                    && task.timer_deadline.is_some_and(|deadline| deadline <= now))
                .then_some(*id)
            })
            .collect::<Vec<_>>();
        for task_id in ready {
            self.tasks
                .get_mut(&task_id)
                .expect("timer task exists")
                .status = TaskStatus::Runnable;
            self.enqueue_task(task_id);
        }
        true
    }

    fn drain_host_io_completions(&mut self) -> bool {
        let mut completed = false;
        while let Ok(completion) = self.host_io_receiver.try_recv() {
            completed |= self.finish_host_io_completion(completion);
        }
        completed
    }

    fn poll_socket_io(&mut self, timeout: Option<Duration>) -> bool {
        if self.socket_io.is_empty() {
            return false;
        }
        if self.socket_reactor.is_none() {
            self.fail_all_socket_io(&std::io::Error::other(
                "interpreter socket reactor allocation failed",
            ));
            return true;
        }
        let notifications = match self
            .socket_reactor
            .as_mut()
            .expect("socket reactor was checked above")
            .wait(timeout)
        {
            Ok(notifications) => notifications,
            Err(error) => {
                self.fail_all_socket_io(&error);
                return true;
            }
        };
        let mut progressed = false;
        for notification in notifications {
            progressed |= self.finish_socket_notification(notification);
        }
        progressed
    }

    fn finish_socket_notification(&mut self, notification: loom_runtime::WaitEvent) -> bool {
        let Some(task_id) = self.socket_io.iter().find_map(|(task_id, pending)| {
            (pending.registration == notification.token).then_some(*task_id)
        }) else {
            return false;
        };
        let Some(mut pending) = self.socket_io.remove(&task_id) else {
            return false;
        };
        let accepts_completion = self.tasks.get(&task_id).is_some_and(|task| {
            !task.cancel_requested && matches!(task.status, TaskStatus::Waiting)
        });
        if !accepts_completion {
            return false;
        }
        let poll = if notification.os_error == 0 {
            poll_socket_operation(&mut pending)
        } else {
            SocketIoPoll::Failed(std::io::Error::from_raw_os_error(notification.os_error))
        };
        match poll {
            SocketIoPoll::Pending => {
                let interests = pending.operation.interests();
                match self.register_socket_wait(&pending.socket, interests) {
                    Ok(registration) => {
                        pending.registration = registration;
                        self.socket_io.insert(task_id, pending);
                        false
                    }
                    Err(error) => {
                        self.finish_socket_task(task_id, pending, Err(error));
                        true
                    }
                }
            }
            SocketIoPoll::Completed(value) => {
                self.finish_socket_task(task_id, pending, Ok(value));
                true
            }
            SocketIoPoll::Failed(error) => {
                self.finish_socket_task(task_id, pending, Err(error));
                true
            }
        }
    }

    fn finish_socket_task(
        &mut self,
        task_id: u64,
        pending: PendingSocketIo,
        result: Result<Value, std::io::Error>,
    ) {
        let PendingSocketIo {
            operation,
            fallible,
            span,
            ..
        } = pending;
        let outcome = match (fallible, result) {
            (false, Ok(value)) => Ok(value),
            (false, Err(error)) => Err(io_failure(operation.fault_code(), &error, span)),
            (true, Ok(value)) => self.result_value(true, value, span),
            (true, Err(error)) => self.io_error_result(host_io_error(error), span),
        };
        let status = match outcome {
            Ok(value) => TaskStatus::Completed(value),
            Err(failure) => TaskStatus::Failed(failure),
        };
        if let Some(task) = self.tasks.get_mut(&task_id) {
            task.status = status;
        }
        self.wake_parent(task_id);
    }

    fn fail_all_socket_io(&mut self, error: &std::io::Error) {
        let message = error.to_string();
        let pending = std::mem::take(&mut self.socket_io);
        for (task_id, operation) in pending {
            self.finish_socket_task(
                task_id,
                operation,
                Err(std::io::Error::other(message.clone())),
            );
        }
    }

    fn register_socket_wait(
        &mut self,
        socket: &TcpStream,
        interests: u32,
    ) -> Result<loom_runtime::WaitToken, std::io::Error> {
        self.socket_reactor
            .as_mut()
            .ok_or_else(|| std::io::Error::other("interpreter socket reactor allocation failed"))?
            .register_source(socket, interests)
    }

    fn cancel_socket_io(&mut self, task_id: u64) {
        let Some(pending) = self.socket_io.remove(&task_id) else {
            return;
        };
        if let Some(reactor) = &mut self.socket_reactor {
            // A stale result is benign when readiness raced with cancellation.
            let _ = reactor.cancel(pending.registration);
        }
    }

    fn finish_host_io_completion(&mut self, completion: HostIoCompletion) -> bool {
        let accepts_completion = self.tasks.get(&completion.task).is_some_and(|task| {
            task.host_io && !task.cancel_requested && matches!(task.status, TaskStatus::Waiting)
        });
        if !accepts_completion {
            return false;
        }
        let outcome = match completion.outcome {
            HostIoCompletionOutcome::Infallible(outcome) => {
                outcome.and_then(|value| self.finish_host_io_value(value, completion.span))
            }
            HostIoCompletionOutcome::Fallible(outcome) => match outcome {
                Ok(value) => self
                    .finish_host_io_value(value, completion.span)
                    .and_then(|value| self.result_value(true, value, completion.span)),
                Err(error) => self.io_error_result(error, completion.span),
            },
        };
        let status = match outcome {
            Ok(value) => TaskStatus::Completed(value),
            Err(failure) => TaskStatus::Failed(failure),
        };
        self.tasks
            .get_mut(&completion.task)
            .expect("host I/O task was checked above")
            .status = status;
        self.wake_parent(completion.task);
        true
    }

    fn finish_host_io_value(
        &mut self,
        value: HostIoValue,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match value {
            HostIoValue::Value(value) => Ok(value),
            HostIoValue::File(file) => self.insert_file(file, span),
            HostIoValue::Socket(socket) => self.insert_socket(socket, span),
        }
    }

    fn enqueue_task(&mut self, task_id: u64) {
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return;
        };
        if !matches!(task.status, TaskStatus::Runnable) || task.queued {
            return;
        }
        task.queued = true;
        self.ready.push_back(task_id);
    }

    fn wake_parent(&mut self, child: u64) {
        let Some(parent) = self.tasks.get(&child).and_then(|task| task.parent) else {
            return;
        };
        let Some(waiting) = self.tasks.get(&parent) else {
            return;
        };
        if !matches!(waiting.status, TaskStatus::Waiting) || !waiting.children.contains(&child) {
            return;
        }
        let children = waiting.children.clone();
        let mode = waiting.join_mode;
        let mut winner = waiting.join_winner;
        let child_status = self.tasks.get(&child).map(|task| task.status.clone());
        if winner.is_none() {
            let selected = match mode {
                TaskJoinMode::Any => {
                    matches!(child_status, Some(TaskStatus::Completed(_)))
                }
                TaskJoinMode::Race => child_status.as_ref().is_some_and(task_terminal),
                TaskJoinMode::All | TaskJoinMode::Settled => false,
            };
            if selected {
                winner = children.iter().position(|candidate| *candidate == child);
                self.tasks
                    .get_mut(&parent)
                    .expect("parent exists")
                    .join_winner = winner;
            }
        }
        let all_failed = mode == TaskJoinMode::All
            && matches!(
                child_status,
                Some(TaskStatus::Failed(_) | TaskStatus::Cancelled)
            );
        if all_failed || winner.is_some() {
            for (index, sibling) in children.iter().copied().enumerate() {
                if Some(index) != winner && sibling != child {
                    self.cancel_task(sibling);
                }
            }
        }
        let ready = children.iter().all(|child| {
            self.tasks
                .get(child)
                .is_some_and(|task| task_terminal(&task.status))
        });
        if !ready {
            return;
        }
        self.tasks.get_mut(&parent).expect("parent checked").status = TaskStatus::Runnable;
        self.enqueue_task(parent);
    }

    fn cancel_task(&mut self, task_id: u64) {
        let Some(task) = self.tasks.get_mut(&task_id) else {
            return;
        };
        if task_terminal(&task.status) || task.cancel_requested {
            return;
        }
        task.cancel_requested = true;
        task.timer_deadline = None;
        let children = task.children.clone();
        task.status = TaskStatus::Runnable;
        self.cancel_socket_io(task_id);
        for child in children {
            self.cancel_task(child);
        }
        self.enqueue_task(task_id);
    }

    #[allow(clippy::too_many_lines)]
    fn resume_task(&mut self, task_id: u64) -> Result<TaskPoll, ExecutionFailure> {
        match self.tasks.get(&task_id).map(|task| &task.status) {
            Some(TaskStatus::Completed(_)) => return Ok(TaskPoll::Completed),
            Some(TaskStatus::Failed(_) | TaskStatus::Cancelled) => {
                return Ok(TaskPoll::Failed);
            }
            Some(TaskStatus::Runnable) => {}
            Some(TaskStatus::Waiting) => {
                return Err(self
                    .runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "scheduler resumed a task that is still waiting",
                        Span::default(),
                    )
                    .into());
            }
            None => {
                return Err(self
                    .runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "scheduler referenced an unknown task",
                        Span::default(),
                    )
                    .into());
            }
        }
        if self
            .tasks
            .get(&task_id)
            .is_some_and(|task| task.cancel_requested)
        {
            return Ok(self.finish_task(task_id, Err(EvalAbort::Cancelled)));
        }
        if let Some(deadline) = self
            .tasks
            .get(&task_id)
            .and_then(|task| task.timer_deadline)
        {
            if Instant::now() < deadline {
                self.tasks
                    .get_mut(&task_id)
                    .expect("timer task exists")
                    .status = TaskStatus::Waiting;
                return Ok(TaskPoll::Pending);
            }
            self.tasks
                .get_mut(&task_id)
                .expect("timer task exists")
                .status = TaskStatus::Completed(Value::Unit);
            return Ok(TaskPoll::Completed);
        }
        let (function_id, frame) = {
            let task = self.tasks.get(&task_id).expect("task checked above");
            (task.function, task.frame)
        };
        let program = self.program;
        let function = program.function(function_id).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "async task references an unknown function",
                Span::default(),
            ))
        })?;
        if let Err(failure) = self.enter_async_contracts(task_id, frame, function) {
            return Ok(self.finish_task(task_id, Err(EvalAbort::Failure(failure))));
        }
        loop {
            let cursor = self.tasks.get(&task_id).expect("task exists").cursor;
            if let Some(statement) = function.body.statements.get(cursor) {
                if let StatementKind::Defer(cleanup) = &statement.kind {
                    let task = self.tasks.get_mut(&task_id).expect("task exists");
                    task.cleanups
                        .push(RuntimeCleanup::Deferred(cleanup.clone()));
                    task.cursor += 1;
                    continue;
                }

                let await_action = match &statement.kind {
                    StatementKind::Let { local, value } => {
                        await_expr(value).map(|awaited| (AwaitDestination::Local(*local), awaited))
                    }
                    StatementKind::Scoped {
                        local,
                        value,
                        disposal,
                    } => await_expr(value).map(|awaited| {
                        (AwaitDestination::Scoped(*local, disposal.clone()), awaited)
                    }),
                    StatementKind::LetTuple { locals, value } => await_expr(value)
                        .map(|awaited| (AwaitDestination::Tuple(locals.clone()), awaited)),
                    StatementKind::Evaluate(value) => {
                        await_expr(value).map(|awaited| (AwaitDestination::Ignore, awaited))
                    }
                    StatementKind::Return(Some(value)) => {
                        await_expr(value).map(|awaited| (AwaitDestination::Return, awaited))
                    }
                    StatementKind::Assign { .. }
                    | StatementKind::Assert { .. }
                    | StatementKind::ForRange { .. }
                    | StatementKind::While { .. }
                    | StatementKind::Break
                    | StatementKind::Continue
                    | StatementKind::Return(None)
                    | StatementKind::Defer(_) => None,
                };
                if let Some((destination, awaited)) = await_action {
                    match self.poll_await(task_id, frame, awaited)? {
                        AwaitPoll::Pending => return Ok(TaskPoll::Pending),
                        AwaitPoll::Failed(failure) => {
                            return Ok(self.finish_task(task_id, Err(EvalAbort::Failure(failure))));
                        }
                        AwaitPoll::Cancelled => {
                            return Ok(self.finish_task(task_id, Err(EvalAbort::Cancelled)));
                        }
                        AwaitPoll::Control(control) => {
                            return Ok(self.finish_task(task_id, Err(control)));
                        }
                        AwaitPoll::Ready(value) => {
                            match destination {
                                AwaitDestination::Ignore => {}
                                AwaitDestination::Local(local) => {
                                    self.set_slot(
                                        frame,
                                        local,
                                        Slot::Value(value),
                                        statement.span,
                                    )?;
                                }
                                AwaitDestination::Scoped(local, disposal) => {
                                    self.set_slot(
                                        frame,
                                        local,
                                        Slot::Value(value),
                                        statement.span,
                                    )?;
                                    self.tasks
                                        .get_mut(&task_id)
                                        .expect("task exists")
                                        .cleanups
                                        .push(RuntimeCleanup::Scoped {
                                            local,
                                            disposal,
                                            span: statement.span,
                                        });
                                }
                                AwaitDestination::Tuple(locals) => {
                                    self.bind_tuple(frame, &locals, value, statement.span)?;
                                }
                                AwaitDestination::Return => {
                                    return Ok(self.finish_task(
                                        task_id,
                                        Err(EvalAbort::Return(Box::new(value))),
                                    ));
                                }
                            }
                            self.tasks.get_mut(&task_id).expect("task exists").cursor += 1;
                            continue;
                        }
                    }
                }

                if let StatementKind::Scoped {
                    local,
                    value,
                    disposal,
                } = &statement.kind
                {
                    match self.eval_expr(frame, value) {
                        Ok(value) => {
                            self.set_slot(frame, *local, Slot::Value(value), statement.span)?;
                            self.tasks
                                .get_mut(&task_id)
                                .expect("task exists")
                                .cleanups
                                .push(RuntimeCleanup::Scoped {
                                    local: *local,
                                    disposal: disposal.clone(),
                                    span: statement.span,
                                });
                            self.tasks.get_mut(&task_id).expect("task exists").cursor += 1;
                        }
                        Err(abort) => return Ok(self.finish_task(task_id, Err(abort))),
                    }
                    continue;
                }

                match self.eval_statement(frame, statement) {
                    Ok(()) => {
                        self.tasks.get_mut(&task_id).expect("task exists").cursor += 1;
                    }
                    Err(abort) => return Ok(self.finish_task(task_id, Err(abort))),
                }
                continue;
            }

            if let Some(tail) = function.body.tail.as_deref() {
                if let Some(awaited) = await_expr(tail) {
                    match self.poll_await(task_id, frame, awaited)? {
                        AwaitPoll::Pending => return Ok(TaskPoll::Pending),
                        AwaitPoll::Ready(value) => {
                            return Ok(self.finish_task(task_id, Ok(value)));
                        }
                        AwaitPoll::Failed(failure) => {
                            return Ok(self.finish_task(task_id, Err(EvalAbort::Failure(failure))));
                        }
                        AwaitPoll::Cancelled => {
                            return Ok(self.finish_task(task_id, Err(EvalAbort::Cancelled)));
                        }
                        AwaitPoll::Control(control) => {
                            return Ok(self.finish_task(task_id, Err(control)));
                        }
                    }
                }
                let outcome = self.eval_expr(frame, tail);
                return Ok(self.finish_task(task_id, outcome));
            }
            return Ok(self.finish_task(task_id, Ok(Value::Unit)));
        }
    }

    #[allow(clippy::too_many_lines)]
    fn poll_await(
        &mut self,
        task_id: u64,
        frame: u64,
        awaited: &Expr,
    ) -> Result<AwaitPoll, ExecutionFailure> {
        let ExprKind::Await { state, task } = &awaited.kind else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "scheduler received a non-Await expression",
                    awaited.span,
                )
                .into());
        };
        let awaiting = self
            .tasks
            .get(&task_id)
            .and_then(|task| task.awaiting_state);
        if awaiting.is_none() {
            let value = match self.eval_expr(frame, task) {
                Ok(value) => value,
                Err(EvalAbort::Failure(failure)) => return Ok(AwaitPoll::Failed(failure)),
                Err(control @ (EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue)) => {
                    return Ok(AwaitPoll::Control(control));
                }
                Err(EvalAbort::Cancelled) => return Ok(AwaitPoll::Cancelled),
            };
            let (children, mode, dynamic, combined) = match value {
                Value::Task { id } => (vec![id], TaskJoinMode::All, false, false),
                Value::Tuple { elements } => {
                    let mut children = Vec::with_capacity(elements.len());
                    for element in elements {
                        let Value::Task { id } = element else {
                            return Err(self
                                .runtime_fault(
                                    "LOOM_RUNTIME_INVALID_MIR",
                                    "Await tuple contained a non-task value",
                                    awaited.span,
                                )
                                .into());
                        };
                        children.push(id);
                    }
                    (children, TaskJoinMode::All, false, true)
                }
                Value::TaskJoin {
                    mode,
                    tasks,
                    dynamic,
                } => (tasks, mode, dynamic, true),
                _ => {
                    return Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "Await operand did not construct a task or task tuple",
                            awaited.span,
                        )
                        .into());
                }
            };
            if children.is_empty() {
                return match mode {
                    TaskJoinMode::All | TaskJoinMode::Settled => {
                        Ok(AwaitPoll::Ready(Value::List {
                            elements: Vec::new(),
                        }))
                    }
                    TaskJoinMode::Any | TaskJoinMode::Race => Ok(AwaitPoll::Failed(
                        self.runtime_fault(
                            "EmptyTaskJoin",
                            "Task.any and Task.race require a non-empty task list",
                            awaited.span,
                        )
                        .into(),
                    )),
                };
            }
            let current = self.tasks.get_mut(&task_id).expect("task exists");
            current.awaiting_state = Some(*state);
            current.children.clone_from(&children);
            current.join_mode = mode;
            current.join_dynamic = dynamic;
            current.join_combined = combined;
            current.join_winner = None;
            current.status = TaskStatus::Waiting;
            for child in children {
                if let Some(child_task) = self.tasks.get_mut(&child) {
                    child_task.parent = Some(task_id);
                }
            }
            let ready = self.tasks.get(&task_id).is_some_and(|parent| {
                parent.children.iter().all(|child| {
                    self.tasks
                        .get(child)
                        .is_some_and(|child| task_terminal(&child.status))
                })
            });
            if ready {
                self.tasks.get_mut(&task_id).expect("task exists").status = TaskStatus::Runnable;
                self.enqueue_task(task_id);
            }
            return Ok(AwaitPoll::Pending);
        }
        if awaiting != Some(*state) {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "task resumed at a different Await state",
                    awaited.span,
                )
                .into());
        }
        let children = self
            .tasks
            .get(&task_id)
            .map(|task| task.children.clone())
            .filter(|children| !children.is_empty())
            .ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "waiting task has no children",
                    awaited.span,
                ))
            })?;
        let (mode, dynamic, combined, winner) = {
            let current = self.tasks.get(&task_id).expect("task exists");
            (
                current.join_mode,
                current.join_dynamic,
                current.join_combined,
                current.join_winner,
            )
        };
        let mut values = Vec::with_capacity(children.len());
        let mut outcomes = Vec::with_capacity(children.len());
        let mut failure = None;
        let mut cancelled = false;
        for child in &children {
            match self.tasks.get(child).map(|task| task.status.clone()) {
                Some(TaskStatus::Completed(value)) => {
                    outcomes.push(self.task_outcome_completed(value.clone(), awaited.span)?);
                    values.push(Some(value));
                }
                Some(TaskStatus::Failed(child_failure)) => {
                    outcomes.push(self.task_outcome_faulted(&child_failure, awaited.span)?);
                    failure.get_or_insert(child_failure);
                    values.push(None);
                }
                Some(TaskStatus::Cancelled) => {
                    outcomes.push(self.task_outcome_cancelled(awaited.span)?);
                    cancelled = true;
                    values.push(None);
                }
                Some(TaskStatus::Runnable | TaskStatus::Waiting) => {
                    self.tasks.get_mut(&task_id).expect("task exists").status = TaskStatus::Waiting;
                    return Ok(AwaitPoll::Pending);
                }
                None => {
                    return Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "waiting task references a collected child",
                            awaited.span,
                        )
                        .into());
                }
            }
        }
        let current = self.tasks.get_mut(&task_id).expect("task exists");
        current.awaiting_state = None;
        current.children.clear();
        self.collect_tasks(self.active_root);
        match mode {
            TaskJoinMode::All => {
                if let Some(failure) = failure {
                    return Ok(AwaitPoll::Failed(failure));
                }
                if cancelled {
                    return Ok(AwaitPoll::Cancelled);
                }
                let values = values.into_iter().flatten().collect::<Vec<_>>();
                if dynamic {
                    Ok(AwaitPoll::Ready(Value::List { elements: values }))
                } else if combined {
                    Ok(AwaitPoll::Ready(Value::Tuple { elements: values }))
                } else {
                    Ok(AwaitPoll::Ready(
                        values.into_iter().next().expect("single task completed"),
                    ))
                }
            }
            TaskJoinMode::Settled => {
                if dynamic {
                    Ok(AwaitPoll::Ready(Value::List { elements: outcomes }))
                } else {
                    Ok(AwaitPoll::Ready(Value::Tuple { elements: outcomes }))
                }
            }
            TaskJoinMode::Any => {
                if let Some(index) = winner
                    && let Some(value) = values.get_mut(index).and_then(Option::take)
                {
                    Ok(AwaitPoll::Ready(value))
                } else {
                    Ok(AwaitPoll::Failed(
                        self.runtime_fault(
                            loom_core::runtime_fault::TASK_ANY_FAILED_FAULT_CODE,
                            loom_core::runtime_fault::TASK_ANY_FAILED_FAULT_MESSAGE,
                            awaited.span,
                        )
                        .into(),
                    ))
                }
            }
            TaskJoinMode::Race => {
                let index = winner.ok_or_else(|| {
                    ExecutionFailure::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "Task.race resumed without a winner",
                        awaited.span,
                    ))
                })?;
                Ok(AwaitPoll::Ready(outcomes[index].clone()))
            }
        }
    }

    fn eval_nested_await(&mut self, frame: u64, awaited: &Expr) -> Result<Value, EvalAbort> {
        let task_id = self.active_task.ok_or_else(|| {
            EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "nested Await has no active async task",
                awaited.span,
            ))
        })?;
        loop {
            if self
                .tasks
                .get(&task_id)
                .is_some_and(|task| task.cancel_requested)
            {
                self.remove_ready_task(task_id);
                return Err(EvalAbort::Cancelled);
            }
            match self
                .poll_await(task_id, frame, awaited)
                .map_err(EvalAbort::from)?
            {
                AwaitPoll::Ready(value) => {
                    self.remove_ready_task(task_id);
                    return Ok(value);
                }
                AwaitPoll::Failed(failure) => return Err(EvalAbort::Failure(failure)),
                AwaitPoll::Control(control) => return Err(control),
                AwaitPoll::Cancelled => return Err(EvalAbort::Cancelled),
                AwaitPoll::Pending => self.drive_nested_wait(task_id, awaited.span)?,
            }
        }
    }

    fn drive_nested_wait(&mut self, parent: u64, span: Span) -> Result<(), EvalAbort> {
        loop {
            let parent_ready = self.tasks.get(&parent).is_some_and(|task| {
                task.cancel_requested
                    || (task.awaiting_state.is_some()
                        && matches!(task.status, TaskStatus::Runnable))
            });
            if parent_ready {
                self.remove_ready_task(parent);
                self.active_task = Some(parent);
                return Ok(());
            }

            let Some(current) = self.ready.pop_front() else {
                if self.wait_for_work() {
                    continue;
                }
                return Err(EvalAbort::from(self.runtime_fault(
                    "AsyncDeadlock",
                    "no runnable task can satisfy the nested await",
                    span,
                )));
            };
            if current == parent {
                if let Some(task) = self.tasks.get_mut(&parent) {
                    task.queued = false;
                }
                self.active_task = Some(parent);
                return Ok(());
            }
            let Some(task) = self.tasks.get_mut(&current) else {
                continue;
            };
            task.queued = false;
            if !matches!(task.status, TaskStatus::Runnable) {
                continue;
            }
            self.active_task = Some(current);
            let poll = self.resume_task(current).map_err(EvalAbort::from)?;
            self.active_task = Some(parent);
            if matches!(poll, TaskPoll::Completed | TaskPoll::Failed) {
                self.wake_parent(current);
            }
        }
    }

    fn remove_ready_task(&mut self, task_id: u64) {
        self.ready.retain(|candidate| *candidate != task_id);
        if let Some(task) = self.tasks.get_mut(&task_id) {
            task.queued = false;
        }
    }

    fn finish_task(&mut self, task_id: u64, outcome: Result<Value, EvalAbort>) -> TaskPoll {
        let (frame, cleanups) = {
            let task = self.tasks.get_mut(&task_id).expect("task exists");
            (task.frame, std::mem::take(&mut task.cleanups))
        };
        let mut outcome = self.run_cleanups(frame, cleanups, outcome);
        if matches!(&outcome, Err(EvalAbort::Break | EvalAbort::Continue)) {
            outcome = Err(EvalAbort::Failure(
                self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "loop control escaped its enclosing loop",
                    Span::default(),
                )
                .into(),
            ));
        }
        let completed_value = match &outcome {
            Ok(value) => Some(value.clone()),
            Err(EvalAbort::Return(value)) => Some((**value).clone()),
            Err(
                EvalAbort::Failure(_)
                | EvalAbort::Break
                | EvalAbort::Continue
                | EvalAbort::Cancelled,
            ) => None,
        };
        if let Some(value) = completed_value {
            let function_id = self
                .tasks
                .get(&task_id)
                .and_then(|task| task.contract_state.as_ref().map(|_| task.function));
            if let Some(function_id) = function_id {
                let program = self.program;
                let checked = program
                    .function(function_id)
                    .ok_or_else(|| {
                        ExecutionFailure::from(self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "async task references an unknown function at exit",
                            Span::default(),
                        ))
                    })
                    .and_then(|function| {
                        self.check_async_exit_contracts(task_id, frame, function, &value)
                    });
                if let Err(failure) = checked {
                    outcome = Err(EvalAbort::Failure(failure));
                }
            }
        }
        let status = match outcome {
            Ok(value) => TaskStatus::Completed(value),
            Err(EvalAbort::Return(value)) => TaskStatus::Completed(*value),
            Err(EvalAbort::Failure(failure)) => TaskStatus::Failed(failure),
            Err(EvalAbort::Break | EvalAbort::Continue) => unreachable!("normalized above"),
            Err(EvalAbort::Cancelled) => TaskStatus::Cancelled,
        };
        let failed = matches!(&status, TaskStatus::Failed(_) | TaskStatus::Cancelled);
        self.tasks.get_mut(&task_id).expect("task exists").status = status;
        if failed {
            TaskPoll::Failed
        } else {
            TaskPoll::Completed
        }
    }

    fn collect_tasks(&mut self, root: Option<u64>) {
        self.gc_stats.collections = self.gc_stats.collections.saturating_add(1);
        for task in self.tasks.values_mut() {
            task.marked = false;
        }
        let mut pending = root.into_iter().collect::<Vec<_>>();
        while let Some(task_id) = pending.pop() {
            let frame = {
                let Some(task) = self.tasks.get_mut(&task_id) else {
                    continue;
                };
                if task.marked {
                    continue;
                }
                task.marked = true;
                pending.extend(task.children.iter().copied());
                if let TaskStatus::Completed(value) = &task.status {
                    referenced_task_ids(value, &mut pending);
                }
                if let Some(contracts) = &task.contract_state {
                    if let Some(receiver) = &contracts.old_receiver {
                        referenced_task_ids(receiver, &mut pending);
                    }
                    for argument in contracts.old_arguments.iter().flatten() {
                        referenced_task_ids(argument, &mut pending);
                    }
                }
                task.frame
            };
            if let Some(frame) = self.frames.get(&frame) {
                for slot in &frame.slots {
                    if let Slot::Value(value) = slot {
                        referenced_task_ids(value, &mut pending);
                    }
                }
            }
        }
        let unreachable = self
            .tasks
            .iter()
            .filter_map(|(id, task)| (!task.marked).then_some((*id, task.frame)))
            .collect::<Vec<_>>();
        for (task, frame) in unreachable {
            self.cancel_socket_io(task);
            self.tasks.remove(&task);
            self.frames.remove(&frame);
            self.gc_stats.reclaimed = self.gc_stats.reclaimed.saturating_add(1);
        }
        self.ready.retain(|task| self.tasks.contains_key(task));
        self.gc_stats.live = self.tasks.len() as u64;
    }

    fn invoke_bound(
        &mut self,
        function_id: FunctionId,
        arguments: Vec<BoundArgument>,
        witnesses: Vec<RuntimeWitness>,
        call_site: Span,
    ) -> Result<Value, ExecutionFailure> {
        match self.begin_bound(function_id, arguments, witnesses, call_site)? {
            BoundInvocation::Complete(value) => Ok(value),
            BoundInvocation::Sync {
                frame,
                function,
                call_site,
            } => self.drive_sync(frame, function, call_site),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn begin_bound(
        &mut self,
        function_id: FunctionId,
        arguments: Vec<BoundArgument>,
        witnesses: Vec<RuntimeWitness>,
        call_site: Span,
    ) -> Result<BoundInvocation<'program>, ExecutionFailure> {
        self.tick(call_site)?;
        if self.call_depth >= self.max_call_depth {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_CALL_DEPTH",
                    "call depth limit exceeded",
                    call_site,
                )
                .into());
        }
        let program = self.program;
        let function = program.function(function_id).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                format!("function #{} does not exist", function_id.0),
                call_site,
            ))
        })?;
        if function.params.len() != arguments.len() {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    format!(
                        "{} expects {} arguments, received {}",
                        function.name,
                        function.params.len(),
                        arguments.len()
                    ),
                    call_site,
                )
                .into());
        }
        if function.witness_params.len() != witnesses.len() {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    format!(
                        "{} expects {} witness arguments, received {}",
                        function.name,
                        function.witness_params.len(),
                        witnesses.len()
                    ),
                    call_site,
                )
                .into());
        }

        let slot_count = function
            .params
            .iter()
            .chain(&function.locals)
            .map(|local| local.id.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let frame_id = self.next_frame;
        self.next_frame += 1;
        let mut frame = Frame {
            slots: vec![Slot::Empty; slot_count],
            witnesses,
        };
        for (parameter, argument) in function.params.iter().zip(arguments) {
            frame.slots[parameter.id.0 as usize] = match argument {
                BoundArgument::Value(value) => Slot::Value(value),
                BoundArgument::Alias(location) => Slot::Alias(location),
            };
        }
        self.frames.insert(frame_id, frame);
        if function.is_async {
            let task_id = self.next_task;
            self.next_task = self.next_task.checked_add(1).ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "TaskIdExhausted",
                    "async task identity space was exhausted",
                    call_site,
                ))
            })?;
            self.tasks.insert(
                task_id,
                ManagedTask {
                    function: function_id,
                    frame: frame_id,
                    parent: self.active_task,
                    children: Vec::new(),
                    cursor: 0,
                    awaiting_state: None,
                    cleanups: Vec::new(),
                    status: TaskStatus::Runnable,
                    queued: false,
                    marked: false,
                    timer_deadline: None,
                    host_io: false,
                    contract_state: Some(AsyncContractState {
                        call_site,
                        entered: false,
                        old_receiver: None,
                        old_arguments: Vec::new(),
                    }),
                    join_mode: TaskJoinMode::All,
                    join_dynamic: false,
                    join_combined: false,
                    join_winner: None,
                    cancel_requested: false,
                },
            );
            self.enqueue_task(task_id);
            self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
            self.gc_stats.live = self.tasks.len() as u64;
            return Ok(BoundInvocation::Complete(Value::Task { id: task_id }));
        }
        self.call_depth += 1;
        Ok(BoundInvocation::Sync {
            frame: frame_id,
            function,
            call_site,
        })
    }

    fn drive_sync(
        &mut self,
        frame: u64,
        function: &'program Function,
        call_site: Span,
    ) -> Result<Value, ExecutionFailure> {
        let mut active_frames = vec![frame];
        let mut continuations: Vec<SyncResume<'program, Value>> = Vec::new();
        let mut step = self.sync_execute_function(frame, function, call_site);

        loop {
            match step {
                SyncStep::Complete(outcome) => {
                    let outcome = match outcome {
                        Ok(value) => Ok(value),
                        Err(EvalAbort::Failure(failure)) => Err(failure),
                        Err(EvalAbort::Return(_)) => Err(self
                            .runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "function return escaped its execution boundary",
                                call_site,
                            )
                            .into()),
                        Err(EvalAbort::Break | EvalAbort::Continue) => Err(self
                            .runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "loop control escaped its enclosing loop",
                                call_site,
                            )
                            .into()),
                        Err(EvalAbort::Cancelled) => Err(self
                            .runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "synchronous function observed task cancellation",
                                call_site,
                            )
                            .into()),
                    };
                    let completed_frame = active_frames
                        .pop()
                        .expect("sync activation must own a frame");
                    self.frames.remove(&completed_frame);
                    self.call_depth -= 1;
                    let Some(resume) = continuations.pop() else {
                        return outcome;
                    };
                    step = resume(self, outcome);
                }
                SyncStep::Call { request, resume } => {
                    match self.begin_bound(
                        request.function,
                        request.arguments,
                        request.witnesses,
                        request.span,
                    ) {
                        Ok(BoundInvocation::Complete(value)) => {
                            step = resume(self, Ok(value));
                        }
                        Ok(BoundInvocation::Sync {
                            frame,
                            function,
                            call_site,
                        }) => {
                            continuations.push(resume);
                            active_frames.push(frame);
                            step = self.sync_execute_function(frame, function, call_site);
                        }
                        Err(failure) => {
                            step = resume(self, Err(failure));
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn sync_execute_function(
        &mut self,
        frame: u64,
        function: &'program Function,
        call_site: Span,
    ) -> SyncStep<'program, Value> {
        let snapshots = (|| -> Result<_, ExecutionFailure> {
            let parameter_values = self.read_parameter_values(frame, function)?;
            let (receiver, arguments) = self.contract_arguments(function, &parameter_values)?;

            if let (Some(contract), Some(receiver_value)) =
                (&function.call_plan.receiver_invariant, receiver.as_ref())
            {
                self.require_contract(
                    contract,
                    ContractFaultKind::Invariant,
                    receiver_value,
                    arguments,
                    None,
                    None,
                    &[],
                    contract.span,
                )?;
            }
            for contract in &function.call_plan.requires {
                self.require_contract(
                    contract,
                    ContractFaultKind::Precondition,
                    receiver.as_ref().unwrap_or(&Value::Unit),
                    arguments,
                    None,
                    None,
                    &[],
                    call_site,
                )?;
            }

            let snapshot_needs = old_snapshot_needs(&function.call_plan.ensures, arguments.len());
            let old_arguments = arguments
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    snapshot_needs
                        .arguments
                        .get(index)
                        .copied()
                        .unwrap_or(false)
                        .then(|| value.clone())
                })
                .collect::<Vec<_>>();
            let old_receiver = snapshot_needs.receiver.then(|| receiver.clone()).flatten();
            Ok((old_receiver, old_arguments))
        })();
        let (old_receiver, old_arguments) = match snapshots {
            Ok(snapshots) => snapshots,
            Err(failure) => return SyncStep::fail(failure),
        };

        let body = self.sync_eval_block(frame, &function.body);
        body.then(self, move |interpreter, outcome| {
            let result = match outcome {
                Ok(value) => value,
                Err(EvalAbort::Return(value)) => *value,
                Err(EvalAbort::Failure(failure)) => return SyncStep::fail(failure),
                Err(EvalAbort::Break | EvalAbort::Continue) => {
                    return SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "loop control escaped its enclosing loop",
                        call_site,
                    ));
                }
                Err(EvalAbort::Cancelled) => {
                    return SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "synchronous function observed task cancellation",
                        call_site,
                    ));
                }
            };
            if function.call_plan.receiver_invariant.is_none()
                && function.call_plan.ensures.is_empty()
            {
                return SyncStep::complete(result);
            }

            let contracts = (|| -> Result<(), ExecutionFailure> {
                let current_parameter_values =
                    interpreter.read_parameter_values(frame, function)?;
                let (current_receiver, current_arguments) =
                    interpreter.contract_arguments(function, &current_parameter_values)?;
                if let (Some(contract), Some(receiver_value)) = (
                    &function.call_plan.receiver_invariant,
                    current_receiver.as_ref(),
                ) {
                    interpreter.require_contract(
                        contract,
                        ContractFaultKind::Invariant,
                        receiver_value,
                        current_arguments,
                        Some(&result),
                        old_receiver.as_ref(),
                        &old_arguments,
                        contract.span,
                    )?;
                }
                for contract in &function.call_plan.ensures {
                    interpreter.require_contract(
                        contract,
                        ContractFaultKind::Postcondition,
                        current_receiver.as_ref().unwrap_or(&Value::Unit),
                        current_arguments,
                        Some(&result),
                        old_receiver.as_ref(),
                        &old_arguments,
                        contract.span,
                    )?;
                }
                Ok(())
            })();
            match contracts {
                Ok(()) => SyncStep::complete(result),
                Err(failure) => SyncStep::fail(failure),
            }
        })
    }

    fn enter_async_contracts(
        &mut self,
        task_id: u64,
        frame: u64,
        function: &Function,
    ) -> Result<(), ExecutionFailure> {
        let state = self
            .tasks
            .get(&task_id)
            .and_then(|task| task.contract_state.as_ref())
            .cloned()
            .ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "async function task is missing contract state",
                    function.span,
                ))
            })?;
        if state.entered {
            return Ok(());
        }

        let parameter_values = self.read_parameter_values(frame, function)?;
        let (receiver, arguments) = self.contract_arguments(function, &parameter_values)?;
        if let (Some(contract), Some(receiver_value)) =
            (&function.call_plan.receiver_invariant, receiver.as_ref())
        {
            self.require_contract(
                contract,
                ContractFaultKind::Invariant,
                receiver_value,
                arguments,
                None,
                None,
                &[],
                contract.span,
            )?;
        }
        for contract in &function.call_plan.requires {
            self.require_contract(
                contract,
                ContractFaultKind::Precondition,
                receiver.as_ref().unwrap_or(&Value::Unit),
                arguments,
                None,
                None,
                &[],
                state.call_site,
            )?;
        }

        let snapshot_needs = old_snapshot_needs(&function.call_plan.ensures, arguments.len());
        let old_arguments = arguments
            .iter()
            .enumerate()
            .map(|(index, value)| {
                snapshot_needs
                    .arguments
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    .then(|| value.clone())
            })
            .collect::<Vec<_>>();
        let old_receiver = snapshot_needs.receiver.then(|| receiver.clone()).flatten();
        let contract_state = self
            .tasks
            .get_mut(&task_id)
            .and_then(|task| task.contract_state.as_mut())
            .expect("async contract state was checked above");
        contract_state.entered = true;
        contract_state.old_receiver = old_receiver;
        contract_state.old_arguments = old_arguments;
        Ok(())
    }

    fn check_async_exit_contracts(
        &mut self,
        task_id: u64,
        frame: u64,
        function: &Function,
        result: &Value,
    ) -> Result<(), ExecutionFailure> {
        let state = self
            .tasks
            .get(&task_id)
            .and_then(|task| task.contract_state.as_ref())
            .cloned()
            .ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "async function task is missing contract state",
                    function.span,
                ))
            })?;
        if !state.entered {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "async function exited before entry contracts",
                    function.span,
                )
                .into());
        }
        if function.call_plan.receiver_invariant.is_none() && function.call_plan.ensures.is_empty()
        {
            return Ok(());
        }
        let parameter_values = self.read_parameter_values(frame, function)?;
        let (receiver, arguments) = self.contract_arguments(function, &parameter_values)?;
        if let (Some(contract), Some(receiver_value)) =
            (&function.call_plan.receiver_invariant, receiver.as_ref())
        {
            self.require_contract(
                contract,
                ContractFaultKind::Invariant,
                receiver_value,
                arguments,
                Some(result),
                state.old_receiver.as_ref(),
                &state.old_arguments,
                contract.span,
            )?;
        }
        for contract in &function.call_plan.ensures {
            self.require_contract(
                contract,
                ContractFaultKind::Postcondition,
                receiver.as_ref().unwrap_or(&Value::Unit),
                arguments,
                Some(result),
                state.old_receiver.as_ref(),
                &state.old_arguments,
                contract.span,
            )?;
        }
        Ok(())
    }

    fn read_parameter_values(
        &self,
        frame: u64,
        function: &Function,
    ) -> Result<Vec<Value>, ExecutionFailure> {
        function
            .params
            .iter()
            .map(|parameter| self.read_place(&Location::local(frame, parameter.id), function.span))
            .collect()
    }

    fn contract_arguments<'values>(
        &self,
        function: &Function,
        values: &'values [Value],
    ) -> Result<(Option<Value>, &'values [Value]), ExecutionFailure> {
        if function.receiver.is_none() {
            return Ok((None, values));
        }
        let Some((receiver, arguments)) = values.split_first() else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "receiver function has no receiver parameter",
                    function.span,
                )
                .into());
        };
        Ok((Some(receiver.clone()), arguments))
    }

    #[allow(clippy::too_many_arguments)]
    fn require_contract(
        &mut self,
        contract: &Contract,
        category: ContractFaultKind,
        receiver: &Value,
        arguments: &[Value],
        result: Option<&Value>,
        old_receiver: Option<&Value>,
        old_arguments: &[Option<Value>],
        blame_span: Span,
    ) -> Result<(), ExecutionFailure> {
        let context = ContractContext {
            receiver: Some(receiver),
            result,
            arguments,
            old_receiver,
            old_arguments,
            bindings: &[],
        };
        let value = self.eval_contract(&contract.expression, &context)?;
        match value {
            Value::Bool { value: true } => Ok(()),
            Value::Bool { value: false } => Err(ContractFault {
                code: match category {
                    ContractFaultKind::Precondition => "PreconditionFault",
                    ContractFaultKind::Postcondition => "PostconditionFault",
                    ContractFaultKind::Invariant => "InvariantFault",
                    ContractFaultKind::Assertion => "AssertionFault",
                }
                .into(),
                category,
                message: format!("contract `{}` was not satisfied", contract.code),
                contract_span: contract.span,
                blame_span,
            }
            .into()),
            _ => Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "contract expression did not produce Bool",
                    contract.span,
                )
                .into()),
        }
    }

    fn sync_eval_block(&mut self, frame: u64, block: &'program Block) -> SyncStep<'program, Value> {
        if let Err(failure) = self.tick(block.span) {
            return SyncStep::fail(failure);
        }
        self.sync_eval_block_from(frame, block, 0, Vec::new())
    }

    #[allow(clippy::too_many_lines)]
    fn sync_eval_block_from(
        &mut self,
        frame: u64,
        block: &'program Block,
        mut cursor: usize,
        mut cleanups: Vec<SyncCleanup<'program>>,
    ) -> SyncStep<'program, Value> {
        while let Some(statement) = block.statements.get(cursor) {
            match &statement.kind {
                StatementKind::Defer(cleanup) => {
                    cleanups.push(SyncCleanup::Deferred(cleanup));
                    cursor += 1;
                }
                StatementKind::Scoped {
                    local,
                    value,
                    disposal,
                } => {
                    let local = *local;
                    let span = statement.span;
                    match self.sync_eval_expr(frame, value) {
                        SyncStep::Complete(Ok(value)) => {
                            if let Err(failure) =
                                self.set_slot(frame, local, Slot::Value(value), span)
                            {
                                return self.sync_run_cleanups(
                                    frame,
                                    cleanups,
                                    Err(EvalAbort::from(failure)),
                                );
                            }
                            cleanups.push(SyncCleanup::Scoped {
                                local,
                                disposal,
                                span,
                            });
                            cursor += 1;
                        }
                        SyncStep::Complete(Err(abort)) => {
                            return self.sync_run_cleanups(frame, cleanups, Err(abort));
                        }
                        SyncStep::Call { request, resume } => {
                            return SyncStep::Call {
                                request,
                                resume: Box::new(move |interpreter, result| {
                                    resume(interpreter, result).then(
                                        interpreter,
                                        move |interpreter, outcome| match outcome {
                                            Ok(value) => {
                                                if let Err(failure) = interpreter.set_slot(
                                                    frame,
                                                    local,
                                                    Slot::Value(value),
                                                    span,
                                                ) {
                                                    return interpreter.sync_run_cleanups(
                                                        frame,
                                                        cleanups,
                                                        Err(EvalAbort::from(failure)),
                                                    );
                                                }
                                                cleanups.push(SyncCleanup::Scoped {
                                                    local,
                                                    disposal,
                                                    span,
                                                });
                                                interpreter.sync_eval_block_from(
                                                    frame,
                                                    block,
                                                    cursor + 1,
                                                    cleanups,
                                                )
                                            }
                                            Err(abort) => interpreter.sync_run_cleanups(
                                                frame,
                                                cleanups,
                                                Err(abort),
                                            ),
                                        },
                                    )
                                }),
                            };
                        }
                    }
                }
                _ => match self.sync_eval_statement(frame, statement) {
                    SyncStep::Complete(Ok(())) => cursor += 1,
                    SyncStep::Complete(Err(abort)) => {
                        return self.sync_run_cleanups(frame, cleanups, Err(abort));
                    }
                    SyncStep::Call { request, resume } => {
                        return SyncStep::Call {
                            request,
                            resume: Box::new(move |interpreter, result| {
                                resume(interpreter, result).then(
                                    interpreter,
                                    move |interpreter, outcome| match outcome {
                                        Ok(()) => interpreter.sync_eval_block_from(
                                            frame,
                                            block,
                                            cursor + 1,
                                            cleanups,
                                        ),
                                        Err(abort) => interpreter.sync_run_cleanups(
                                            frame,
                                            cleanups,
                                            Err(abort),
                                        ),
                                    },
                                )
                            }),
                        };
                    }
                },
            }
        }

        let outcome = block.tail.as_deref().map_or_else(
            || SyncStep::complete(Value::Unit),
            |tail| self.sync_eval_expr(frame, tail),
        );
        outcome.then(self, move |interpreter, outcome| {
            interpreter.sync_run_cleanups(frame, cleanups, outcome)
        })
    }

    fn sync_run_cleanups(
        &mut self,
        frame: u64,
        mut cleanups: Vec<SyncCleanup<'program>>,
        mut outcome: Result<Value, EvalAbort>,
    ) -> SyncStep<'program, Value> {
        while let Some(cleanup) = cleanups.pop() {
            let (cleanup, span) = match cleanup {
                SyncCleanup::Deferred(block) => (self.sync_eval_block(frame, block), block.span),
                SyncCleanup::Scoped {
                    local,
                    disposal,
                    span,
                } => (
                    self.sync_eval_scoped_disposal(frame, local, disposal, span),
                    span,
                ),
            };
            match cleanup {
                SyncStep::Complete(cleanup_outcome) => {
                    Self::merge_cleanup_outcome(&mut outcome, cleanup_outcome, span);
                }
                SyncStep::Call { request, resume } => {
                    return SyncStep::Call {
                        request,
                        resume: Box::new(move |interpreter, result| {
                            resume(interpreter, result).then(
                                interpreter,
                                move |interpreter, cleanup_outcome| {
                                    Self::merge_cleanup_outcome(
                                        &mut outcome,
                                        cleanup_outcome,
                                        span,
                                    );
                                    interpreter.sync_run_cleanups(frame, cleanups, outcome)
                                },
                            )
                        }),
                    };
                }
            }
        }
        SyncStep::Complete(outcome)
    }

    fn merge_cleanup_outcome(
        outcome: &mut Result<Value, EvalAbort>,
        cleanup_outcome: Result<Value, EvalAbort>,
        cleanup_span: Span,
    ) {
        match cleanup_outcome {
            Ok(_) => {}
            Err(EvalAbort::Failure(failure)) => {
                if matches!(
                    outcome,
                    Ok(_) | Err(EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue)
                ) {
                    *outcome = Err(EvalAbort::Failure(failure));
                }
            }
            Err(EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue) => {
                if matches!(
                    outcome,
                    Ok(_) | Err(EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue)
                ) {
                    *outcome = Err(EvalAbort::Failure(
                        RuntimeFault {
                            code: "LOOM_RUNTIME_INVALID_MIR".into(),
                            message: "defer cleanup attempted non-local control flow".into(),
                            span: cleanup_span,
                        }
                        .into(),
                    ));
                }
            }
            Err(EvalAbort::Cancelled) => {
                if !matches!(outcome, Err(EvalAbort::Failure(_))) {
                    *outcome = Err(EvalAbort::Cancelled);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn sync_eval_statement(
        &mut self,
        frame: u64,
        statement: &'program Statement,
    ) -> SyncStep<'program, ()> {
        if let Err(failure) = self.tick(statement.span) {
            return SyncStep::fail(failure);
        }
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let local = *local;
                let span = statement.span;
                let value = self.sync_eval_expr(frame, value);
                value.then(self, move |interpreter, outcome| match outcome {
                    Ok(value) => match interpreter.set_slot(frame, local, Slot::Value(value), span)
                    {
                        Ok(()) => SyncStep::complete(()),
                        Err(failure) => SyncStep::fail(failure),
                    },
                    Err(abort) => SyncStep::Complete(Err(abort)),
                })
            }
            StatementKind::LetTuple { locals, value } => {
                let span = statement.span;
                let value = self.sync_eval_expr(frame, value);
                value.then(self, move |interpreter, outcome| match outcome {
                    Ok(value) => match interpreter.bind_tuple(frame, locals, value, span) {
                        Ok(()) => SyncStep::complete(()),
                        Err(failure) => SyncStep::fail(failure),
                    },
                    Err(abort) => SyncStep::Complete(Err(abort)),
                })
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let local = *local;
                let span = statement.span;
                let start = self.sync_eval_expr(frame, start);
                start.and_then(self, move |interpreter, start| {
                    let end_step = interpreter.sync_eval_expr(frame, end);
                    end_step.and_then(interpreter, move |interpreter, end| {
                        let (Value::Int { value: start }, Value::Int { value: end }) =
                            (unrefined(start), unrefined(end))
                        else {
                            return SyncStep::fail(interpreter.runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "range bounds did not produce Int",
                                span,
                            ));
                        };
                        interpreter.sync_eval_for_range(frame, local, start, end, body, span)
                    })
                })
            }
            StatementKind::While { condition, body } => {
                self.sync_eval_while(frame, condition, body, statement.span)
            }
            StatementKind::Break => SyncStep::Complete(Err(EvalAbort::Break)),
            StatementKind::Continue => SyncStep::Complete(Err(EvalAbort::Continue)),
            StatementKind::Assign { place, value } => {
                let span = statement.span;
                let value = self.sync_eval_expr(frame, value);
                value.then(self, move |interpreter, outcome| match outcome {
                    Ok(value) => {
                        let location = Location::from_place(frame, place);
                        match interpreter.write_place(&location, value, span) {
                            Ok(()) => SyncStep::complete(()),
                            Err(failure) => SyncStep::fail(failure),
                        }
                    }
                    Err(abort) => SyncStep::Complete(Err(abort)),
                })
            }
            StatementKind::Assert { condition } => {
                let span = statement.span;
                let condition = self.sync_eval_expr(frame, condition);
                condition.then(self, move |interpreter, outcome| match outcome {
                    Ok(Value::Bool { value: true }) => SyncStep::complete(()),
                    Ok(Value::Bool { value: false }) => SyncStep::fail(ContractFault {
                        code: "AssertionFault".into(),
                        category: ContractFaultKind::Assertion,
                        message: "assertion was not satisfied".into(),
                        contract_span: span,
                        blame_span: span,
                    }),
                    Ok(_) => SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "assert expression did not produce Bool",
                        span,
                    )),
                    Err(abort) => SyncStep::Complete(Err(abort)),
                })
            }
            StatementKind::Evaluate(expression) => {
                let expression = self.sync_eval_expr(frame, expression);
                expression.then(self, |_, outcome| match outcome {
                    Ok(_) => SyncStep::complete(()),
                    Err(abort) => SyncStep::Complete(Err(abort)),
                })
            }
            StatementKind::Return(value) => value.as_ref().map_or_else(
                || SyncStep::Complete(Err(EvalAbort::Return(Box::new(Value::Unit)))),
                |value| {
                    let value = self.sync_eval_expr(frame, value);
                    value.then(self, |_, outcome| match outcome {
                        Ok(value) => SyncStep::Complete(Err(EvalAbort::Return(Box::new(value)))),
                        Err(abort) => SyncStep::Complete(Err(abort)),
                    })
                },
            ),
            StatementKind::Defer(_) | StatementKind::Scoped { .. } => {
                SyncStep::fail(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "cleanup registration escaped block execution",
                    statement.span,
                ))
            }
        }
    }

    fn sync_eval_for_range(
        &mut self,
        frame: u64,
        local: LocalId,
        mut current: i64,
        end: i64,
        body: &'program Block,
        span: Span,
    ) -> SyncStep<'program, ()> {
        while current < end {
            if let Err(failure) = self.set_slot(
                frame,
                local,
                Slot::Value(Value::Int { value: current }),
                span,
            ) {
                return SyncStep::fail(failure);
            }
            let body_step = self.sync_eval_block(frame, body);
            let next = current + 1;
            match body_step {
                SyncStep::Complete(Ok(_) | Err(EvalAbort::Continue)) => current = next,
                SyncStep::Complete(Err(EvalAbort::Break)) => return SyncStep::complete(()),
                SyncStep::Complete(Err(abort)) => return SyncStep::Complete(Err(abort)),
                SyncStep::Call { request, resume } => {
                    return SyncStep::Call {
                        request,
                        resume: Box::new(move |interpreter, outcome| {
                            resume(interpreter, outcome).then(
                                interpreter,
                                move |interpreter, outcome| match outcome {
                                    Ok(_) | Err(EvalAbort::Continue) => interpreter
                                        .sync_eval_for_range(frame, local, next, end, body, span),
                                    Err(EvalAbort::Break) => SyncStep::complete(()),
                                    Err(abort) => SyncStep::Complete(Err(abort)),
                                },
                            )
                        }),
                    };
                }
            }
        }
        SyncStep::complete(())
    }

    fn sync_eval_while(
        &mut self,
        frame: u64,
        condition: &'program Expr,
        body: &'program Block,
        span: Span,
    ) -> SyncStep<'program, ()> {
        loop {
            match self.sync_eval_expr(frame, condition) {
                SyncStep::Complete(Ok(Value::Bool { value: false })) => {
                    return SyncStep::complete(());
                }
                SyncStep::Complete(Ok(Value::Bool { value: true })) => {}
                SyncStep::Complete(Ok(_)) => {
                    return SyncStep::fail(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "while condition did not produce Bool",
                        span,
                    ));
                }
                SyncStep::Complete(Err(abort)) => return SyncStep::Complete(Err(abort)),
                SyncStep::Call { request, resume } => {
                    return SyncStep::Call {
                        request,
                        resume: Box::new(move |interpreter, result| {
                            resume(interpreter, result).then(
                                interpreter,
                                move |interpreter, outcome| match outcome {
                                    Ok(Value::Bool { value: false }) => SyncStep::complete(()),
                                    Ok(Value::Bool { value: true }) => interpreter
                                        .sync_eval_while_body(frame, condition, body, span),
                                    Ok(_) => SyncStep::fail(interpreter.runtime_fault(
                                        "LOOM_RUNTIME_INVALID_MIR",
                                        "while condition did not produce Bool",
                                        span,
                                    )),
                                    Err(abort) => SyncStep::Complete(Err(abort)),
                                },
                            )
                        }),
                    };
                }
            }
            match self.sync_eval_block(frame, body) {
                SyncStep::Complete(Ok(_) | Err(EvalAbort::Continue)) => {}
                SyncStep::Complete(Err(EvalAbort::Break)) => return SyncStep::complete(()),
                SyncStep::Complete(Err(abort)) => return SyncStep::Complete(Err(abort)),
                SyncStep::Call { request, resume } => {
                    return SyncStep::Call {
                        request,
                        resume: Box::new(move |interpreter, result| {
                            resume(interpreter, result).then(
                                interpreter,
                                move |interpreter, outcome| match outcome {
                                    Ok(_) | Err(EvalAbort::Continue) => {
                                        interpreter.sync_eval_while(frame, condition, body, span)
                                    }
                                    Err(EvalAbort::Break) => SyncStep::complete(()),
                                    Err(abort) => SyncStep::Complete(Err(abort)),
                                },
                            )
                        }),
                    };
                }
            }
        }
    }

    fn sync_eval_while_body(
        &mut self,
        frame: u64,
        condition: &'program Expr,
        body: &'program Block,
        span: Span,
    ) -> SyncStep<'program, ()> {
        let body_step = self.sync_eval_block(frame, body);
        body_step.then(self, move |interpreter, outcome| match outcome {
            Ok(_) | Err(EvalAbort::Continue) => {
                interpreter.sync_eval_while(frame, condition, body, span)
            }
            Err(EvalAbort::Break) => SyncStep::complete(()),
            Err(abort) => SyncStep::Complete(Err(abort)),
        })
    }

    fn sync_eval_scoped_disposal(
        &mut self,
        frame: u64,
        local: LocalId,
        disposal: &'program ScopedDisposal,
        span: Span,
    ) -> SyncStep<'program, Value> {
        match disposal {
            ScopedDisposal::FileClose | ScopedDisposal::SocketClose => {
                let builtin = match disposal {
                    ScopedDisposal::FileClose => Builtin::FileClose,
                    ScopedDisposal::SocketClose => Builtin::SocketClose,
                    ScopedDisposal::StaticConcept { .. } => unreachable!(),
                };
                match self.eval_resource_close(
                    frame,
                    builtin,
                    &[CallArgument::InOut(Place::local(local))],
                    span,
                ) {
                    Ok(value) => SyncStep::complete(value),
                    Err(abort) => SyncStep::Complete(Err(abort)),
                }
            }
            ScopedDisposal::StaticConcept {
                requirement,
                witness,
                ..
            } => {
                let runtime_witness = match self.resolve_witness(frame, witness, span) {
                    Ok(witness) => witness,
                    Err(abort) => return SyncStep::Complete(Err(abort)),
                };
                let Some(definition) = self.program.witness(runtime_witness.definition) else {
                    return SyncStep::fail(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "scoped disposal references an unknown witness",
                        span,
                    ));
                };
                let Some(function) = definition.methods.get(requirement).copied() else {
                    return SyncStep::fail(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "scoped disposal witness is missing Dispose.dispose",
                        span,
                    ));
                };
                SyncStep::call(SyncCall {
                    function,
                    arguments: vec![BoundArgument::Alias(Location::local(frame, local))],
                    witnesses: runtime_witness.arguments,
                    span,
                })
            }
        }
    }

    fn eval_block(&mut self, frame: u64, block: &Block) -> Result<Value, EvalAbort> {
        self.tick(block.span).map_err(EvalAbort::from)?;
        let mut cleanups = Vec::new();
        let outcome = (|| {
            for statement in &block.statements {
                match &statement.kind {
                    StatementKind::Defer(cleanup) => {
                        cleanups.push(RuntimeCleanup::Deferred(cleanup.clone()));
                    }
                    StatementKind::Scoped {
                        local,
                        value,
                        disposal,
                    } => {
                        let value = self.eval_expr(frame, value)?;
                        self.set_slot(frame, *local, Slot::Value(value), statement.span)
                            .map_err(EvalAbort::from)?;
                        cleanups.push(RuntimeCleanup::Scoped {
                            local: *local,
                            disposal: disposal.clone(),
                            span: statement.span,
                        });
                    }
                    _ => self.eval_statement(frame, statement)?,
                }
            }
            block
                .tail
                .as_deref()
                .map_or_else(|| Ok(Value::Unit), |tail| self.eval_expr(frame, tail))
        })();
        self.run_cleanups(frame, cleanups, outcome)
    }

    fn run_cleanups(
        &mut self,
        frame: u64,
        cleanups: Vec<RuntimeCleanup>,
        mut outcome: Result<Value, EvalAbort>,
    ) -> Result<Value, EvalAbort> {
        for cleanup in cleanups.into_iter().rev() {
            let (cleanup_outcome, cleanup_span) = match cleanup {
                RuntimeCleanup::Deferred(block) => {
                    let span = block.span;
                    (self.eval_block(frame, &block), span)
                }
                RuntimeCleanup::Scoped {
                    local,
                    disposal,
                    span,
                } => (
                    self.eval_scoped_disposal(frame, local, &disposal, span),
                    span,
                ),
            };
            match cleanup_outcome {
                Ok(_) => {}
                Err(EvalAbort::Failure(failure)) => {
                    if matches!(
                        &outcome,
                        Ok(_) | Err(EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue)
                    ) {
                        outcome = Err(EvalAbort::Failure(failure));
                    }
                }
                Err(EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue) => {
                    if matches!(
                        &outcome,
                        Ok(_) | Err(EvalAbort::Return(_) | EvalAbort::Break | EvalAbort::Continue)
                    ) {
                        outcome = Err(EvalAbort::from(self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "defer cleanup attempted non-local control flow",
                            cleanup_span,
                        )));
                    }
                }
                Err(EvalAbort::Cancelled) => {
                    if !matches!(&outcome, Err(EvalAbort::Failure(_))) {
                        outcome = Err(EvalAbort::Cancelled);
                    }
                }
            }
        }
        outcome
    }

    #[expect(
        clippy::too_many_lines,
        reason = "statement evaluation keeps the complete MIR statement dispatch and control-flow aborts together"
    )]
    fn eval_statement(&mut self, frame: u64, statement: &Statement) -> Result<(), EvalAbort> {
        self.tick(statement.span).map_err(EvalAbort::from)?;
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let value = self.eval_expr(frame, value)?;
                self.set_slot(frame, *local, Slot::Value(value), statement.span)
                    .map_err(EvalAbort::from)?;
                Ok(())
            }
            StatementKind::LetTuple { locals, value } => {
                let value = self.eval_expr(frame, value)?;
                self.bind_tuple(frame, locals, value, statement.span)
                    .map_err(EvalAbort::from)
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let start = self.eval_expr(frame, start)?;
                let end = self.eval_expr(frame, end)?;
                let (Value::Int { value: mut current }, Value::Int { value: end }) =
                    (unrefined(start), unrefined(end))
                else {
                    return Err(EvalAbort::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "range bounds did not produce Int",
                        statement.span,
                    )));
                };
                while current < end {
                    self.set_slot(
                        frame,
                        *local,
                        Slot::Value(Value::Int { value: current }),
                        statement.span,
                    )
                    .map_err(EvalAbort::from)?;
                    match self.eval_block(frame, body) {
                        Ok(_) | Err(EvalAbort::Continue) => {}
                        Err(EvalAbort::Break) => break,
                        Err(abort) => return Err(abort),
                    }
                    // `current < end` means current cannot be i64::MAX.
                    current += 1;
                }
                Ok(())
            }
            StatementKind::While { condition, body } => {
                loop {
                    match self.eval_expr(frame, condition)? {
                        Value::Bool { value: false } => break,
                        Value::Bool { value: true } => {}
                        _ => {
                            return Err(EvalAbort::from(self.runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "while condition did not produce Bool",
                                statement.span,
                            )));
                        }
                    }
                    match self.eval_block(frame, body) {
                        Ok(_) | Err(EvalAbort::Continue) => {}
                        Err(EvalAbort::Break) => break,
                        Err(abort) => return Err(abort),
                    }
                }
                Ok(())
            }
            StatementKind::Break => Err(EvalAbort::Break),
            StatementKind::Continue => Err(EvalAbort::Continue),
            StatementKind::Assign { place, value } => {
                let value = self.eval_expr(frame, value)?;
                let location = Location::from_place(frame, place);
                self.write_place(&location, value, statement.span)
                    .map_err(EvalAbort::from)?;
                Ok(())
            }
            StatementKind::Assert { condition } => {
                let value = self.eval_expr(frame, condition)?;
                match value {
                    Value::Bool { value: true } => Ok(()),
                    Value::Bool { value: false } => Err(EvalAbort::from(ContractFault {
                        code: "AssertionFault".into(),
                        category: ContractFaultKind::Assertion,
                        message: "assertion was not satisfied".into(),
                        contract_span: statement.span,
                        blame_span: statement.span,
                    })),
                    _ => Err(EvalAbort::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "assert expression did not produce Bool",
                        statement.span,
                    ))),
                }
            }
            StatementKind::Evaluate(expression) => {
                self.eval_expr(frame, expression)?;
                Ok(())
            }
            StatementKind::Defer(_) | StatementKind::Scoped { .. } => {
                Err(EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "cleanup registration escaped block execution",
                    statement.span,
                )))
            }
            StatementKind::Return(value) => {
                let value = value
                    .as_ref()
                    .map_or_else(|| Ok(Value::Unit), |value| self.eval_expr(frame, value))?;
                Err(EvalAbort::Return(Box::new(value)))
            }
        }
    }

    fn bind_tuple(
        &mut self,
        frame: u64,
        locals: &[LocalId],
        value: Value,
        span: Span,
    ) -> Result<(), ExecutionFailure> {
        let Value::Tuple { elements } = value else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "tuple binding received a non-tuple value",
                    span,
                )
                .into());
        };
        if elements.len() != locals.len() {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "tuple binding arity does not match its value",
                    span,
                )
                .into());
        }
        for (local, element) in locals.iter().zip(elements) {
            self.set_slot(frame, *local, Slot::Value(element), span)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn sync_eval_expr(
        &mut self,
        frame: u64,
        expression: &'program Expr,
    ) -> SyncStep<'program, Value> {
        if let Err(failure) = self.tick(expression.span) {
            return SyncStep::fail(failure);
        }
        match &expression.kind {
            ExprKind::Constant(value) => SyncStep::complete(Value::from(value.clone())),
            ExprKind::Tuple(elements) => {
                let elements = self.sync_eval_expr_sequence(frame, elements, 0, Vec::new());
                elements.and_then(self, |_, elements| {
                    SyncStep::complete(Value::Tuple { elements })
                })
            }
            ExprKind::List(elements) => {
                let elements = self.sync_eval_expr_sequence(frame, elements, 0, Vec::new());
                elements.and_then(self, |_, elements| {
                    SyncStep::complete(Value::List { elements })
                })
            }
            ExprKind::Copy(place) => {
                match self.read_place(&Location::from_place(frame, place), expression.span) {
                    Ok(value) => SyncStep::complete(owned_value_clone(&value)),
                    Err(failure) => SyncStep::fail(failure),
                }
            }
            ExprKind::Move(place) => {
                match self.take_place(&Location::from_place(frame, place), expression.span) {
                    Ok(value) => SyncStep::complete(value),
                    Err(failure) => SyncStep::fail(failure),
                }
            }
            ExprKind::Unary(operator, value) => {
                let operator = *operator;
                let span = expression.span;
                let value = self.sync_eval_expr(frame, value);
                value.and_then(self, move |interpreter, value| {
                    match interpreter.eval_unary(operator, value, span) {
                        Ok(value) => SyncStep::complete(value),
                        Err(failure) => SyncStep::fail(failure),
                    }
                })
            }
            ExprKind::Binary(BinaryOp::And, left, right) => {
                let span = expression.span;
                let left = self.sync_eval_expr(frame, left);
                left.and_then(self, move |interpreter, left| {
                    let left = match expect_bool(&left, span) {
                        Ok(left) => left,
                        Err(failure) => return SyncStep::fail(failure),
                    };
                    if !left {
                        return SyncStep::complete(Value::Bool { value: false });
                    }
                    let right = interpreter.sync_eval_expr(frame, right);
                    right.and_then(interpreter, move |_, right| {
                        match expect_bool(&right, span) {
                            Ok(value) => SyncStep::complete(Value::Bool { value }),
                            Err(failure) => SyncStep::fail(failure),
                        }
                    })
                })
            }
            ExprKind::Binary(BinaryOp::Or, left, right) => {
                let span = expression.span;
                let left = self.sync_eval_expr(frame, left);
                left.and_then(self, move |interpreter, left| {
                    let left = match expect_bool(&left, span) {
                        Ok(left) => left,
                        Err(failure) => return SyncStep::fail(failure),
                    };
                    if left {
                        return SyncStep::complete(Value::Bool { value: true });
                    }
                    let right = interpreter.sync_eval_expr(frame, right);
                    right.and_then(interpreter, move |_, right| {
                        match expect_bool(&right, span) {
                            Ok(value) => SyncStep::complete(Value::Bool { value }),
                            Err(failure) => SyncStep::fail(failure),
                        }
                    })
                })
            }
            ExprKind::Binary(operator, left, right) => {
                let operator = *operator;
                let span = expression.span;
                let left = self.sync_eval_expr(frame, left);
                left.and_then(self, move |interpreter, left| {
                    let right = interpreter.sync_eval_expr(frame, right);
                    right.and_then(interpreter, move |interpreter, right| {
                        match interpreter.eval_binary(operator, left, right, span) {
                            Ok(value) => SyncStep::complete(value),
                            Err(failure) => SyncStep::fail(failure),
                        }
                    })
                })
            }
            ExprKind::Block(block) => self.sync_eval_block(frame, block),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let span = expression.span;
                let condition = self.sync_eval_expr(frame, condition);
                condition.and_then(self, move |interpreter, condition| {
                    let condition = match expect_bool(&condition, span) {
                        Ok(condition) => condition,
                        Err(failure) => return SyncStep::fail(failure),
                    };
                    interpreter
                        .sync_eval_block(frame, if condition { then_branch } else { else_branch })
                })
            }
            ExprKind::Match { scrutinee, arms } => {
                let span = expression.span;
                let scrutinee = self.sync_eval_expr(frame, scrutinee);
                scrutinee.and_then(self, move |interpreter, value| {
                    interpreter.sync_eval_match(frame, &value, arms, span)
                })
            }
            ExprKind::Record {
                ty,
                fields,
                construction,
                ..
            } => {
                let ty = *ty;
                let construction = *construction;
                let span = expression.span;
                let fields = self.sync_eval_expr_sequence(frame, fields, 0, Vec::new());
                fields.and_then(self, move |interpreter, fields| {
                    let value = Value::Record { ty, fields };
                    let value = match construction {
                        ConstructionMode::Runtime => interpreter.checked_record(ty, value, span),
                        ConstructionMode::Recheck => interpreter.rechecked_record(ty, value, span),
                        ConstructionMode::Plain | ConstructionMode::Proven => Ok(value),
                    };
                    match value {
                        Ok(value) => SyncStep::complete(value),
                        Err(failure) => SyncStep::fail(failure),
                    }
                })
            }
            ExprKind::Variant {
                ty,
                variant,
                payload,
                ..
            } => {
                let ty = *ty;
                let variant = *variant;
                let payload = self.sync_eval_expr_sequence(frame, payload, 0, Vec::new());
                payload.and_then(self, move |_, payload| {
                    SyncStep::complete(Value::Enum {
                        ty,
                        variant,
                        payload,
                    })
                })
            }
            ExprKind::Refine {
                ty,
                value,
                construction,
            } => {
                let ty = *ty;
                let construction = *construction;
                let span = expression.span;
                let value = self.sync_eval_expr(frame, value);
                value.and_then(self, move |interpreter, value| {
                    let value = match construction {
                        ConstructionMode::Runtime => interpreter.checked_refine(ty, value, span),
                        ConstructionMode::Recheck => interpreter.rechecked_refine(ty, value, span),
                        ConstructionMode::Plain | ConstructionMode::Proven => Ok(Value::Refined {
                            ty,
                            value: Box::new(value),
                        }),
                    };
                    match value {
                        Ok(value) => SyncStep::complete(value),
                        Err(failure) => SyncStep::fail(failure),
                    }
                })
            }
            ExprKind::Unrefine(value) => {
                let span = expression.span;
                let value = self.sync_eval_expr(frame, value);
                value.and_then(self, move |interpreter, value| {
                    let Value::Refined { value, .. } = value else {
                        return SyncStep::fail(interpreter.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "unrefine operand is not a constrained value",
                            span,
                        ));
                    };
                    SyncStep::complete(*value)
                })
            }
            ExprKind::Call {
                target,
                arguments,
                witnesses,
                ..
            } => self.sync_eval_call(frame, target, arguments, witnesses, expression.span),
            ExprKind::MakeView {
                value,
                writeback,
                witness,
                mutable,
                token,
            } => {
                let span = expression.span;
                let mutable = *mutable;
                let token = *token;
                let value = self.sync_eval_expr(frame, value);
                value.and_then(self, move |interpreter, value| {
                    let witness = match interpreter.resolve_witness(frame, witness, span) {
                        Ok(witness) => witness,
                        Err(abort) => return SyncStep::Complete(Err(abort)),
                    };
                    SyncStep::complete(Value::DynView {
                        value: Box::new(value),
                        writeback: writeback
                            .as_ref()
                            .map(|owner| Location::from_place(frame, owner)),
                        witness,
                        mutable,
                        token,
                    })
                })
            }
            ExprKind::ReborrowView {
                owner,
                mutable,
                token,
            } => {
                let location = Location::from_place(frame, owner);
                match self.read_place(&location, expression.span) {
                    Ok(Value::DynView { value, witness, .. }) => {
                        SyncStep::complete(Value::DynView {
                            value,
                            writeback: Some(location),
                            witness,
                            mutable: *mutable,
                            token: *token,
                        })
                    }
                    Ok(_) => SyncStep::fail(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "interface reborrow source is not an erased interface",
                        expression.span,
                    )),
                    Err(failure) => SyncStep::fail(failure),
                }
            }
            ExprKind::Await { .. } => match self.eval_nested_await(frame, expression) {
                Ok(value) => SyncStep::complete(value),
                Err(abort) => SyncStep::Complete(Err(abort)),
            },
            ExprKind::Sleep { milliseconds } => {
                let span = expression.span;
                let duration = self.sync_eval_expr(frame, milliseconds);
                duration.and_then(self, move |interpreter, duration| {
                    interpreter.sync_eval_sleep(duration, span)
                })
            }
            ExprKind::TaskJoin { mode, arguments } => {
                let mode = *mode;
                let span = expression.span;
                let values = self.sync_eval_expr_sequence(frame, arguments, 0, Vec::new());
                values.and_then(self, move |interpreter, values| {
                    interpreter.sync_eval_task_join(mode, values, span)
                })
            }
        }
    }

    fn sync_eval_expr_sequence(
        &mut self,
        frame: u64,
        expressions: &'program [Expr],
        mut cursor: usize,
        mut values: Vec<Value>,
    ) -> SyncStep<'program, Vec<Value>> {
        while let Some(expression) = expressions.get(cursor) {
            let step = self.sync_eval_expr(frame, expression);
            match step {
                SyncStep::Complete(Ok(value)) => {
                    values.push(value);
                    cursor += 1;
                }
                SyncStep::Complete(Err(abort)) => return SyncStep::Complete(Err(abort)),
                SyncStep::Call { request, resume } => {
                    return SyncStep::Call {
                        request,
                        resume: Box::new(move |interpreter, outcome| {
                            resume(interpreter, outcome).and_then(
                                interpreter,
                                move |interpreter, value| {
                                    values.push(value);
                                    interpreter.sync_eval_expr_sequence(
                                        frame,
                                        expressions,
                                        cursor + 1,
                                        values,
                                    )
                                },
                            )
                        }),
                    };
                }
            }
        }
        SyncStep::complete(values)
    }

    fn sync_eval_match(
        &mut self,
        frame: u64,
        value: &Value,
        arms: &'program [MatchArm],
        span: Span,
    ) -> SyncStep<'program, Value> {
        for arm in arms {
            let mut bindings = Vec::new();
            if pattern_matches(&arm.pattern, value, &mut bindings) {
                if bindings.len() != arm.bindings.len() {
                    return SyncStep::fail(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "match binding count does not match pattern",
                        span,
                    ));
                }
                for (local, value) in arm.bindings.iter().zip(bindings) {
                    if let Err(failure) = self.set_slot(frame, *local, Slot::Value(value), span) {
                        return SyncStep::fail(failure);
                    }
                }
                return self.sync_eval_expr(frame, &arm.value);
            }
        }
        SyncStep::fail(self.runtime_fault(
            "LOOM_RUNTIME_NON_EXHAUSTIVE_MATCH",
            "checked MIR reached a non-exhaustive match",
            span,
        ))
    }

    fn sync_eval_sleep(&mut self, duration: Value, span: Span) -> SyncStep<'program, Value> {
        let milliseconds = match duration {
            Value::Int { value } => value,
            Value::Record { ty, fields } if self.program.prelude.duration == Some(ty) => {
                match record_descriptor(&fields, span) {
                    Ok(value) => value,
                    Err(failure) => return SyncStep::fail(failure),
                }
            }
            _ => {
                return SyncStep::fail(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "Task.sleep duration did not produce Int or Duration",
                    span,
                ));
            }
        };
        if milliseconds < 0 {
            return SyncStep::fail(self.runtime_fault(
                INVALID_SLEEP_DURATION_FAULT_CODE,
                INVALID_SLEEP_DURATION_FAULT_MESSAGE,
                span,
            ));
        }
        let Some(nanoseconds) = milliseconds.checked_mul(1_000_000) else {
            return SyncStep::fail(self.runtime_fault(
                SLEEP_DURATION_OVERFLOW_FAULT_CODE,
                SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
                span,
            ));
        };
        let Some(deadline) =
            Instant::now().checked_add(Duration::from_nanos(nanoseconds.cast_unsigned()))
        else {
            return SyncStep::fail(self.runtime_fault(
                SLEEP_DURATION_OVERFLOW_FAULT_CODE,
                SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
                span,
            ));
        };
        let task_id = self.next_task;
        let Some(next_task) = self.next_task.checked_add(1) else {
            return SyncStep::fail(self.runtime_fault(
                "TaskIdExhausted",
                "async task identity space was exhausted",
                span,
            ));
        };
        self.next_task = next_task;
        self.tasks.insert(
            task_id,
            ManagedTask {
                function: FunctionId(u32::MAX),
                frame: u64::MAX,
                parent: self.active_task,
                children: Vec::new(),
                cursor: 0,
                awaiting_state: None,
                cleanups: Vec::new(),
                status: TaskStatus::Runnable,
                queued: false,
                marked: false,
                timer_deadline: Some(deadline),
                host_io: false,
                contract_state: None,
                join_mode: TaskJoinMode::All,
                join_dynamic: false,
                join_combined: false,
                join_winner: None,
                cancel_requested: false,
            },
        );
        self.enqueue_task(task_id);
        self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
        self.gc_stats.live = self.tasks.len() as u64;
        SyncStep::complete(Value::Task { id: task_id })
    }

    fn sync_eval_task_join(
        &mut self,
        mode: TaskJoinMode,
        values: Vec<Value>,
        span: Span,
    ) -> SyncStep<'program, Value> {
        let (values, dynamic) = match values.as_slice() {
            [Value::List { elements }] => (elements.clone(), true),
            _ => (values, false),
        };
        let mut tasks = Vec::with_capacity(values.len());
        for value in values {
            let Value::Task { id } = value else {
                return SyncStep::fail(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "Task join contained a non-task value",
                    span,
                ));
            };
            tasks.push(id);
        }
        SyncStep::complete(Value::TaskJoin {
            mode,
            tasks,
            dynamic,
        })
    }

    fn sync_eval_call(
        &mut self,
        frame: u64,
        target: &'program CallTarget,
        arguments: &'program [CallArgument],
        witnesses: &'program [WitnessRef],
        span: Span,
    ) -> SyncStep<'program, Value> {
        match target {
            CallTarget::Builtin(builtin) => {
                self.sync_eval_builtin_call(frame, *builtin, arguments, span)
            }
            CallTarget::Dynamic { requirement } => {
                self.sync_eval_dynamic_call(frame, *requirement, arguments, span)
            }
            CallTarget::StaticConcept {
                requirement,
                witness,
                ..
            } => self.sync_eval_static_concept_call(
                frame,
                *requirement,
                witness,
                arguments,
                witnesses,
                span,
            ),
            CallTarget::Direct(function) | CallTarget::Inherent(function) => {
                let function = *function;
                let values = self.sync_eval_bound_arguments(frame, arguments, 0, Vec::new());
                values.and_then(self, move |interpreter, values| {
                    let witness_values = witnesses
                        .iter()
                        .map(|witness| interpreter.resolve_witness(frame, witness, span))
                        .collect::<Result<Vec<_>, _>>();
                    match witness_values {
                        Ok(witness_values) => SyncStep::call(SyncCall {
                            function,
                            arguments: values,
                            witnesses: witness_values,
                            span,
                        }),
                        Err(abort) => SyncStep::Complete(Err(abort)),
                    }
                })
            }
        }
    }

    fn sync_eval_bound_arguments(
        &mut self,
        frame: u64,
        arguments: &'program [CallArgument],
        mut cursor: usize,
        mut values: Vec<BoundArgument>,
    ) -> SyncStep<'program, Vec<BoundArgument>> {
        while let Some(argument) = arguments.get(cursor) {
            match argument {
                CallArgument::InOut(place) => {
                    values.push(BoundArgument::Alias(Location::from_place(frame, place)));
                    cursor += 1;
                }
                CallArgument::Value(expression) => {
                    let step = self.sync_eval_expr(frame, expression);
                    match step {
                        SyncStep::Complete(Ok(value)) => {
                            values.push(BoundArgument::Value(value));
                            cursor += 1;
                        }
                        SyncStep::Complete(Err(abort)) => {
                            return SyncStep::Complete(Err(abort));
                        }
                        SyncStep::Call { request, resume } => {
                            return SyncStep::Call {
                                request,
                                resume: Box::new(move |interpreter, outcome| {
                                    resume(interpreter, outcome).and_then(
                                        interpreter,
                                        move |interpreter, value| {
                                            values.push(BoundArgument::Value(value));
                                            interpreter.sync_eval_bound_arguments(
                                                frame,
                                                arguments,
                                                cursor + 1,
                                                values,
                                            )
                                        },
                                    )
                                }),
                            };
                        }
                    }
                }
            }
        }
        SyncStep::complete(values)
    }

    fn sync_eval_builtin_values(
        &mut self,
        frame: u64,
        arguments: &'program [CallArgument],
        mut cursor: usize,
        mut values: Vec<Value>,
        span: Span,
    ) -> SyncStep<'program, Vec<Value>> {
        while let Some(argument) = arguments.get(cursor) {
            match argument {
                CallArgument::InOut(place) => {
                    match self.read_place(&Location::from_place(frame, place), span) {
                        Ok(value) => values.push(value),
                        Err(failure) => return SyncStep::fail(failure),
                    }
                    cursor += 1;
                }
                CallArgument::Value(expression) => {
                    let step = self.sync_eval_expr(frame, expression);
                    match step {
                        SyncStep::Complete(Ok(value)) => {
                            values.push(value);
                            cursor += 1;
                        }
                        SyncStep::Complete(Err(abort)) => {
                            return SyncStep::Complete(Err(abort));
                        }
                        SyncStep::Call { request, resume } => {
                            return SyncStep::Call {
                                request,
                                resume: Box::new(move |interpreter, outcome| {
                                    resume(interpreter, outcome).and_then(
                                        interpreter,
                                        move |interpreter, value| {
                                            values.push(value);
                                            interpreter.sync_eval_builtin_values(
                                                frame,
                                                arguments,
                                                cursor + 1,
                                                values,
                                                span,
                                            )
                                        },
                                    )
                                }),
                            };
                        }
                    }
                }
            }
        }
        SyncStep::complete(values)
    }

    fn sync_eval_builtin_call(
        &mut self,
        frame: u64,
        builtin: Builtin,
        arguments: &'program [CallArgument],
        span: Span,
    ) -> SyncStep<'program, Value> {
        if builtin == Builtin::ListAdd {
            let [CallArgument::InOut(place), CallArgument::Value(value)] = arguments else {
                return SyncStep::fail(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "List.add has an invalid checked argument shape",
                    span,
                ));
            };
            let value = self.sync_eval_expr(frame, value);
            return value.and_then(self, move |interpreter, value| {
                let location = Location::from_place(frame, place);
                let list = match interpreter.read_place(&location, span) {
                    Ok(value) => value,
                    Err(failure) => return SyncStep::fail(failure),
                };
                let Value::List { mut elements } = unrefined(list) else {
                    return SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "List.add receiver is not a List",
                        span,
                    ));
                };
                elements.push(value);
                match interpreter.write_place(&location, Value::List { elements }, span) {
                    Ok(()) => SyncStep::complete(Value::Unit),
                    Err(failure) => SyncStep::fail(failure),
                }
            });
        }
        if matches!(builtin, Builtin::FileClose | Builtin::SocketClose) {
            return match self.eval_resource_close(frame, builtin, arguments, span) {
                Ok(value) => SyncStep::complete(value),
                Err(abort) => SyncStep::Complete(Err(abort)),
            };
        }
        let values = self.sync_eval_builtin_values(frame, arguments, 0, Vec::new(), span);
        values.and_then(self, move |interpreter, values| {
            match interpreter.eval_builtin(builtin, &values, span) {
                Ok(value) => SyncStep::complete(value),
                Err(failure) => SyncStep::fail(failure),
            }
        })
    }

    fn sync_eval_static_concept_call(
        &mut self,
        frame: u64,
        requirement: RequirementId,
        witness_ref: &'program WitnessRef,
        arguments: &'program [CallArgument],
        method_witnesses: &'program [WitnessRef],
        span: Span,
    ) -> SyncStep<'program, Value> {
        let runtime_witness = match self.resolve_witness(frame, witness_ref, span) {
            Ok(witness) => witness,
            Err(abort) => return SyncStep::Complete(Err(abort)),
        };
        let Some(witness) = self.program.witness(runtime_witness.definition) else {
            return SyncStep::fail(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "static concept call references an unknown witness",
                span,
            ));
        };
        let Some(function) = witness.methods.get(&requirement).copied() else {
            return SyncStep::fail(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "static witness is missing a required method",
                span,
            ));
        };
        let values = self.sync_eval_bound_arguments(frame, arguments, 0, Vec::new());
        values.and_then(self, move |interpreter, values| {
            let mut proof_arguments = runtime_witness.arguments;
            let method_arguments = method_witnesses
                .iter()
                .map(|witness| interpreter.resolve_witness(frame, witness, span))
                .collect::<Result<Vec<_>, _>>();
            match method_arguments {
                Ok(method_arguments) => {
                    proof_arguments.extend(method_arguments);
                    SyncStep::call(SyncCall {
                        function,
                        arguments: values,
                        witnesses: proof_arguments,
                        span,
                    })
                }
                Err(abort) => SyncStep::Complete(Err(abort)),
            }
        })
    }

    fn sync_eval_dynamic_call(
        &mut self,
        frame: u64,
        requirement: RequirementId,
        arguments: &'program [CallArgument],
        span: Span,
    ) -> SyncStep<'program, Value> {
        let Some(first) = arguments.first() else {
            return SyncStep::fail(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "dynamic call is missing its receiver",
                span,
            ));
        };
        let receiver = match first {
            CallArgument::Value(value) => self.sync_eval_expr(frame, value),
            CallArgument::InOut(place) => {
                match self.read_place(&Location::from_place(frame, place), span) {
                    Ok(value) => SyncStep::complete(value),
                    Err(failure) => SyncStep::fail(failure),
                }
            }
        };
        receiver.and_then(self, move |interpreter, receiver| {
            let Value::DynView {
                value,
                writeback,
                witness,
                mutable,
                ..
            } = receiver
            else {
                return SyncStep::fail(interpreter.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "dynamic call receiver is not an erased interface",
                    span,
                ));
            };
            let Some(witness_definition) = interpreter.program.witness(witness.definition) else {
                return SyncStep::fail(interpreter.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "erased interface references an unknown witness",
                    span,
                ));
            };
            let Some(function) = witness_definition.methods.get(&requirement).copied() else {
                return SyncStep::fail(interpreter.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "dynamic witness is missing a required method",
                    span,
                ));
            };
            let Some(receiver_kind) = interpreter
                .program
                .function(function)
                .map(|function| function.receiver)
            else {
                return SyncStep::fail(interpreter.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "dynamic witness method does not exist",
                    span,
                ));
            };
            if receiver_kind == Some(Receiver::Mutable) {
                if !mutable {
                    return SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "a readonly interface argument dispatched a `mut self` method",
                        span,
                    ));
                }
                let CallArgument::InOut(place) = first else {
                    return SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "mutable dynamic receiver is not an inout place",
                        span,
                    ));
                };
                return interpreter.sync_invoke_mutable_dynamic(
                    frame,
                    Location::from_place(frame, place),
                    *value,
                    writeback,
                    witness,
                    function,
                    &arguments[1..],
                    span,
                );
            }
            let mut values = vec![BoundArgument::Value(*value)];
            let trailing =
                interpreter.sync_eval_bound_arguments(frame, &arguments[1..], 0, Vec::new());
            trailing.and_then(interpreter, move |_, trailing| {
                values.extend(trailing);
                SyncStep::call(SyncCall {
                    function,
                    arguments: values,
                    witnesses: witness.arguments,
                    span,
                })
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_invoke_mutable_dynamic(
        &mut self,
        frame: u64,
        carrier: Location,
        value: Value,
        writeback: Option<Location>,
        witness: RuntimeWitness,
        function: FunctionId,
        arguments: &'program [CallArgument],
        span: Span,
    ) -> SyncStep<'program, Value> {
        let trailing = self.sync_eval_bound_arguments(frame, arguments, 0, Vec::new());
        trailing.and_then(self, move |interpreter, trailing| {
            let temporary_frame = interpreter.next_frame;
            let Some(next_frame) = interpreter.next_frame.checked_add(1) else {
                return SyncStep::fail(interpreter.runtime_fault(
                    "LOOM_RUNTIME_FRAME_OVERFLOW",
                    "interpreter frame identifier space was exhausted",
                    span,
                ));
            };
            interpreter.next_frame = next_frame;
            interpreter.frames.insert(
                temporary_frame,
                Frame {
                    slots: vec![Slot::Value(value)],
                    witnesses: Vec::new(),
                },
            );
            let temporary = Location::local(temporary_frame, LocalId(0));
            let mut values = vec![BoundArgument::Alias(temporary.clone())];
            values.extend(trailing);
            let call = SyncStep::call(SyncCall {
                function,
                arguments: values,
                witnesses: witness.arguments.clone(),
                span,
            });
            call.then(interpreter, move |interpreter, outcome| {
                let mutated = interpreter.read_place(&temporary, span);
                interpreter.frames.remove(&temporary_frame);
                let mutated = match mutated {
                    Ok(mutated) => mutated,
                    Err(failure) => return SyncStep::fail(failure),
                };
                let current = match interpreter.read_place(&carrier, span) {
                    Ok(current) => current,
                    Err(failure) => return SyncStep::fail(failure),
                };
                let Value::DynView { mutable, token, .. } = current else {
                    return SyncStep::fail(interpreter.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "mutable dynamic receiver changed representation during its call",
                        span,
                    ));
                };
                if let Err(failure) = interpreter.write_place(
                    &carrier,
                    Value::DynView {
                        value: Box::new(mutated.clone()),
                        writeback: writeback.clone(),
                        witness,
                        mutable,
                        token,
                    },
                    span,
                ) {
                    return SyncStep::fail(failure);
                }
                if let Some(writeback) = writeback
                    && let Err(failure) =
                        interpreter.propagate_interface_writeback(writeback, mutated, span)
                {
                    return SyncStep::fail(failure);
                }
                SyncStep::Complete(outcome)
            })
        })
    }

    #[allow(clippy::too_many_lines)]
    fn eval_expr(&mut self, frame: u64, expression: &Expr) -> Result<Value, EvalAbort> {
        self.tick(expression.span).map_err(EvalAbort::from)?;
        match &expression.kind {
            ExprKind::Constant(value) => Ok(Value::from(value.clone())),
            ExprKind::Tuple(elements) => Ok(Value::Tuple {
                elements: elements
                    .iter()
                    .map(|element| self.eval_expr(frame, element))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            ExprKind::List(elements) => Ok(Value::List {
                elements: elements
                    .iter()
                    .map(|element| self.eval_expr(frame, element))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            ExprKind::Copy(place) => {
                let value =
                    self.read_place(&Location::from_place(frame, place), expression.span)?;
                Ok(owned_value_clone(&value))
            }
            ExprKind::Move(place) => {
                Ok(self.take_place(&Location::from_place(frame, place), expression.span)?)
            }
            ExprKind::Unary(operator, value) => {
                let value = self.eval_expr(frame, value)?;
                Ok(self.eval_unary(*operator, value, expression.span)?)
            }
            ExprKind::Binary(BinaryOp::And, left, right) => {
                let left = self.eval_expr(frame, left)?;
                if expect_bool(&left, expression.span)? {
                    let right = self.eval_expr(frame, right)?;
                    Ok(Value::Bool {
                        value: expect_bool(&right, expression.span)?,
                    })
                } else {
                    Ok(Value::Bool { value: false })
                }
            }
            ExprKind::Binary(BinaryOp::Or, left, right) => {
                let left = self.eval_expr(frame, left)?;
                if expect_bool(&left, expression.span)? {
                    Ok(Value::Bool { value: true })
                } else {
                    let right = self.eval_expr(frame, right)?;
                    Ok(Value::Bool {
                        value: expect_bool(&right, expression.span)?,
                    })
                }
            }
            ExprKind::Binary(operator, left, right) => {
                let left = self.eval_expr(frame, left)?;
                let right = self.eval_expr(frame, right)?;
                Ok(self.eval_binary(*operator, left, right, expression.span)?)
            }
            ExprKind::Block(block) => self.eval_block(frame, block),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.eval_expr(frame, condition)?;
                let branch = if expect_bool(&condition, expression.span)? {
                    then_branch
                } else {
                    else_branch
                };
                self.eval_block(frame, branch)
            }
            ExprKind::Match { scrutinee, arms } => {
                let value = self.eval_expr(frame, scrutinee)?;
                self.eval_match(frame, &value, arms, expression.span)
            }
            ExprKind::Record {
                ty,
                fields,
                construction,
                ..
            } => {
                let fields = fields
                    .iter()
                    .map(|field| self.eval_expr(frame, field))
                    .collect::<Result<Vec<_>, _>>()?;
                let value = Value::Record { ty: *ty, fields };
                match construction {
                    ConstructionMode::Runtime => {
                        Ok(self.checked_record(*ty, value, expression.span)?)
                    }
                    ConstructionMode::Recheck => {
                        Ok(self.rechecked_record(*ty, value, expression.span)?)
                    }
                    ConstructionMode::Plain | ConstructionMode::Proven => Ok(value),
                }
            }
            ExprKind::Variant {
                ty,
                variant,
                payload,
                ..
            } => Ok(Value::Enum {
                ty: *ty,
                variant: *variant,
                payload: payload
                    .iter()
                    .map(|value| self.eval_expr(frame, value))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            ExprKind::Refine {
                ty,
                value,
                construction,
            } => {
                let value = self.eval_expr(frame, value)?;
                match construction {
                    ConstructionMode::Runtime => {
                        Ok(self.checked_refine(*ty, value, expression.span)?)
                    }
                    ConstructionMode::Recheck => {
                        Ok(self.rechecked_refine(*ty, value, expression.span)?)
                    }
                    ConstructionMode::Plain | ConstructionMode::Proven => Ok(Value::Refined {
                        ty: *ty,
                        value: Box::new(value),
                    }),
                }
            }
            ExprKind::Unrefine(value) => {
                let value = self.eval_expr(frame, value)?;
                let Value::Refined { value, .. } = value else {
                    return Err(EvalAbort::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "unrefine operand is not a constrained value",
                        expression.span,
                    )));
                };
                Ok(*value)
            }
            ExprKind::Call {
                target,
                arguments,
                witnesses,
                ..
            } => self.eval_call(frame, target, arguments, witnesses, expression.span),
            ExprKind::MakeView {
                value,
                writeback,
                witness,
                mutable,
                token,
            } => {
                let value = self.eval_expr(frame, value)?;
                Ok(Value::DynView {
                    value: Box::new(value),
                    writeback: writeback
                        .as_ref()
                        .map(|owner| Location::from_place(frame, owner)),
                    witness: self.resolve_witness(frame, witness, expression.span)?,
                    mutable: *mutable,
                    token: *token,
                })
            }
            ExprKind::ReborrowView {
                owner,
                mutable,
                token,
            } => {
                let location = Location::from_place(frame, owner);
                let Value::DynView { value, witness, .. } =
                    self.read_place(&location, expression.span)?
                else {
                    return Err(EvalAbort::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "interface reborrow source is not an erased interface",
                        expression.span,
                    )));
                };
                Ok(Value::DynView {
                    value,
                    writeback: Some(location),
                    witness,
                    mutable: *mutable,
                    token: *token,
                })
            }
            ExprKind::Await { .. } => self.eval_nested_await(frame, expression),
            ExprKind::Sleep { milliseconds } => {
                let duration = self.eval_expr(frame, milliseconds)?;
                let milliseconds = match duration {
                    Value::Int { value } => value,
                    Value::Record { ty, fields } if self.program.prelude.duration == Some(ty) => {
                        record_descriptor(&fields, expression.span)?
                    }
                    _ => {
                        return Err(EvalAbort::from(self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "Task.sleep duration did not produce Int or Duration",
                            expression.span,
                        )));
                    }
                };
                if milliseconds < 0 {
                    return Err(EvalAbort::from(self.runtime_fault(
                        INVALID_SLEEP_DURATION_FAULT_CODE,
                        INVALID_SLEEP_DURATION_FAULT_MESSAGE,
                        expression.span,
                    )));
                }
                let nanoseconds = milliseconds.checked_mul(1_000_000).ok_or_else(|| {
                    EvalAbort::from(self.runtime_fault(
                        SLEEP_DURATION_OVERFLOW_FAULT_CODE,
                        SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
                        expression.span,
                    ))
                })?;
                let deadline = Instant::now()
                    .checked_add(Duration::from_nanos(nanoseconds.cast_unsigned()))
                    .ok_or_else(|| {
                        EvalAbort::from(self.runtime_fault(
                            SLEEP_DURATION_OVERFLOW_FAULT_CODE,
                            SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
                            expression.span,
                        ))
                    })?;
                let task_id = self.next_task;
                self.next_task = self.next_task.checked_add(1).ok_or_else(|| {
                    EvalAbort::from(self.runtime_fault(
                        "TaskIdExhausted",
                        "async task identity space was exhausted",
                        expression.span,
                    ))
                })?;
                self.tasks.insert(
                    task_id,
                    ManagedTask {
                        function: FunctionId(u32::MAX),
                        frame: u64::MAX,
                        parent: self.active_task,
                        children: Vec::new(),
                        cursor: 0,
                        awaiting_state: None,
                        cleanups: Vec::new(),
                        status: TaskStatus::Runnable,
                        queued: false,
                        marked: false,
                        timer_deadline: Some(deadline),
                        host_io: false,
                        contract_state: None,
                        join_mode: TaskJoinMode::All,
                        join_dynamic: false,
                        join_combined: false,
                        join_winner: None,
                        cancel_requested: false,
                    },
                );
                self.enqueue_task(task_id);
                self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
                self.gc_stats.live = self.tasks.len() as u64;
                Ok(Value::Task { id: task_id })
            }
            ExprKind::TaskJoin { mode, arguments } => {
                let values = arguments
                    .iter()
                    .map(|argument| self.eval_expr(frame, argument))
                    .collect::<Result<Vec<_>, _>>()?;
                let (values, dynamic) = match values.as_slice() {
                    [Value::List { elements }] => (elements.clone(), true),
                    _ => (values, false),
                };
                let mut tasks = Vec::with_capacity(values.len());
                for value in values {
                    let Value::Task { id } = value else {
                        return Err(EvalAbort::from(self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "Task join contained a non-task value",
                            expression.span,
                        )));
                    };
                    tasks.push(id);
                }
                Ok(Value::TaskJoin {
                    mode: *mode,
                    tasks,
                    dynamic,
                })
            }
        }
    }

    fn eval_match(
        &mut self,
        frame: u64,
        value: &Value,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        for arm in arms {
            let mut bindings = Vec::new();
            if pattern_matches(&arm.pattern, value, &mut bindings) {
                if bindings.len() != arm.bindings.len() {
                    return Err(EvalAbort::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "match binding count does not match pattern",
                        span,
                    )));
                }
                for (local, value) in arm.bindings.iter().zip(bindings) {
                    self.set_slot(frame, *local, Slot::Value(value), span)
                        .map_err(EvalAbort::from)?;
                }
                return self.eval_expr(frame, &arm.value);
            }
        }
        Err(EvalAbort::from(self.runtime_fault(
            "LOOM_RUNTIME_NON_EXHAUSTIVE_MATCH",
            "checked MIR reached a non-exhaustive match",
            span,
        )))
    }

    fn eval_call(
        &mut self,
        frame: u64,
        target: &CallTarget,
        arguments: &[CallArgument],
        witnesses: &[WitnessRef],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        if let CallTarget::Builtin(builtin) = target {
            return self.eval_builtin_call(frame, *builtin, arguments, span);
        }

        if let CallTarget::Dynamic { requirement } = target {
            return self.eval_dynamic_call(frame, *requirement, arguments, span);
        }

        if let CallTarget::StaticConcept {
            requirement,
            witness,
            ..
        } = target
        {
            return self.eval_static_concept_call(
                frame,
                *requirement,
                witness,
                arguments,
                witnesses,
                span,
            );
        }

        let function = match target {
            CallTarget::Direct(function) | CallTarget::Inherent(function) => *function,
            CallTarget::Dynamic { .. }
            | CallTarget::Builtin(_)
            | CallTarget::StaticConcept { .. } => unreachable!(),
        };
        let values = arguments
            .iter()
            .map(|argument| match argument {
                CallArgument::Value(value) => {
                    self.eval_expr(frame, value).map(BoundArgument::Value)
                }
                CallArgument::InOut(place) => {
                    Ok(BoundArgument::Alias(Location::from_place(frame, place)))
                }
            })
            .collect::<Result<Vec<_>, EvalAbort>>()?;
        let witness_values = witnesses
            .iter()
            .map(|witness| self.resolve_witness(frame, witness, span))
            .collect::<Result<Vec<_>, _>>()?;
        self.invoke_bound(function, values, witness_values, span)
            .map_err(EvalAbort::from)
    }

    fn eval_builtin_call(
        &mut self,
        frame: u64,
        builtin: Builtin,
        arguments: &[CallArgument],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        if builtin == Builtin::ListAdd {
            return self.eval_list_add(frame, arguments, span);
        }
        if matches!(builtin, Builtin::FileClose | Builtin::SocketClose) {
            return self.eval_resource_close(frame, builtin, arguments, span);
        }
        let values = arguments
            .iter()
            .map(|argument| match argument {
                CallArgument::Value(value) => self.eval_expr(frame, value),
                CallArgument::InOut(place) => {
                    Ok(self.read_place(&Location::from_place(frame, place), span)?)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.eval_builtin(builtin, &values, span)
            .map_err(EvalAbort::from)
    }

    fn eval_list_add(
        &mut self,
        frame: u64,
        arguments: &[CallArgument],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        let [CallArgument::InOut(place), CallArgument::Value(value)] = arguments else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "List.add has an invalid checked argument shape",
                span,
            )));
        };
        let value = self.eval_expr(frame, value)?;
        let location = Location::from_place(frame, place);
        let list = self.read_place(&location, span)?;
        let Value::List { mut elements } = unrefined(list) else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "List.add receiver is not a List",
                span,
            )));
        };
        elements.push(value);
        self.write_place(&location, Value::List { elements }, span)
            .map_err(EvalAbort::from)?;
        Ok(Value::Unit)
    }

    fn eval_resource_close(
        &mut self,
        frame: u64,
        builtin: Builtin,
        arguments: &[CallArgument],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        let [CallArgument::InOut(place)] = arguments else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "resource close has an invalid checked argument shape",
                span,
            )));
        };
        let location = Location::from_place(frame, place);
        let value = self.read_place(&location, span)?;
        let Value::Record { ty, mut fields } = unrefined(value) else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "resource close receiver is not a record",
                span,
            )));
        };
        let file = builtin == Builtin::FileClose;
        let expected = if file {
            self.program.prelude.file
        } else {
            self.program.prelude.socket
        };
        if expected != Some(ty) {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "resource close receiver has the wrong nominal type",
                span,
            )));
        }
        let descriptor = record_descriptor(&fields, span)?;
        if descriptor >= 0 {
            let handle = u64::try_from(descriptor).map_err(|_| {
                EvalAbort::from(self.runtime_fault(
                    "InvalidResourceHandle",
                    "resource handle exceeds the interpreter range",
                    span,
                ))
            })?;
            if file {
                self.files.remove(&handle);
            } else {
                self.sockets.remove(&handle);
            }
            fields[0] = Value::Int { value: -1 };
            self.write_place(&location, Value::Record { ty, fields }, span)
                .map_err(EvalAbort::from)?;
        }
        Ok(Value::Unit)
    }

    fn eval_scoped_disposal(
        &mut self,
        frame: u64,
        local: LocalId,
        disposal: &ScopedDisposal,
        span: Span,
    ) -> Result<Value, EvalAbort> {
        match disposal {
            ScopedDisposal::FileClose | ScopedDisposal::SocketClose => {
                let builtin = match disposal {
                    ScopedDisposal::FileClose => Builtin::FileClose,
                    ScopedDisposal::SocketClose => Builtin::SocketClose,
                    ScopedDisposal::StaticConcept { .. } => unreachable!(),
                };
                self.eval_resource_close(
                    frame,
                    builtin,
                    &[CallArgument::InOut(Place::local(local))],
                    span,
                )
            }
            ScopedDisposal::StaticConcept {
                requirement,
                witness,
                ..
            } => {
                let runtime_witness = self.resolve_witness(frame, witness, span)?;
                let definition = self
                    .program
                    .witness(runtime_witness.definition)
                    .cloned()
                    .ok_or_else(|| {
                        EvalAbort::from(self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "scoped disposal references an unknown witness",
                            span,
                        ))
                    })?;
                let function = definition
                    .methods
                    .get(requirement)
                    .copied()
                    .ok_or_else(|| {
                        EvalAbort::from(self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "scoped disposal witness is missing Dispose.dispose",
                            span,
                        ))
                    })?;
                self.invoke_bound(
                    function,
                    vec![BoundArgument::Alias(Location::local(frame, local))],
                    runtime_witness.arguments,
                    span,
                )
                .map_err(EvalAbort::from)
            }
        }
    }

    fn eval_static_concept_call(
        &mut self,
        frame: u64,
        requirement: RequirementId,
        witness_ref: &WitnessRef,
        arguments: &[CallArgument],
        method_witnesses: &[WitnessRef],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        let runtime_witness = self.resolve_witness(frame, witness_ref, span)?;
        let witness = self
            .program
            .witness(runtime_witness.definition)
            .cloned()
            .ok_or_else(|| {
                EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "static concept call references an unknown witness",
                    span,
                ))
            })?;
        let function = witness.methods.get(&requirement).copied().ok_or_else(|| {
            EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "static witness is missing a required method",
                span,
            ))
        })?;
        let values = arguments
            .iter()
            .map(|argument| match argument {
                CallArgument::Value(value) => {
                    self.eval_expr(frame, value).map(BoundArgument::Value)
                }
                CallArgument::InOut(place) => {
                    Ok(BoundArgument::Alias(Location::from_place(frame, place)))
                }
            })
            .collect::<Result<Vec<_>, EvalAbort>>()?;
        let mut proof_arguments = runtime_witness.arguments;
        proof_arguments.extend(
            method_witnesses
                .iter()
                .map(|witness| self.resolve_witness(frame, witness, span))
                .collect::<Result<Vec<_>, _>>()?,
        );
        self.invoke_bound(function, values, proof_arguments, span)
            .map_err(EvalAbort::from)
    }

    fn eval_dynamic_call(
        &mut self,
        frame: u64,
        requirement: RequirementId,
        arguments: &[CallArgument],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        let Some(first) = arguments.first() else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "dynamic call is missing its receiver",
                span,
            )));
        };
        let receiver = match first {
            CallArgument::Value(value) => self.eval_expr(frame, value)?,
            CallArgument::InOut(place) => {
                self.read_place(&Location::from_place(frame, place), span)?
            }
        };
        let Value::DynView {
            value,
            writeback,
            witness,
            mutable,
            ..
        } = receiver
        else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "dynamic call receiver is not an erased interface",
                span,
            )));
        };
        let witness_definition = self
            .program
            .witness(witness.definition)
            .cloned()
            .ok_or_else(|| {
                EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "erased interface references an unknown witness",
                    span,
                ))
            })?;
        let function_id = witness_definition
            .methods
            .get(&requirement)
            .copied()
            .ok_or_else(|| {
                EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "dynamic witness is missing a required method",
                    span,
                ))
            })?;
        let receiver_kind = self
            .program
            .function(function_id)
            .map(|function| function.receiver)
            .ok_or_else(|| {
                EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "dynamic witness method does not exist",
                    span,
                ))
            })?;
        if receiver_kind == Some(Receiver::Mutable) {
            if !mutable {
                return Err(EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "a readonly interface argument dispatched a `mut self` method",
                    span,
                )));
            }
            let CallArgument::InOut(place) = first else {
                return Err(EvalAbort::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "mutable dynamic receiver is not an inout place",
                    span,
                )));
            };
            let carrier = Location::from_place(frame, place);
            return self.invoke_mutable_dynamic(
                frame,
                &carrier,
                *value,
                writeback,
                witness,
                function_id,
                &arguments[1..],
                span,
            );
        }
        let mut values = vec![BoundArgument::Value(*value)];
        for argument in &arguments[1..] {
            values.push(match argument {
                CallArgument::Value(value) => BoundArgument::Value(self.eval_expr(frame, value)?),
                CallArgument::InOut(place) => {
                    BoundArgument::Alias(Location::from_place(frame, place))
                }
            });
        }
        self.invoke_bound(function_id, values, witness.arguments, span)
            .map_err(EvalAbort::from)
    }

    #[allow(clippy::too_many_arguments)]
    fn invoke_mutable_dynamic(
        &mut self,
        frame: u64,
        carrier: &Location,
        value: Value,
        writeback: Option<Location>,
        witness: RuntimeWitness,
        function: FunctionId,
        arguments: &[CallArgument],
        span: Span,
    ) -> Result<Value, EvalAbort> {
        let mut trailing = Vec::with_capacity(arguments.len());
        for argument in arguments {
            trailing.push(match argument {
                CallArgument::Value(value) => BoundArgument::Value(self.eval_expr(frame, value)?),
                CallArgument::InOut(place) => {
                    BoundArgument::Alias(Location::from_place(frame, place))
                }
            });
        }
        let temporary_frame = self.next_frame;
        self.next_frame = self.next_frame.checked_add(1).ok_or_else(|| {
            EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_FRAME_OVERFLOW",
                "interpreter frame identifier space was exhausted",
                span,
            ))
        })?;
        self.frames.insert(
            temporary_frame,
            Frame {
                slots: vec![Slot::Value(value)],
                witnesses: Vec::new(),
            },
        );
        let temporary = Location::local(temporary_frame, LocalId(0));
        let mut values = vec![BoundArgument::Alias(temporary.clone())];
        values.extend(trailing);
        let outcome = self.invoke_bound(function, values, witness.arguments.clone(), span);
        let mutated = self.read_place(&temporary, span);
        self.frames.remove(&temporary_frame);
        let mutated = mutated.map_err(EvalAbort::from)?;
        let current = self.read_place(carrier, span).map_err(EvalAbort::from)?;
        let Value::DynView { mutable, token, .. } = current else {
            return Err(EvalAbort::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "mutable dynamic receiver changed representation during its call",
                span,
            )));
        };
        self.write_place(
            carrier,
            Value::DynView {
                value: Box::new(mutated.clone()),
                writeback: writeback.clone(),
                witness,
                mutable,
                token,
            },
            span,
        )
        .map_err(EvalAbort::from)?;
        if let Some(writeback) = writeback {
            self.propagate_interface_writeback(writeback, mutated, span)
                .map_err(EvalAbort::from)?;
        }
        outcome.map_err(EvalAbort::from)
    }

    fn propagate_interface_writeback(
        &mut self,
        mut target: Location,
        value: Value,
        span: Span,
    ) -> Result<(), ExecutionFailure> {
        for _ in 0..64 {
            let current = self.read_place(&target, span)?;
            let Value::DynView {
                writeback,
                witness,
                mutable,
                token,
                ..
            } = current
            else {
                return self.write_place(&target, value, span);
            };
            self.write_place(
                &target,
                Value::DynView {
                    value: Box::new(value.clone()),
                    writeback: writeback.clone(),
                    witness,
                    mutable,
                    token,
                },
                span,
            )?;
            let Some(next) = writeback else {
                return Ok(());
            };
            target = next;
        }
        Err(self
            .runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "interface writeback chain exceeded its checked bound",
                span,
            )
            .into())
    }

    fn resolve_witness(
        &self,
        frame: u64,
        witness: &WitnessRef,
        span: Span,
    ) -> Result<RuntimeWitness, EvalAbort> {
        match witness {
            WitnessRef::Concrete(witness) => Ok(RuntimeWitness {
                definition: *witness,
                arguments: Vec::new(),
            }),
            WitnessRef::Parameter(index) => self
                .frames
                .get(&frame)
                .and_then(|frame| frame.witnesses.get(*index as usize))
                .cloned()
                .ok_or_else(|| {
                    EvalAbort::from(self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "witness parameter does not exist",
                        span,
                    ))
                }),
            WitnessRef::Apply { witness, arguments } => Ok(RuntimeWitness {
                definition: *witness,
                arguments: arguments
                    .iter()
                    .map(|argument| self.resolve_witness(frame, argument, span))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
        }
    }

    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    fn eval_builtin(
        &mut self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        if matches!(
            builtin,
            Builtin::DurationMilliseconds | Builtin::DurationAsMilliseconds
        ) {
            return self.eval_duration_builtin(builtin, arguments, span);
        }
        if matches!(
            builtin,
            Builtin::TextLength
                | Builtin::TextGet
                | Builtin::TextConcat
                | Builtin::TextContains
                | Builtin::TextEncodeUtf8
                | Builtin::TextFromUtf8Units
                | Builtin::BytesLength
                | Builtin::BytesGet
                | Builtin::BytesAppend
                | Builtin::BytesDecodeUtf8
                | Builtin::PathFromText
                | Builtin::PathAsText
                | Builtin::PathJoin
        ) {
            return self.eval_builtin_value(builtin, arguments, span);
        }
        if matches!(
            builtin,
            Builtin::FileOpenRead
                | Builtin::FileCreate
                | Builtin::FileOpenReadPath
                | Builtin::FileCreatePath
                | Builtin::FileTryOpenRead
                | Builtin::FileTryCreate
                | Builtin::FileTryOpenReadPath
                | Builtin::FileTryCreatePath
                | Builtin::FileReadText
                | Builtin::FileWriteText
                | Builtin::FileTryReadText
                | Builtin::FileTryWriteText
        ) {
            return self.eval_file_builtin(builtin, arguments, span);
        }
        if matches!(
            builtin,
            Builtin::SocketConnect
                | Builtin::SocketReadText
                | Builtin::SocketWriteText
                | Builtin::SocketTryConnect
                | Builtin::SocketTryReadText
                | Builtin::SocketTryWriteText
        ) {
            return self.eval_socket_builtin(builtin, arguments, span);
        }
        if matches!(
            builtin,
            Builtin::ListAdd | Builtin::ListLength | Builtin::ListGet | Builtin::ListToTextMap
        ) {
            return self.eval_list_builtin(builtin, arguments, span);
        }
        if matches!(
            builtin,
            Builtin::ProcessArgumentCount
                | Builtin::ProcessArgumentAt
                | Builtin::ProcessEnvironment
        ) {
            return self.eval_process_builtin(builtin, arguments, span);
        }
        if matches!(
            builtin,
            Builtin::TextMapNew
                | Builtin::TextMapLength
                | Builtin::TextMapContains
                | Builtin::TextMapGet
                | Builtin::TextMapEntryAt
                | Builtin::TextMapInsert
                | Builtin::TextMapRemove
        ) {
            return self.eval_text_map_builtin(builtin, arguments, span);
        }
        if builtin == Builtin::JsonFormat {
            return self.eval_json_format_builtin(arguments, span);
        }
        if matches!(builtin, Builtin::IoErrorKind | Builtin::IoErrorMessage) {
            return self.eval_io_error_builtin(builtin, arguments, span);
        }
        if builtin == Builtin::LogWrite {
            return self.eval_log_builtin(builtin, arguments, span);
        }
        if builtin == Builtin::StdoutWrite {
            return self.eval_stdout_builtin(arguments, span);
        }
        match (builtin, arguments) {
            (Builtin::FloatIsFinite, [value]) => Ok(Value::Bool {
                value: as_float(value).is_some_and(f64::is_finite),
            }),
            (Builtin::IntToFloat, [value]) => as_int(value).map_or_else(
                || {
                    Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "Int-to-Float conversion expected Int",
                            span,
                        )
                        .into())
                },
                |value| {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "Int-to-Float is an explicit language conversion with specified binary64 rounding"
                    )]
                    let converted = value as f64;
                    Ok(Value::Float { value: converted })
                },
            ),
            (Builtin::FloatToIntStatus, [value]) => as_float(value).map_or_else(
                || {
                    Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "Float-to-Int conversion expected Float",
                            span,
                        )
                        .into())
                },
                |value| {
                    let (converted, status) = float_to_int_status(value);
                    Ok(Value::Tuple {
                        elements: vec![
                            Value::Int { value: converted },
                            Value::Int { value: status },
                        ],
                    })
                },
            ),
            (Builtin::FloatParseStatus, [Value::Text { value }]) => {
                let (number, status) = match parse_float(value) {
                    Ok(number) => (number, 0),
                    Err(ParseFloatFailure::InvalidSyntax) => (0.0, 1),
                    Err(ParseFloatFailure::OutOfRange) => (0.0, 2),
                };
                Ok(Value::Tuple {
                    elements: vec![
                        Value::Float { value: number },
                        Value::Int { value: status },
                    ],
                })
            }
            (Builtin::FloatFormat, [value]) => as_float(value).map_or_else(
                || {
                    Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "format_float expected Float",
                            span,
                        )
                        .into())
                },
                |value| {
                    Ok(Value::Text {
                        value: format_float(value),
                    })
                },
            ),
            (
                Builtin::TaskFaultCode | Builtin::TaskFaultMessage,
                [Value::Record { ty, fields }],
            ) if self.program.prelude.task_fault == Some(*ty) => {
                let index = usize::from(builtin == Builtin::TaskFaultMessage);
                fields.get(index).cloned().ok_or_else(|| {
                    self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "TaskFault record is missing an accessor field",
                        span,
                    )
                    .into()
                })
            }
            _ => Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "builtin called with invalid arguments",
                    span,
                )
                .into()),
        }
    }

    fn eval_list_builtin(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::ListLength, [Value::List { elements }]) => {
                let value = i64::try_from(elements.len()).map_err(|_| {
                    ExecutionFailure::from(self.runtime_fault(
                        "ListTooLarge",
                        "List length exceeds Int",
                        span,
                    ))
                })?;
                Ok(Value::Int { value })
            }
            (Builtin::ListGet, [Value::List { elements }, Value::Int { value }]) => {
                let element = usize::try_from(*value)
                    .ok()
                    .and_then(|index| elements.get(index))
                    .cloned();
                self.option_value(element, span)
            }
            (Builtin::ListToTextMap, [Value::List { elements }]) => {
                let mut entries = elements
                    .iter()
                    .map(|element| match element {
                        Value::Tuple { elements } => match elements.as_slice() {
                            [Value::Text { value: key }, value] => Ok((key.clone(), value.clone())),
                            _ => Err(self.invalid_builtin_fault(span)),
                        },
                        _ => Err(self.invalid_builtin_fault(span)),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                if let Some(key) = entries
                    .windows(2)
                    .find_map(|pair| (pair[0].0 == pair[1].0).then(|| pair[0].0.clone()))
                {
                    self.result_value(false, Value::Text { value: key }, span)
                } else {
                    let map = self.text_map_value_from_sorted_entries(entries, span)?;
                    self.result_value(true, map, span)
                }
            }
            (Builtin::ListAdd, _) => Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "List.add did not receive an inout receiver",
                    span,
                )
                .into()),
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn eval_process_builtin(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::ProcessArgumentCount, []) => Ok(Value::Int {
                value: i64::try_from(self.process_arguments.len()).map_err(|_| {
                    self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "process argument count exceeds Int",
                        span,
                    )
                })?,
            }),
            (Builtin::ProcessArgumentAt, [Value::Int { value: index }]) => {
                let value = usize::try_from(*index)
                    .ok()
                    .and_then(|index| self.process_arguments.get(index))
                    .cloned()
                    .ok_or_else(|| {
                        self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "process argument index is out of bounds",
                            span,
                        )
                    })?;
                Ok(Value::Text { value })
            }
            (Builtin::ProcessEnvironment, [Value::Text { value: name }]) => self.option_value(
                std::env::var(name).ok().map(|value| Value::Text { value }),
                span,
            ),
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn eval_text_map_builtin(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::TextMapNew, []) => self.text_map_value(Vec::new(), span),
            (Builtin::TextMapLength, [map]) => {
                let entries = self.text_map_entries(map, span)?;
                Ok(Value::Int {
                    value: i64::try_from(entries.len()).map_err(|_| {
                        self.runtime_fault("TextMapTooLarge", "TextMap length exceeds Int", span)
                    })?,
                })
            }
            (Builtin::TextMapContains, [map, Value::Text { value: key }]) => {
                let entries = self.text_map_entries(map, span)?;
                Ok(Value::Bool {
                    value: entries.iter().any(|(candidate, _)| candidate == key),
                })
            }
            (Builtin::TextMapGet, [map, Value::Text { value: key }]) => {
                let value = self
                    .text_map_entries(map, span)?
                    .into_iter()
                    .find(|(candidate, _)| candidate == key)
                    .map(|(_, value)| value);
                self.option_value(value, span)
            }
            (Builtin::TextMapEntryAt, [map, Value::Int { value: index }]) => {
                let entries = self.text_map_entries(map, span)?;
                let entry = usize::try_from(*index)
                    .ok()
                    .and_then(|index| entries.get(index).cloned());
                self.option_value(
                    entry.map(|(key, value)| Value::Tuple {
                        elements: vec![Value::Text { value: key }, value],
                    }),
                    span,
                )
            }
            (Builtin::TextMapInsert, [map, Value::Text { value: key }, value]) => {
                let mut entries = self.text_map_entries(map, span)?;
                if let Some((_, existing)) =
                    entries.iter_mut().find(|(candidate, _)| candidate == key)
                {
                    *existing = value.clone();
                } else {
                    entries.push((key.clone(), value.clone()));
                }
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                self.text_map_value_from_sorted_entries(entries, span)
            }
            (Builtin::TextMapRemove, [map, Value::Text { value: key }]) => {
                let mut entries = self.text_map_entries(map, span)?;
                entries.retain(|(candidate, _)| candidate != key);
                self.text_map_value(entries, span)
            }
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn text_map_entries(
        &self,
        value: &Value,
        span: Span,
    ) -> Result<Vec<(String, Value)>, ExecutionFailure> {
        let Value::Record { ty, fields } = value else {
            return Err(self.invalid_builtin_fault(span));
        };
        if self.program.prelude.text_map != Some(*ty) || fields.len() % 2 != 0 {
            return Err(self.invalid_builtin_fault(span));
        }
        fields
            .chunks_exact(2)
            .map(|pair| match pair {
                [Value::Text { value: key }, value] => Ok((key.clone(), value.clone())),
                _ => Err(self.invalid_builtin_fault(span)),
            })
            .collect()
    }

    fn text_map_value(
        &self,
        mut entries: Vec<(String, Value)>,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        self.text_map_value_from_sorted_entries(entries, span)
    }

    fn text_map_value_from_sorted_entries(
        &self,
        entries: Vec<(String, Value)>,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let ty = self.program.prelude.text_map.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude TextMap type is missing",
                span,
            )
        })?;
        Ok(Value::Record {
            ty,
            fields: entries
                .into_iter()
                .flat_map(|(key, value)| [Value::Text { value: key }, value])
                .collect(),
        })
    }

    fn eval_json_format_builtin(
        &self,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match arguments {
            [value] => match self.json_to_runtime(value, span, 0) {
                Ok(value) => match loom_runtime::format_json(&value) {
                    Ok(value) => self.result_value(true, Value::Text { value }, span),
                    Err(error) => match error {
                        loom_runtime::JsonFormatFailure::DepthLimit => {
                            self.json_format_error_result(VariantId(2), span)
                        }
                        loom_runtime::JsonFormatFailure::NonFiniteNumber => {
                            self.json_format_error_result(VariantId(3), span)
                        }
                    },
                },
                Err(JsonConversionFailure::DepthLimit) => {
                    self.json_format_error_result(VariantId(2), span)
                }
                Err(JsonConversionFailure::Invalid(failure)) => Err(failure),
            },
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn json_to_runtime(
        &self,
        value: &Value,
        span: Span,
        depth: usize,
    ) -> Result<loom_runtime::JsonNode, JsonConversionFailure> {
        let Value::Enum {
            ty,
            variant,
            payload,
        } = value
        else {
            return Err(JsonConversionFailure::Invalid(
                self.invalid_builtin_fault(span),
            ));
        };
        if self.program.prelude.json != Some(*ty) {
            return Err(JsonConversionFailure::Invalid(
                self.invalid_builtin_fault(span),
            ));
        }
        match (variant.0, payload.as_slice()) {
            (0, []) => Ok(loom_runtime::JsonNode::Null),
            (1, [Value::Bool { value }]) => Ok(loom_runtime::JsonNode::Bool(*value)),
            (2, [Value::Float { value }]) => Ok(loom_runtime::JsonNode::Number(*value)),
            (3, [Value::Text { value }]) => Ok(loom_runtime::JsonNode::Text(value.clone())),
            (4, [Value::List { elements }]) => {
                if depth >= loom_runtime::JSON_DEPTH_LIMIT {
                    return Err(JsonConversionFailure::DepthLimit);
                }
                Ok(loom_runtime::JsonNode::Array(
                    elements
                        .iter()
                        .map(|value| self.json_to_runtime(value, span, depth + 1))
                        .collect::<Result<Vec<_>, _>>()?,
                ))
            }
            (5, [map]) => {
                if depth >= loom_runtime::JSON_DEPTH_LIMIT {
                    return Err(JsonConversionFailure::DepthLimit);
                }
                let mut object = BTreeMap::new();
                for (key, value) in self.text_map_entries(map, span)? {
                    object.insert(key, self.json_to_runtime(&value, span, depth + 1)?);
                }
                Ok(loom_runtime::JsonNode::Object(object))
            }
            _ => Err(JsonConversionFailure::Invalid(
                self.invalid_builtin_fault(span),
            )),
        }
    }

    fn json_format_error_result(
        &self,
        variant: VariantId,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let ty = self.program.prelude.json_error.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude JsonError type is missing",
                span,
            )
        })?;
        self.result_value(
            false,
            Value::Enum {
                ty,
                variant,
                payload: Vec::new(),
            },
            span,
        )
    }

    fn eval_io_error_builtin(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::IoErrorKind | Builtin::IoErrorMessage, [Value::Record { ty, fields }])
                if self.program.prelude.io_error == Some(*ty) =>
            {
                fields
                    .get(usize::from(builtin == Builtin::IoErrorMessage))
                    .cloned()
                    .ok_or_else(|| self.invalid_builtin_fault(span))
            }
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn eval_log_builtin(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let (level, message, fields) = match (builtin, arguments) {
            (Builtin::LogWrite, [level, Value::Text { value }, fields]) => {
                let level = self.log_level(level, span)?;
                (level, value, self.text_map_entries(fields, span)?)
            }
            _ => return Err(self.invalid_builtin_fault(span)),
        };
        let mut line = String::from("{\"level\":");
        line.push_str(&loom_runtime::escape_json_text(level));
        line.push_str(",\"message\":");
        line.push_str(&loom_runtime::escape_json_text(message));
        line.push_str(",\"fields\":{");
        for (index, (key, value)) in fields.into_iter().enumerate() {
            let Value::Text { value } = value else {
                return Err(self.invalid_builtin_fault(span));
            };
            if index > 0 {
                line.push(',');
            }
            line.push_str(&loom_runtime::escape_json_text(&key));
            line.push(':');
            line.push_str(&loom_runtime::escape_json_text(&value));
        }
        line.push_str("}}\n");
        if loom_runtime::write_process_stderr(line.as_bytes()) != loom_runtime::TYPED_LOG_OK {
            return Err(self
                .runtime_fault(LOG_WRITE_FAULT_CODE, LOG_WRITE_FAULT_MESSAGE, span)
                .into());
        }
        Ok(Value::Unit)
    }

    fn eval_stdout_builtin(
        &self,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let [Value::Text { value }] = arguments else {
            return Err(self.invalid_builtin_fault(span));
        };
        let status = loom_runtime::write_process_stdout(value.as_bytes());
        if status != loom_runtime::STDOUT_WRITE_OK {
            return Err(self
                .runtime_fault(STDOUT_WRITE_FAULT_CODE, STDOUT_WRITE_FAULT_MESSAGE, span)
                .into());
        }
        Ok(Value::Unit)
    }

    fn log_level(&self, value: &Value, span: Span) -> Result<&'static str, ExecutionFailure> {
        let Value::Enum {
            ty,
            variant,
            payload,
        } = value
        else {
            return Err(self.invalid_builtin_fault(span));
        };
        if self.program.prelude.log_level != Some(*ty) || !payload.is_empty() {
            return Err(self.invalid_builtin_fault(span));
        }
        ["debug", "info", "warn", "error"]
            .get(variant.0 as usize)
            .copied()
            .ok_or_else(|| self.invalid_builtin_fault(span))
    }

    fn eval_duration_builtin(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::DurationMilliseconds, [Value::Int { value }]) => {
                if *value < 0 {
                    return Err(self
                        .runtime_fault(
                            INVALID_DURATION_FAULT_CODE,
                            INVALID_DURATION_FAULT_MESSAGE,
                            span,
                        )
                        .into());
                }
                self.opaque_record(self.program.prelude.duration, *value, "Duration", span)
            }
            (Builtin::DurationAsMilliseconds, [Value::Record { ty, fields }])
                if self.program.prelude.duration == Some(*ty) =>
            {
                fields.first().cloned().ok_or_else(|| {
                    self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "Duration record is missing its value",
                        span,
                    )
                    .into()
                })
            }
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn eval_builtin_value(
        &self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::TextLength, [Value::Text { value }]) => Ok(Value::Int {
                value: i64::try_from(value.chars().count()).map_err(|_| {
                    self.runtime_fault("TextTooLarge", "Text length exceeds Int", span)
                })?,
            }),
            (Builtin::TextGet, [Value::Text { value }, Value::Int { value: index }]) => {
                let scalar = usize::try_from(*index)
                    .ok()
                    .and_then(|index| value.chars().nth(index))
                    .map(|value| Value::Text {
                        value: value.to_string(),
                    });
                self.option_value(scalar, span)
            }
            (Builtin::TextConcat, [Value::Text { value: left }, Value::Text { value: right }]) => {
                let mut value = String::with_capacity(left.len().saturating_add(right.len()));
                value.push_str(left);
                value.push_str(right);
                Ok(Value::Text { value })
            }
            (Builtin::TextContains, [Value::Text { value }, Value::Text { value: needle }]) => {
                Ok(Value::Bool {
                    value: value.contains(needle),
                })
            }
            (Builtin::TextEncodeUtf8, [Value::Text { value }]) => {
                self.bytes_value(value.as_bytes().to_vec(), span)
            }
            (Builtin::TextFromUtf8Units, [Value::List { elements }]) => {
                let mut units = Vec::with_capacity(elements.len());
                let mut invalid_unit = false;
                for element in elements {
                    let Value::Int { value } = element else {
                        return Err(self
                            .runtime_fault(
                                "LOOM_RUNTIME_INVALID_MIR",
                                "Text.from_utf8_units expected List[Int]",
                                span,
                            )
                            .into());
                    };
                    if let Ok(value) = u8::try_from(*value) {
                        units.push(value);
                    } else {
                        invalid_unit = true;
                        break;
                    }
                }
                let value = if invalid_unit {
                    None
                } else {
                    String::from_utf8(units).ok()
                };
                if let Some(value) = value {
                    self.result_value(true, Value::Text { value }, span)
                } else {
                    let ty = self.program.prelude.decode_text_error.ok_or_else(|| {
                        self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "prelude DecodeTextError type is missing",
                            span,
                        )
                    })?;
                    self.result_value(
                        false,
                        Value::Enum {
                            ty,
                            variant: VariantId(0),
                            payload: Vec::new(),
                        },
                        span,
                    )
                }
            }
            (Builtin::BytesLength, [bytes]) => Ok(Value::Int {
                value: i64::try_from(self.bytes_payload(bytes, span)?.len()).map_err(|_| {
                    self.runtime_fault("BytesTooLarge", "Bytes length exceeds Int", span)
                })?,
            }),
            (Builtin::BytesGet, [bytes, Value::Int { value: index }]) => {
                let byte = usize::try_from(*index)
                    .ok()
                    .and_then(|index| self.bytes_payload(bytes, span).ok()?.get(index))
                    .map(|value| Value::Int {
                        value: i64::from(*value),
                    });
                self.option_value(byte, span)
            }
            (Builtin::BytesAppend, [left, right]) => {
                let left = self.bytes_payload(left, span)?;
                let right = self.bytes_payload(right, span)?;
                let mut value = Vec::with_capacity(left.len().saturating_add(right.len()));
                value.extend_from_slice(left);
                value.extend_from_slice(right);
                self.bytes_value(value, span)
            }
            (Builtin::BytesDecodeUtf8, [bytes]) => {
                let Ok(value) = String::from_utf8(self.bytes_payload(bytes, span)?.to_vec()) else {
                    let ty = self.program.prelude.decode_text_error.ok_or_else(|| {
                        self.runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "prelude DecodeTextError type is missing",
                            span,
                        )
                    })?;
                    return self.result_value(
                        false,
                        Value::Enum {
                            ty,
                            variant: VariantId(0),
                            payload: Vec::new(),
                        },
                        span,
                    );
                };
                self.result_value(true, Value::Text { value }, span)
            }
            (Builtin::PathFromText, [Value::Text { value }]) => {
                if value.as_bytes().contains(&0) {
                    self.path_result_error(VariantId(0), span)
                } else {
                    self.result_value(true, self.path_value(value.clone(), span)?, span)
                }
            }
            (Builtin::PathAsText, [path]) => Ok(Value::Text {
                value: self.path_payload(path, span)?.to_owned(),
            }),
            (Builtin::PathJoin, [base, child]) => {
                let base = self.path_payload(base, span)?;
                let child = self.path_payload(child, span)?;
                if child.starts_with('/') {
                    return self.path_result_error(VariantId(1), span);
                }
                let value = if base.is_empty() {
                    child.to_owned()
                } else if base.ends_with('/') || child.is_empty() {
                    format!("{base}{child}")
                } else {
                    format!("{base}/{child}")
                };
                self.result_value(true, self.path_value(value, span)?, span)
            }
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn bytes_payload<'value>(
        &self,
        value: &'value Value,
        span: Span,
    ) -> Result<&'value [u8], ExecutionFailure> {
        let Value::Record { ty, fields } = value else {
            return Err(self.invalid_builtin_fault(span));
        };
        if self.program.prelude.bytes != Some(*ty) {
            return Err(self.invalid_builtin_fault(span));
        }
        match fields.first() {
            Some(Value::Bytes { value }) => Ok(value),
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn bytes_value(&self, value: Vec<u8>, span: Span) -> Result<Value, ExecutionFailure> {
        let ty = self.program.prelude.bytes.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude Bytes type is missing",
                span,
            )
        })?;
        Ok(Value::Record {
            ty,
            fields: vec![Value::Bytes { value }],
        })
    }

    fn path_payload<'value>(
        &self,
        value: &'value Value,
        span: Span,
    ) -> Result<&'value str, ExecutionFailure> {
        let Value::Record { ty, fields } = value else {
            return Err(self.invalid_builtin_fault(span));
        };
        if self.program.prelude.path != Some(*ty) {
            return Err(self.invalid_builtin_fault(span));
        }
        match fields.first() {
            Some(Value::Text { value }) => Ok(value),
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn path_value(&self, value: String, span: Span) -> Result<Value, ExecutionFailure> {
        let ty = self.program.prelude.path.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude Path type is missing",
                span,
            )
        })?;
        Ok(Value::Record {
            ty,
            fields: vec![Value::Text { value }],
        })
    }

    fn path_result_error(&self, variant: VariantId, span: Span) -> Result<Value, ExecutionFailure> {
        let ty = self.program.prelude.path_error.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude PathError type is missing",
                span,
            )
        })?;
        self.result_value(
            false,
            Value::Enum {
                ty,
                variant,
                payload: Vec::new(),
            },
            span,
        )
    }

    #[allow(clippy::too_many_lines)]
    fn eval_file_builtin(
        &mut self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::FileOpenRead, [Value::Text { value: path }]) => {
                let path = path.clone();
                self.spawn_host_io_task(span, move || {
                    std::fs::File::open(path)
                        .map(HostIoValue::File)
                        .map_err(|error| io_failure("FileOpenFault", &error, span))
                })
            }
            (Builtin::FileCreate, [Value::Text { value: path }]) => {
                let path = path.clone();
                self.spawn_host_io_task(span, move || {
                    std::fs::File::create(path)
                        .map(HostIoValue::File)
                        .map_err(|error| io_failure("FileCreateFault", &error, span))
                })
            }
            (Builtin::FileOpenReadPath, [path]) => {
                let path = self.path_payload(path, span)?.to_owned();
                self.spawn_host_io_task(span, move || {
                    std::fs::File::open(path)
                        .map(HostIoValue::File)
                        .map_err(|error| io_failure("FileOpenFault", &error, span))
                })
            }
            (Builtin::FileCreatePath, [path]) => {
                let path = self.path_payload(path, span)?.to_owned();
                self.spawn_host_io_task(span, move || {
                    std::fs::File::create(path)
                        .map(HostIoValue::File)
                        .map_err(|error| io_failure("FileCreateFault", &error, span))
                })
            }
            (
                Builtin::FileTryOpenRead
                | Builtin::FileTryCreate
                | Builtin::FileTryOpenReadPath
                | Builtin::FileTryCreatePath,
                [path],
            ) => {
                let path = if matches!(
                    builtin,
                    Builtin::FileTryOpenReadPath | Builtin::FileTryCreatePath
                ) {
                    self.path_payload(path, span)?.to_owned()
                } else if let Value::Text { value } = path {
                    value.clone()
                } else {
                    return Err(self.invalid_builtin_fault(span));
                };
                let create = matches!(builtin, Builtin::FileTryCreate | Builtin::FileTryCreatePath);
                self.spawn_try_host_io_task(span, move || {
                    let result = if create {
                        std::fs::File::create(path)
                    } else {
                        std::fs::File::open(path)
                    };
                    result.map(HostIoValue::File).map_err(host_io_error)
                })
            }
            (Builtin::FileReadText, [Value::Record { ty, fields }])
                if self.program.prelude.file == Some(*ty) =>
            {
                let handle = checked_resource_handle(fields, span)?;
                let file = self.files.get(&handle).map_or_else(
                    || Err(resource_closed("File", span)),
                    |file| {
                        file.try_clone()
                            .map_err(|error| io_failure("FileReadFault", &error, span))
                    },
                );
                match file {
                    Ok(mut file) => self.spawn_host_io_task(span, move || {
                        let mut value = String::new();
                        file.read_to_string(&mut value)
                            .map(|_| HostIoValue::Value(Value::Text { value }))
                            .map_err(|error| io_failure("FileReadFault", &error, span))
                    }),
                    Err(failure) => self.spawn_terminal_task(Err(failure), span),
                }
            }
            (Builtin::FileWriteText, [Value::Record { ty, fields }, Value::Text { value }])
                if self.program.prelude.file == Some(*ty) =>
            {
                let handle = checked_resource_handle(fields, span)?;
                let file = self.files.get(&handle).map_or_else(
                    || Err(resource_closed("File", span)),
                    |file| {
                        file.try_clone()
                            .map_err(|error| io_failure("FileWriteFault", &error, span))
                    },
                );
                match file {
                    Ok(mut file) => {
                        let value = value.clone();
                        self.spawn_host_io_task(span, move || {
                            file.write_all(value.as_bytes())
                                .map(|()| HostIoValue::Value(Value::Unit))
                                .map_err(|error| io_failure("FileWriteFault", &error, span))
                        })
                    }
                    Err(failure) => self.spawn_terminal_task(Err(failure), span),
                }
            }
            (Builtin::FileTryReadText, [Value::Record { ty, fields }])
                if self.program.prelude.file == Some(*ty) =>
            {
                let Some(Value::Int { value: descriptor }) = fields.first() else {
                    return Err(self.invalid_builtin_fault(span));
                };
                let descriptor = *descriptor;
                if descriptor < 0 {
                    return self.spawn_io_error_task(8, "File is already closed".into(), span);
                }
                let Ok(handle) = u64::try_from(descriptor) else {
                    return self.spawn_io_error_task(8, "File is already closed".into(), span);
                };
                let Some(file) = self.files.get(&handle) else {
                    return self.spawn_io_error_task(8, "File is already closed".into(), span);
                };
                match file.try_clone() {
                    Ok(mut file) => self.spawn_try_host_io_task(span, move || {
                        let mut value = String::new();
                        file.read_to_string(&mut value)
                            .map(|_| HostIoValue::Value(Value::Text { value }))
                            .map_err(host_io_error)
                    }),
                    Err(error) => {
                        let error = host_io_error(error);
                        self.spawn_io_error_task(error.kind, error.message, span)
                    }
                }
            }
            (Builtin::FileTryWriteText, [Value::Record { ty, fields }, Value::Text { value }])
                if self.program.prelude.file == Some(*ty) =>
            {
                let Some(Value::Int { value: descriptor }) = fields.first() else {
                    return Err(self.invalid_builtin_fault(span));
                };
                let descriptor = *descriptor;
                if descriptor < 0 {
                    return self.spawn_io_error_task(8, "File is already closed".into(), span);
                }
                let Ok(handle) = u64::try_from(descriptor) else {
                    return self.spawn_io_error_task(8, "File is already closed".into(), span);
                };
                let Some(file) = self.files.get(&handle) else {
                    return self.spawn_io_error_task(8, "File is already closed".into(), span);
                };
                match file.try_clone() {
                    Ok(mut file) => {
                        let value = value.clone();
                        self.spawn_try_host_io_task(span, move || {
                            file.write_all(value.as_bytes())
                                .map(|()| HostIoValue::Value(Value::Unit))
                                .map_err(host_io_error)
                        })
                    }
                    Err(error) => {
                        let error = host_io_error(error);
                        self.spawn_io_error_task(error.kind, error.message, span)
                    }
                }
            }
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn eval_socket_builtin(
        &mut self,
        builtin: Builtin,
        arguments: &[Value],
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (builtin, arguments) {
            (Builtin::SocketConnect, [Value::Text { value: host }, Value::Int { value: port }]) => {
                let Ok(port) = u16::try_from(*port) else {
                    let failure =
                        self.runtime_fault("InvalidPort", "socket port must fit UInt16", span);
                    return self.spawn_terminal_task(Err(failure.into()), span);
                };
                let host = host.clone();
                self.spawn_host_io_task(span, move || {
                    let address = (host.as_str(), port)
                        .to_socket_addrs()
                        .map_err(|error| io_failure("SocketResolveFault", &error, span))?
                        .next()
                        .ok_or_else(|| {
                            ExecutionFailure::from(RuntimeFault {
                                code: "SocketResolveFault".into(),
                                message: "host resolved to no addresses".into(),
                                span,
                            })
                        })?;
                    TcpStream::connect(address)
                        .map(HostIoValue::Socket)
                        .map_err(|error| io_failure("SocketConnectFault", &error, span))
                })
            }
            (
                Builtin::SocketTryConnect,
                [Value::Text { value: host }, Value::Int { value: port }],
            ) => {
                let Ok(port) = u16::try_from(*port) else {
                    return self.spawn_io_error_task(3, "socket port must fit UInt16".into(), span);
                };
                let host = host.clone();
                self.spawn_try_host_io_task(span, move || {
                    let mut addresses = (host.as_str(), port)
                        .to_socket_addrs()
                        .map_err(host_io_error)?;
                    let address = addresses.next().ok_or_else(|| HostIoError {
                        kind: 9,
                        message: "host resolved to no addresses".into(),
                    })?;
                    TcpStream::connect(address)
                        .map(HostIoValue::Socket)
                        .map_err(host_io_error)
                })
            }
            (Builtin::SocketReadText, [Value::Record { ty, fields }])
                if self.program.prelude.socket == Some(*ty) =>
            {
                let handle = checked_resource_handle(fields, span)?;
                let socket = self.sockets.get(&handle).map_or_else(
                    || Err(resource_closed("Socket", span)),
                    |socket| {
                        socket
                            .try_clone()
                            .map_err(|error| io_failure("SocketReadFault", &error, span))
                    },
                );
                match socket {
                    Ok(socket) => self.spawn_socket_io_task(
                        socket,
                        SocketIoOperation::Read { bytes: Vec::new() },
                        false,
                        span,
                    ),
                    Err(failure) => self.spawn_terminal_task(Err(failure), span),
                }
            }
            (Builtin::SocketWriteText, [Value::Record { ty, fields }, Value::Text { value }])
                if self.program.prelude.socket == Some(*ty) =>
            {
                let handle = checked_resource_handle(fields, span)?;
                let socket = self.sockets.get(&handle).map_or_else(
                    || Err(resource_closed("Socket", span)),
                    |socket| {
                        socket
                            .try_clone()
                            .map_err(|error| io_failure("SocketWriteFault", &error, span))
                    },
                );
                match socket {
                    Ok(socket) => self.spawn_socket_io_task(
                        socket,
                        SocketIoOperation::Write {
                            bytes: value.as_bytes().to_vec(),
                            offset: 0,
                        },
                        false,
                        span,
                    ),
                    Err(failure) => self.spawn_terminal_task(Err(failure), span),
                }
            }
            (Builtin::SocketTryReadText, [Value::Record { ty, fields }])
                if self.program.prelude.socket == Some(*ty) =>
            {
                let Some(Value::Int { value: descriptor }) = fields.first() else {
                    return Err(self.invalid_builtin_fault(span));
                };
                let descriptor = *descriptor;
                if descriptor < 0 {
                    return self.spawn_io_error_task(8, "Socket is already closed".into(), span);
                }
                let Ok(handle) = u64::try_from(descriptor) else {
                    return self.spawn_io_error_task(8, "Socket is already closed".into(), span);
                };
                let Some(socket) = self.sockets.get(&handle) else {
                    return self.spawn_io_error_task(8, "Socket is already closed".into(), span);
                };
                match socket.try_clone() {
                    Ok(socket) => self.spawn_socket_io_task(
                        socket,
                        SocketIoOperation::Read { bytes: Vec::new() },
                        true,
                        span,
                    ),
                    Err(error) => {
                        let error = host_io_error(error);
                        self.spawn_io_error_task(error.kind, error.message, span)
                    }
                }
            }
            (
                Builtin::SocketTryWriteText,
                [Value::Record { ty, fields }, Value::Text { value }],
            ) if self.program.prelude.socket == Some(*ty) => {
                let Some(Value::Int { value: descriptor }) = fields.first() else {
                    return Err(self.invalid_builtin_fault(span));
                };
                let descriptor = *descriptor;
                if descriptor < 0 {
                    return self.spawn_io_error_task(8, "Socket is already closed".into(), span);
                }
                let Ok(handle) = u64::try_from(descriptor) else {
                    return self.spawn_io_error_task(8, "Socket is already closed".into(), span);
                };
                let Some(socket) = self.sockets.get(&handle) else {
                    return self.spawn_io_error_task(8, "Socket is already closed".into(), span);
                };
                match socket.try_clone() {
                    Ok(socket) => self.spawn_socket_io_task(
                        socket,
                        SocketIoOperation::Write {
                            bytes: value.as_bytes().to_vec(),
                            offset: 0,
                        },
                        true,
                        span,
                    ),
                    Err(error) => {
                        let error = host_io_error(error);
                        self.spawn_io_error_task(error.kind, error.message, span)
                    }
                }
            }
            _ => Err(self.invalid_builtin_fault(span)),
        }
    }

    fn invalid_builtin_fault(&self, span: Span) -> ExecutionFailure {
        self.runtime_fault(
            "LOOM_RUNTIME_INVALID_MIR",
            "builtin called with invalid arguments",
            span,
        )
        .into()
    }

    fn option_value(&self, payload: Option<Value>, span: Span) -> Result<Value, ExecutionFailure> {
        let option_type = self
            .program
            .prelude
            .option
            .and_then(|id| self.program.type_def(id))
            .ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "prelude Option type is missing",
                    span,
                ))
            })?;
        Ok(Value::Enum {
            ty: option_type.id,
            variant: VariantId(u32::from(payload.is_some())),
            payload: payload.into_iter().collect(),
        })
    }

    fn task_outcome_completed(&self, value: Value, span: Span) -> Result<Value, ExecutionFailure> {
        if self.program.prelude.task_outcome.is_none() {
            return Ok(Value::TaskOutcome {
                outcome: TaskOutcomeValue::Completed(Box::new(value)),
            });
        }
        self.task_outcome_variant(VariantId(0), vec![value], span)
    }

    fn task_outcome_faulted(
        &self,
        failure: &ExecutionFailure,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        if self.program.prelude.task_outcome.is_none() {
            return Ok(Value::TaskOutcome {
                outcome: TaskOutcomeValue::Faulted,
            });
        }
        let task_fault = self.program.prelude.task_fault.ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude TaskFault type is missing",
                span,
            ))
        })?;
        let (code, message) = match failure {
            ExecutionFailure::Contract { fault } => (&fault.code, &fault.message),
            ExecutionFailure::Runtime { fault } => (&fault.code, &fault.message),
            ExecutionFailure::Defect { defect } => (&defect.code, &defect.message),
        };
        let fault = Value::Record {
            ty: task_fault,
            fields: vec![
                Value::Text {
                    value: code.clone(),
                },
                Value::Text {
                    value: message.clone(),
                },
            ],
        };
        self.task_outcome_variant(VariantId(1), vec![fault], span)
    }

    fn task_outcome_cancelled(&self, span: Span) -> Result<Value, ExecutionFailure> {
        if self.program.prelude.task_outcome.is_none() {
            return Ok(Value::TaskOutcome {
                outcome: TaskOutcomeValue::Cancelled,
            });
        }
        self.task_outcome_variant(VariantId(2), Vec::new(), span)
    }

    fn task_outcome_variant(
        &self,
        variant: VariantId,
        payload: Vec<Value>,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let task_outcome = self.program.prelude.task_outcome.ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude TaskOutcome type is missing",
                span,
            ))
        })?;
        Ok(Value::Enum {
            ty: task_outcome,
            variant,
            payload,
        })
    }

    fn opaque_record(
        &self,
        ty: Option<TypeId>,
        raw: i64,
        name: &str,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let ty = ty.ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                format!("prelude {name} type is missing"),
                span,
            ))
        })?;
        Ok(Value::Record {
            ty,
            fields: vec![Value::Int { value: raw }],
        })
    }

    fn insert_file(&mut self, file: std::fs::File, span: Span) -> Result<Value, ExecutionFailure> {
        let handle = self.allocate_resource_handle(span)?;
        self.files.insert(handle, file);
        self.opaque_record(
            self.program.prelude.file,
            i64::try_from(handle).expect("resource handle is bounded by i64"),
            "File",
            span,
        )
    }

    fn insert_socket(&mut self, socket: TcpStream, span: Span) -> Result<Value, ExecutionFailure> {
        let handle = self.allocate_resource_handle(span)?;
        self.sockets.insert(handle, socket);
        self.opaque_record(
            self.program.prelude.socket,
            i64::try_from(handle).expect("resource handle is bounded by i64"),
            "Socket",
            span,
        )
    }

    fn allocate_resource_handle(&mut self, span: Span) -> Result<u64, ExecutionFailure> {
        let handle = self.next_resource;
        if handle > i64::MAX.cast_unsigned() {
            return Err(self
                .runtime_fault(
                    "ResourceHandleExhausted",
                    "interpreter resource handle space was exhausted",
                    span,
                )
                .into());
        }
        self.next_resource = self.next_resource.checked_add(1).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "ResourceHandleExhausted",
                "interpreter resource handle space was exhausted",
                span,
            ))
        })?;
        Ok(handle)
    }

    fn spawn_terminal_task(
        &mut self,
        outcome: Result<Value, ExecutionFailure>,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let task_id = self.next_task;
        self.next_task = self.next_task.checked_add(1).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "TaskIdExhausted",
                "async task identity space was exhausted",
                span,
            ))
        })?;
        let status = match outcome {
            Ok(value) => TaskStatus::Completed(value),
            Err(failure) => TaskStatus::Failed(failure),
        };
        self.tasks.insert(
            task_id,
            ManagedTask {
                function: FunctionId(u32::MAX),
                frame: u64::MAX,
                parent: self.active_task,
                children: Vec::new(),
                cursor: 0,
                awaiting_state: None,
                cleanups: Vec::new(),
                status,
                queued: false,
                marked: false,
                timer_deadline: None,
                host_io: false,
                contract_state: None,
                join_mode: TaskJoinMode::All,
                join_dynamic: false,
                join_combined: false,
                join_winner: None,
                cancel_requested: false,
            },
        );
        self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
        self.gc_stats.live = self.tasks.len() as u64;
        Ok(Value::Task { id: task_id })
    }

    fn spawn_socket_io_task(
        &mut self,
        socket: TcpStream,
        operation: SocketIoOperation,
        fallible: bool,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let fault_code = operation.fault_code();
        if let Err(error) = socket.set_nonblocking(true) {
            return if fallible {
                let error = host_io_error(error);
                self.spawn_io_error_task(error.kind, error.message, span)
            } else {
                self.spawn_terminal_task(Err(io_failure(fault_code, &error, span)), span)
            };
        }
        let task_id = self.next_task;
        let next_task = self.next_task.checked_add(1).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "TaskIdExhausted",
                "async task identity space was exhausted",
                span,
            ))
        })?;
        let registration = match self.register_socket_wait(&socket, operation.interests()) {
            Ok(registration) => registration,
            Err(error) => {
                return if fallible {
                    let error = host_io_error(error);
                    self.spawn_io_error_task(error.kind, error.message, span)
                } else {
                    self.spawn_terminal_task(Err(io_failure(fault_code, &error, span)), span)
                };
            }
        };
        self.next_task = next_task;
        self.tasks.insert(
            task_id,
            ManagedTask {
                function: FunctionId(u32::MAX),
                frame: u64::MAX,
                parent: self.active_task,
                children: Vec::new(),
                cursor: 0,
                awaiting_state: None,
                cleanups: Vec::new(),
                status: TaskStatus::Waiting,
                queued: false,
                marked: false,
                timer_deadline: None,
                host_io: false,
                contract_state: None,
                join_mode: TaskJoinMode::All,
                join_dynamic: false,
                join_combined: false,
                join_winner: None,
                cancel_requested: false,
            },
        );
        self.socket_io.insert(
            task_id,
            PendingSocketIo {
                socket,
                registration,
                operation,
                fallible,
                span,
            },
        );
        self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
        self.gc_stats.live = self.tasks.len() as u64;
        Ok(Value::Task { id: task_id })
    }

    fn spawn_host_io_task<F>(&mut self, span: Span, work: F) -> Result<Value, ExecutionFailure>
    where
        F: FnOnce() -> Result<HostIoValue, ExecutionFailure> + Send + 'static,
    {
        let task_id = self.next_task;
        self.next_task = self.next_task.checked_add(1).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "TaskIdExhausted",
                "async task identity space was exhausted",
                span,
            ))
        })?;
        self.tasks.insert(
            task_id,
            ManagedTask {
                function: FunctionId(u32::MAX),
                frame: u64::MAX,
                parent: self.active_task,
                children: Vec::new(),
                cursor: 0,
                awaiting_state: None,
                cleanups: Vec::new(),
                status: TaskStatus::Waiting,
                queued: false,
                marked: false,
                timer_deadline: None,
                host_io: true,
                contract_state: None,
                join_mode: TaskJoinMode::All,
                join_dynamic: false,
                join_combined: false,
                join_winner: None,
                cancel_requested: false,
            },
        );
        let completion_sender = self.host_io_sender.clone();
        let job = Box::new(move || {
            let _ = completion_sender.send(HostIoCompletion {
                task: task_id,
                span,
                outcome: HostIoCompletionOutcome::Infallible(work()),
            });
        });
        if host_io_pool().try_send(job).is_err() {
            let failure = self.runtime_fault(
                "BlockingPoolSaturated",
                "bounded interpreter I/O worker queue is full",
                span,
            );
            self.tasks
                .get_mut(&task_id)
                .expect("host I/O task was just inserted")
                .status = TaskStatus::Failed(failure.into());
        }
        self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
        self.gc_stats.live = self.tasks.len() as u64;
        Ok(Value::Task { id: task_id })
    }

    fn spawn_try_host_io_task<F>(&mut self, span: Span, work: F) -> Result<Value, ExecutionFailure>
    where
        F: FnOnce() -> Result<HostIoValue, HostIoError> + Send + 'static,
    {
        let task_id = self.next_task;
        self.next_task = self.next_task.checked_add(1).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "TaskIdExhausted",
                "async task identity space was exhausted",
                span,
            ))
        })?;
        self.tasks.insert(
            task_id,
            ManagedTask {
                function: FunctionId(u32::MAX),
                frame: u64::MAX,
                parent: self.active_task,
                children: Vec::new(),
                cursor: 0,
                awaiting_state: None,
                cleanups: Vec::new(),
                status: TaskStatus::Waiting,
                queued: false,
                marked: false,
                timer_deadline: None,
                host_io: true,
                contract_state: None,
                join_mode: TaskJoinMode::All,
                join_dynamic: false,
                join_combined: false,
                join_winner: None,
                cancel_requested: false,
            },
        );
        let completion_sender = self.host_io_sender.clone();
        let job = Box::new(move || {
            let _ = completion_sender.send(HostIoCompletion {
                task: task_id,
                span,
                outcome: HostIoCompletionOutcome::Fallible(work()),
            });
        });
        if host_io_pool().try_send(job).is_err() {
            let failure = self.runtime_fault(
                "BlockingPoolSaturated",
                "bounded interpreter I/O worker queue is full",
                span,
            );
            self.tasks
                .get_mut(&task_id)
                .expect("host I/O task was just inserted")
                .status = TaskStatus::Failed(failure.into());
        }
        self.gc_stats.allocations = self.gc_stats.allocations.saturating_add(1);
        self.gc_stats.live = self.tasks.len() as u64;
        Ok(Value::Task { id: task_id })
    }

    fn spawn_io_error_task(
        &mut self,
        kind: u32,
        message: String,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let result = self.io_error_result(HostIoError { kind, message }, span)?;
        self.spawn_terminal_task(Ok(result), span)
    }

    fn io_error_result(&self, error: HostIoError, span: Span) -> Result<Value, ExecutionFailure> {
        let error_ty = self.program.prelude.io_error.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude IoError type is missing",
                span,
            )
        })?;
        let kind_ty = self.program.prelude.io_error_kind.ok_or_else(|| {
            self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "prelude IoErrorKind type is missing",
                span,
            )
        })?;
        self.result_value(
            false,
            Value::Record {
                ty: error_ty,
                fields: vec![
                    Value::Enum {
                        ty: kind_ty,
                        variant: VariantId(error.kind),
                        payload: Vec::new(),
                    },
                    Value::Text {
                        value: error.message,
                    },
                ],
            },
            span,
        )
    }

    fn checked_refine(
        &mut self,
        ty: TypeId,
        value: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let definition = self.program.type_def(ty).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "refined type does not exist",
                span,
            ))
        })?;
        let TypeDefKind::Refined { base, predicate } = &definition.kind else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "checked refinement targets a non-refined type",
                    span,
                )
                .into());
        };
        let context = ContractContext {
            receiver: Some(&value),
            result: None,
            arguments: &[],
            old_receiver: None,
            old_arguments: &[],
            bindings: &[],
        };
        let predicate_value = self.eval_contract(&predicate.expression, &context)?;
        let accepted = expect_bool(&predicate_value, span)?;
        if accepted {
            self.result_value(
                true,
                Value::Refined {
                    ty,
                    value: Box::new(value),
                },
                span,
            )
        } else {
            let constraint_error = ConstraintError {
                target_type: definition.name.clone(),
                code: "ConstraintViolation".into(),
                predicate: predicate.code.clone(),
                path: Vec::new(),
                value_summary: disclosure_type_summary(self.program, base),
                contract_span: predicate.span,
            };
            self.result_value(
                false,
                Value::ConstraintError {
                    value: constraint_error,
                },
                span,
            )
        }
    }

    fn rechecked_refine(
        &mut self,
        ty: TypeId,
        value: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let definition = self.program.type_def(ty).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "refined type does not exist",
                span,
            ))
        })?;
        let TypeDefKind::Refined { predicate, .. } = &definition.kind else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "proof recheck targets a non-refined type",
                    span,
                )
                .into());
        };
        let context = ContractContext {
            receiver: Some(&value),
            result: None,
            arguments: &[],
            old_receiver: None,
            old_arguments: &[],
            bindings: &[],
        };
        let predicate_value = self.eval_contract(&predicate.expression, &context)?;
        if expect_bool(&predicate_value, span)? {
            Ok(Value::Refined {
                ty,
                value: Box::new(value),
            })
        } else {
            Err(self
                .runtime_fault(
                    ARTIFACT_PROOF_REJECTED_FAULT_CODE,
                    ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE,
                    span,
                )
                .into())
        }
    }

    fn checked_record(
        &mut self,
        ty: TypeId,
        value: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let definition = self.program.type_def(ty).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "record type does not exist",
                span,
            ))
        })?;
        let TypeDefKind::Record { invariant, .. } = &definition.kind else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "record construction targets a non-record type",
                    span,
                )
                .into());
        };
        let Some(invariant) = invariant else {
            return Ok(value);
        };
        let context = ContractContext {
            receiver: Some(&value),
            result: None,
            arguments: &[],
            old_receiver: None,
            old_arguments: &[],
            bindings: &[],
        };
        let invariant_value = self.eval_contract(&invariant.expression, &context)?;
        let accepted = expect_bool(&invariant_value, span)?;
        if accepted {
            self.result_value(true, value, span)
        } else {
            let constraint_error = ConstraintError {
                target_type: definition.name.clone(),
                code: "InvariantViolation".into(),
                predicate: invariant.code.clone(),
                path: Vec::new(),
                value_summary: disclosure_type_summary(
                    self.program,
                    &Type::Nominal(ty, Vec::new()),
                ),
                contract_span: invariant.span,
            };
            self.result_value(
                false,
                Value::ConstraintError {
                    value: constraint_error,
                },
                span,
            )
        }
    }

    fn rechecked_record(
        &mut self,
        ty: TypeId,
        value: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let definition = self.program.type_def(ty).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "record type does not exist",
                span,
            ))
        })?;
        let TypeDefKind::Record {
            invariant: Some(invariant),
            ..
        } = &definition.kind
        else {
            return Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "proof recheck targets a record without an invariant",
                    span,
                )
                .into());
        };
        let context = ContractContext {
            receiver: Some(&value),
            result: None,
            arguments: &[],
            old_receiver: None,
            old_arguments: &[],
            bindings: &[],
        };
        let invariant_value = self.eval_contract(&invariant.expression, &context)?;
        if expect_bool(&invariant_value, span)? {
            Ok(value)
        } else {
            Err(self
                .runtime_fault(
                    ARTIFACT_PROOF_REJECTED_FAULT_CODE,
                    ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE,
                    span,
                )
                .into())
        }
    }

    fn result_value(
        &self,
        ok: bool,
        payload: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let result_type = self
            .program
            .prelude
            .result
            .and_then(|id| self.program.type_def(id))
            .ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "prelude Result type is missing",
                    span,
                ))
            })?;
        Ok(Value::Enum {
            ty: result_type.id,
            variant: VariantId(u32::from(!ok)),
            payload: vec![payload],
        })
    }

    fn eval_contract(
        &mut self,
        expression: &ContractExpr,
        context: &ContractContext<'_>,
    ) -> Result<Value, ExecutionFailure> {
        self.tick(expression.span)?;
        match &expression.kind {
            ContractExprKind::Constant(value) => Ok(Value::from(value.clone())),
            ContractExprKind::Value(value) => {
                contract_value(*value, context).cloned().ok_or_else(|| {
                    self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "contract referenced an unavailable value",
                        expression.span,
                    )
                    .into()
                })
            }
            ContractExprKind::Binding(index) => context
                .bindings
                .get(*index as usize)
                .cloned()
                .ok_or_else(|| {
                    self.runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "contract referenced an unavailable pattern binding",
                        expression.span,
                    )
                    .into()
                }),
            ContractExprKind::Field(value, field) => {
                let value = self.eval_contract(value, context)?;
                read_value_projection(&value, &[*field], expression.span)
            }
            ContractExprKind::Unary(operator, value) => {
                let value = self.eval_contract(value, context)?;
                self.eval_unary(*operator, value, expression.span)
            }
            ContractExprKind::Binary(BinaryOp::And, left, right) => {
                let left = self.eval_contract(left, context)?;
                if expect_bool(&left, expression.span)? {
                    let right = self.eval_contract(right, context)?;
                    Ok(Value::Bool {
                        value: expect_bool(&right, expression.span)?,
                    })
                } else {
                    Ok(Value::Bool { value: false })
                }
            }
            ContractExprKind::Binary(BinaryOp::Or, left, right) => {
                let left = self.eval_contract(left, context)?;
                if expect_bool(&left, expression.span)? {
                    Ok(Value::Bool { value: true })
                } else {
                    let right = self.eval_contract(right, context)?;
                    Ok(Value::Bool {
                        value: expect_bool(&right, expression.span)?,
                    })
                }
            }
            ContractExprKind::Binary(operator, left, right) => {
                let left = self.eval_contract(left, context)?;
                let right = self.eval_contract(right, context)?;
                self.eval_binary(*operator, left, right, expression.span)
            }
            ContractExprKind::IsFinite(value) => {
                let value = self.eval_contract(value, context)?;
                Ok(Value::Bool {
                    value: as_float(&value).is_some_and(f64::is_finite),
                })
            }
            ContractExprKind::Match { scrutinee, arms } => {
                let value = self.eval_contract(scrutinee, context)?;
                self.eval_contract_match(&value, arms, context, expression.span)
            }
        }
    }

    fn eval_contract_match(
        &mut self,
        value: &Value,
        arms: &[ContractArm],
        context: &ContractContext<'_>,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        for arm in arms {
            let mut arm_bindings = Vec::new();
            if pattern_matches(&arm.pattern, value, &mut arm_bindings) {
                if arm_bindings.len() != arm.bindings.len() {
                    return Err(self
                        .runtime_fault(
                            "LOOM_RUNTIME_INVALID_MIR",
                            "contract match binding count does not match pattern",
                            span,
                        )
                        .into());
                }
                let mut bindings = context.bindings.to_vec();
                bindings.extend(arm_bindings);
                let nested = ContractContext {
                    receiver: context.receiver,
                    result: context.result,
                    arguments: context.arguments,
                    old_receiver: context.old_receiver,
                    old_arguments: context.old_arguments,
                    bindings: &bindings,
                };
                return self.eval_contract(&arm.value, &nested);
            }
        }
        Err(self
            .runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "contract match was not exhaustive",
                span,
            )
            .into())
    }

    fn eval_unary(
        &self,
        operator: UnaryOp,
        value: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (operator, unrefined(value)) {
            (UnaryOp::Not, Value::Bool { value }) => Ok(Value::Bool { value: !value }),
            (UnaryOp::Negate, Value::Int { value }) => value.checked_neg().map_or_else(
                || {
                    Err(self
                        .runtime_fault(
                            INTEGER_OVERFLOW_FAULT_CODE,
                            INTEGER_OVERFLOW_FAULT_MESSAGE,
                            span,
                        )
                        .into())
                },
                |value| Ok(Value::Int { value }),
            ),
            (UnaryOp::Negate, Value::Float { value }) => Ok(Value::Float { value: -value }),
            _ => Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "invalid unary operation in checked MIR",
                    span,
                )
                .into()),
        }
    }

    fn eval_binary(
        &self,
        operator: BinaryOp,
        left: Value,
        right: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let left = unrefined(left);
        let right = unrefined(right);
        match operator {
            BinaryOp::Equal | BinaryOp::NotEqual => {
                let equal = semantic_equal(&left, &right);
                Ok(Value::Bool {
                    value: if operator == BinaryOp::Equal {
                        equal
                    } else {
                        !equal
                    },
                })
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                self.eval_arithmetic(operator, left, right, span)
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                self.eval_order(operator, left, right, span)
            }
            BinaryOp::And | BinaryOp::Or => Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "logical operation reached non-short-circuit evaluator",
                    span,
                )
                .into()),
        }
    }

    fn eval_arithmetic(
        &self,
        operator: BinaryOp,
        left: Value,
        right: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        match (left, right) {
            (Value::Int { value: left }, Value::Int { value: right }) => {
                if operator == BinaryOp::Divide && right == 0 {
                    return Err(self
                        .runtime_fault("IntegerDivisionByZero", "integer division by zero", span)
                        .into());
                }
                if operator == BinaryOp::Divide && left == i64::MIN && right == -1 {
                    return Err(self
                        .runtime_fault(
                            "IntegerDivisionOverflow",
                            "integer division overflowed",
                            span,
                        )
                        .into());
                }
                let result = match operator {
                    BinaryOp::Add => left.checked_add(right),
                    BinaryOp::Subtract => left.checked_sub(right),
                    BinaryOp::Multiply => left.checked_mul(right),
                    BinaryOp::Divide => left.checked_div(right),
                    _ => None,
                };
                result.map_or_else(
                    || {
                        Err(self
                            .runtime_fault(
                                INTEGER_OVERFLOW_FAULT_CODE,
                                INTEGER_OVERFLOW_FAULT_MESSAGE,
                                span,
                            )
                            .into())
                    },
                    |value| Ok(Value::Int { value }),
                )
            }
            (Value::Float { value: left }, Value::Float { value: right }) => {
                let value = match operator {
                    BinaryOp::Add => left + right,
                    BinaryOp::Subtract => left - right,
                    BinaryOp::Multiply => left * right,
                    BinaryOp::Divide => left / right,
                    _ => unreachable!(),
                };
                Ok(Value::Float { value })
            }
            _ => Err(self
                .runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "invalid arithmetic in checked MIR",
                    span,
                )
                .into()),
        }
    }

    fn eval_order(
        &self,
        operator: BinaryOp,
        left: Value,
        right: Value,
        span: Span,
    ) -> Result<Value, ExecutionFailure> {
        let value = match (left, right) {
            (Value::Int { value: left }, Value::Int { value: right }) => match operator {
                BinaryOp::Less => left < right,
                BinaryOp::LessEqual => left <= right,
                BinaryOp::Greater => left > right,
                BinaryOp::GreaterEqual => left >= right,
                _ => unreachable!(),
            },
            (Value::Float { value: left }, Value::Float { value: right }) => match operator {
                BinaryOp::Less => left < right,
                BinaryOp::LessEqual => left <= right,
                BinaryOp::Greater => left > right,
                BinaryOp::GreaterEqual => left >= right,
                _ => unreachable!(),
            },
            _ => {
                return Err(self
                    .runtime_fault(
                        "LOOM_RUNTIME_INVALID_MIR",
                        "invalid comparison in checked MIR",
                        span,
                    )
                    .into());
            }
        };
        Ok(Value::Bool { value })
    }

    fn read_place(&self, location: &Location, span: Span) -> Result<Value, ExecutionFailure> {
        let (root, mut projection) = self.resolve_location(location, span)?;
        projection.extend_from_slice(&location.projection);
        let frame = self.frames.get(&root.frame).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_DANGLING_VIEW",
                "place refers to a frame that is no longer alive",
                span,
            ))
        })?;
        let slot = frame.slots.get(root.local.0 as usize).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "local does not exist",
                span,
            ))
        })?;
        match slot {
            Slot::Value(value) => read_value_projection(value, &projection, span),
            Slot::Empty => Err(self
                .runtime_fault("LOOM_RUNTIME_UNINITIALIZED", "local is uninitialized", span)
                .into()),
            Slot::Moved => Err(self
                .runtime_fault("LOOM_RUNTIME_USE_AFTER_MOVE", "value has been moved", span)
                .into()),
            Slot::Alias(_) => unreachable!("aliases are removed by resolve_location"),
        }
    }

    fn write_place(
        &mut self,
        location: &Location,
        value: Value,
        span: Span,
    ) -> Result<(), ExecutionFailure> {
        let (root, mut projection) = self.resolve_location(location, span)?;
        projection.extend_from_slice(&location.projection);
        let frame = self
            .frames
            .get_mut(&root.frame)
            .ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_DANGLING_VIEW".into(),
                message: "place refers to a frame that is no longer alive".into(),
                span,
            })?;
        let slot = frame
            .slots
            .get_mut(root.local.0 as usize)
            .ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "local does not exist".into(),
                span,
            })?;
        if projection.is_empty() {
            *slot = Slot::Value(value);
            return Ok(());
        }
        match slot {
            Slot::Value(root_value) => write_value_projection(root_value, &projection, value, span),
            Slot::Empty => Err(RuntimeFault {
                code: "LOOM_RUNTIME_UNINITIALIZED".into(),
                message: "local is uninitialized".into(),
                span,
            }
            .into()),
            Slot::Moved => Err(RuntimeFault {
                code: "LOOM_RUNTIME_USE_AFTER_MOVE".into(),
                message: "value has been moved".into(),
                span,
            }
            .into()),
            Slot::Alias(_) => unreachable!("aliases are removed by resolve_location"),
        }
    }

    fn take_place(&mut self, location: &Location, span: Span) -> Result<Value, ExecutionFailure> {
        let (root, mut projection) = self.resolve_location(location, span)?;
        projection.extend_from_slice(&location.projection);
        let frame = self
            .frames
            .get_mut(&root.frame)
            .ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_DANGLING_VIEW".into(),
                message: "place refers to a frame that is no longer alive".into(),
                span,
            })?;
        let slot = frame
            .slots
            .get_mut(root.local.0 as usize)
            .ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "local does not exist".into(),
                span,
            })?;
        match std::mem::replace(slot, Slot::Moved) {
            Slot::Value(value) => take_value_projection(value, &projection, span),
            Slot::Empty => Err(RuntimeFault {
                code: "LOOM_RUNTIME_UNINITIALIZED".into(),
                message: "local is uninitialized".into(),
                span,
            }
            .into()),
            Slot::Moved => Err(RuntimeFault {
                code: "LOOM_RUNTIME_USE_AFTER_MOVE".into(),
                message: "value has already been moved".into(),
                span,
            }
            .into()),
            Slot::Alias(_) => unreachable!("aliases are removed by resolve_location"),
        }
    }

    fn resolve_location(
        &self,
        location: &Location,
        span: Span,
    ) -> Result<(Location, Vec<u32>), ExecutionFailure> {
        let mut current = Location::local(location.frame, location.local);
        let mut segments = Vec::new();
        for _ in 0..64 {
            let frame = self.frames.get(&current.frame).ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_DANGLING_VIEW",
                    "place refers to a frame that is no longer alive",
                    span,
                ))
            })?;
            let slot = frame.slots.get(current.local.0 as usize).ok_or_else(|| {
                ExecutionFailure::from(self.runtime_fault(
                    "LOOM_RUNTIME_INVALID_MIR",
                    "local does not exist",
                    span,
                ))
            })?;
            if let Slot::Alias(alias) = slot {
                segments.push(alias.projection.clone());
                current = Location {
                    frame: alias.frame,
                    local: alias.local,
                    projection: Vec::new(),
                };
            } else {
                segments.reverse();
                let prefix = segments.into_iter().flatten().collect();
                return Ok((current, prefix));
            }
        }
        Err(self
            .runtime_fault(
                "LOOM_RUNTIME_INVALID_MIR",
                "place alias chain is cyclic or too deep",
                span,
            )
            .into())
    }

    fn set_slot(
        &mut self,
        frame: u64,
        local: LocalId,
        value: Slot,
        span: Span,
    ) -> Result<(), ExecutionFailure> {
        let frame = self.frames.get_mut(&frame).ok_or_else(|| RuntimeFault {
            code: "LOOM_RUNTIME_INVALID_MIR".into(),
            message: "frame does not exist".into(),
            span,
        })?;
        let slot = frame
            .slots
            .get_mut(local.0 as usize)
            .ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "local does not exist".into(),
                span,
            })?;
        *slot = value;
        Ok(())
    }

    fn tick(&mut self, span: Span) -> Result<(), ExecutionFailure> {
        self.fuel = self.fuel.checked_sub(1).ok_or_else(|| {
            ExecutionFailure::from(self.runtime_fault(
                "LOOM_RUNTIME_FUEL_EXHAUSTED",
                "execution fuel was exhausted",
                span,
            ))
        })?;
        Ok(())
    }

    #[allow(clippy::unused_self)]
    fn runtime_fault(
        &self,
        code: impl Into<String>,
        message: impl Into<String>,
        span: Span,
    ) -> RuntimeFault {
        RuntimeFault {
            code: code.into(),
            message: message.into(),
            span,
        }
    }
}

impl Location {
    fn local(frame: u64, local: LocalId) -> Self {
        Self {
            frame,
            local,
            projection: Vec::new(),
        }
    }

    fn from_place(frame: u64, place: &Place) -> Self {
        Self {
            frame,
            local: place.local,
            projection: place.projection.clone(),
        }
    }
}

impl From<Constant> for Value {
    fn from(value: Constant) -> Self {
        match value {
            Constant::Unit => Self::Unit,
            Constant::Bool(value) => Self::Bool { value },
            Constant::Int(value) => Self::Int { value },
            Constant::Float(value) => Self::Float { value },
            Constant::Text(value) => Self::Text { value },
        }
    }
}

fn await_expr(expression: &Expr) -> Option<&Expr> {
    matches!(expression.kind, ExprKind::Await { .. }).then_some(expression)
}

fn task_terminal(status: &TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed(_) | TaskStatus::Failed(_) | TaskStatus::Cancelled
    )
}

fn contract_value<'a>(value: ContractValue, context: &'a ContractContext<'_>) -> Option<&'a Value> {
    match value {
        ContractValue::SelfValue => context.receiver,
        ContractValue::Result => context.result,
        ContractValue::Argument(index) => context.arguments.get(index as usize),
        ContractValue::OldSelf => context.old_receiver,
        ContractValue::OldArgument(index) => context
            .old_arguments
            .get(index as usize)
            .and_then(Option::as_ref),
    }
}

fn old_snapshot_needs(contracts: &[Contract], argument_count: usize) -> OldSnapshotNeeds {
    let mut needs = OldSnapshotNeeds {
        receiver: false,
        arguments: vec![false; argument_count],
    };
    for contract in contracts {
        collect_old_snapshot_needs(&contract.expression, &mut needs);
    }
    needs
}

fn collect_old_snapshot_needs(expression: &ContractExpr, needs: &mut OldSnapshotNeeds) {
    match &expression.kind {
        ContractExprKind::Value(ContractValue::OldSelf) => needs.receiver = true,
        ContractExprKind::Value(ContractValue::OldArgument(index)) => {
            if let Some(argument) = needs.arguments.get_mut(*index as usize) {
                *argument = true;
            }
        }
        ContractExprKind::Field(value, _)
        | ContractExprKind::Unary(_, value)
        | ContractExprKind::IsFinite(value) => collect_old_snapshot_needs(value, needs),
        ContractExprKind::Binary(_, left, right) => {
            collect_old_snapshot_needs(left, needs);
            collect_old_snapshot_needs(right, needs);
        }
        ContractExprKind::Match { scrutinee, arms } => {
            collect_old_snapshot_needs(scrutinee, needs);
            for arm in arms {
                collect_old_snapshot_needs(&arm.value, needs);
            }
        }
        ContractExprKind::Constant(_)
        | ContractExprKind::Value(_)
        | ContractExprKind::Binding(_) => {}
    }
}

fn unrefined(mut value: Value) -> Value {
    while let Value::Refined { value: inner, .. } = value {
        value = *inner;
    }
    value
}

fn owned_value_clone(value: &Value) -> Value {
    clone_value(value, true)
}

#[allow(clippy::too_many_lines)]
fn clone_value(value: &Value, clear_dyn_writeback: bool) -> Value {
    enum CloneFrame<'a> {
        Tuple {
            remaining: &'a [Value],
            cloned: Vec<Value>,
        },
        List {
            remaining: &'a [Value],
            cloned: Vec<Value>,
        },
        Record {
            ty: TypeId,
            remaining: &'a [Value],
            cloned: Vec<Value>,
        },
        Enum {
            ty: TypeId,
            variant: VariantId,
            remaining: &'a [Value],
            cloned: Vec<Value>,
        },
        Refined {
            ty: TypeId,
        },
        DynView {
            writeback: Option<Location>,
            witness: RuntimeWitness,
            mutable: bool,
            token: u32,
        },
        TaskOutcome,
    }

    let mut frames = Vec::new();
    let mut current = value;
    loop {
        let mut completed = match current {
            Value::Tuple { elements } => {
                if let Some((first, remaining)) = elements.split_first() {
                    frames.push(CloneFrame::Tuple {
                        remaining,
                        cloned: Vec::with_capacity(elements.len()),
                    });
                    current = first;
                    continue;
                }
                Value::Tuple {
                    elements: Vec::new(),
                }
            }
            Value::List { elements } => {
                if let Some((first, remaining)) = elements.split_first() {
                    frames.push(CloneFrame::List {
                        remaining,
                        cloned: Vec::with_capacity(elements.len()),
                    });
                    current = first;
                    continue;
                }
                Value::List {
                    elements: Vec::new(),
                }
            }
            Value::Record { ty, fields } => {
                if let Some((first, remaining)) = fields.split_first() {
                    frames.push(CloneFrame::Record {
                        ty: *ty,
                        remaining,
                        cloned: Vec::with_capacity(fields.len()),
                    });
                    current = first;
                    continue;
                }
                Value::Record {
                    ty: *ty,
                    fields: Vec::new(),
                }
            }
            Value::Enum {
                ty,
                variant,
                payload,
            } => {
                if let Some((first, remaining)) = payload.split_first() {
                    frames.push(CloneFrame::Enum {
                        ty: *ty,
                        variant: *variant,
                        remaining,
                        cloned: Vec::with_capacity(payload.len()),
                    });
                    current = first;
                    continue;
                }
                Value::Enum {
                    ty: *ty,
                    variant: *variant,
                    payload: Vec::new(),
                }
            }
            Value::Refined { ty, value } => {
                frames.push(CloneFrame::Refined { ty: *ty });
                current = value;
                continue;
            }
            Value::DynView {
                value,
                writeback,
                witness,
                mutable,
                token,
            } => {
                frames.push(CloneFrame::DynView {
                    writeback: if clear_dyn_writeback {
                        None
                    } else {
                        writeback.clone()
                    },
                    witness: witness.clone(),
                    mutable: *mutable,
                    token: *token,
                });
                current = value;
                continue;
            }
            Value::TaskOutcome {
                outcome: TaskOutcomeValue::Completed(value),
            } => {
                frames.push(CloneFrame::TaskOutcome);
                current = value;
                continue;
            }
            Value::Unit => Value::Unit,
            Value::Bool { value } => Value::Bool { value: *value },
            Value::Int { value } => Value::Int { value: *value },
            Value::Float { value } => Value::Float { value: *value },
            Value::Text { value } => Value::Text {
                value: value.clone(),
            },
            Value::Bytes { value } => Value::Bytes {
                value: value.clone(),
            },
            Value::ConstraintError { value } => Value::ConstraintError {
                value: value.clone(),
            },
            Value::Task { id } => Value::Task { id: *id },
            Value::TaskJoin {
                mode,
                tasks,
                dynamic,
            } => Value::TaskJoin {
                mode: *mode,
                tasks: tasks.clone(),
                dynamic: *dynamic,
            },
            Value::TaskOutcome {
                outcome: TaskOutcomeValue::Faulted,
            } => Value::TaskOutcome {
                outcome: TaskOutcomeValue::Faulted,
            },
            Value::TaskOutcome {
                outcome: TaskOutcomeValue::Cancelled,
            } => Value::TaskOutcome {
                outcome: TaskOutcomeValue::Cancelled,
            },
        };

        loop {
            let Some(frame) = frames.pop() else {
                return completed;
            };
            match frame {
                CloneFrame::Tuple {
                    remaining,
                    mut cloned,
                } => {
                    cloned.push(completed);
                    if let Some((next, remaining)) = remaining.split_first() {
                        frames.push(CloneFrame::Tuple { remaining, cloned });
                        current = next;
                        break;
                    }
                    completed = Value::Tuple { elements: cloned };
                }
                CloneFrame::List {
                    remaining,
                    mut cloned,
                } => {
                    cloned.push(completed);
                    if let Some((next, remaining)) = remaining.split_first() {
                        frames.push(CloneFrame::List { remaining, cloned });
                        current = next;
                        break;
                    }
                    completed = Value::List { elements: cloned };
                }
                CloneFrame::Record {
                    ty,
                    remaining,
                    mut cloned,
                } => {
                    cloned.push(completed);
                    if let Some((next, remaining)) = remaining.split_first() {
                        frames.push(CloneFrame::Record {
                            ty,
                            remaining,
                            cloned,
                        });
                        current = next;
                        break;
                    }
                    completed = Value::Record { ty, fields: cloned };
                }
                CloneFrame::Enum {
                    ty,
                    variant,
                    remaining,
                    mut cloned,
                } => {
                    cloned.push(completed);
                    if let Some((next, remaining)) = remaining.split_first() {
                        frames.push(CloneFrame::Enum {
                            ty,
                            variant,
                            remaining,
                            cloned,
                        });
                        current = next;
                        break;
                    }
                    completed = Value::Enum {
                        ty,
                        variant,
                        payload: cloned,
                    };
                }
                CloneFrame::Refined { ty } => {
                    completed = Value::Refined {
                        ty,
                        value: Box::new(completed),
                    };
                }
                CloneFrame::DynView {
                    writeback,
                    witness,
                    mutable,
                    token,
                } => {
                    completed = Value::DynView {
                        value: Box::new(completed),
                        writeback,
                        witness,
                        mutable,
                        token,
                    };
                }
                CloneFrame::TaskOutcome => {
                    completed = Value::TaskOutcome {
                        outcome: TaskOutcomeValue::Completed(Box::new(completed)),
                    };
                }
            }
        }
    }
}

fn as_float(value: &Value) -> Option<f64> {
    match value {
        Value::Float { value } => Some(*value),
        Value::Refined { value, .. } => as_float(value),
        _ => None,
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Int { value } => Some(*value),
        Value::Refined { value, .. } => as_int(value),
        _ => None,
    }
}

fn float_to_int_status(value: f64) -> (i64, i64) {
    const LOWER: f64 = -9_223_372_036_854_775_808.0;
    const UPPER_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    if !value.is_finite() {
        return (0, 1);
    }
    if !(LOWER..UPPER_EXCLUSIVE).contains(&value) {
        return (0, 2);
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "finite bounds and explicit truncation define the checked conversion contract"
    )]
    (value.trunc() as i64, 0)
}

#[derive(Clone, Copy)]
enum ParseFloatFailure {
    InvalidSyntax,
    OutOfRange,
}

fn parse_float(text: &str) -> Result<f64, ParseFloatFailure> {
    match text {
        "NaN" => return Ok(f64::from_bits(0x7ff8_0000_0000_0000)),
        "Infinity" => return Ok(f64::INFINITY),
        "-Infinity" => return Ok(f64::NEG_INFINITY),
        _ => {}
    }
    if !is_float_text(text) {
        return Err(ParseFloatFailure::InvalidSyntax);
    }
    let value = text
        .parse::<f64>()
        .map_err(|_| ParseFloatFailure::InvalidSyntax)?;
    if value.is_infinite() {
        Err(ParseFloatFailure::OutOfRange)
    } else {
        Ok(value)
    }
}

fn is_float_text(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut index = usize::from(bytes.first() == Some(&b'-'));
    let integer_start = index;
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if index == integer_start {
        return false;
    }

    let mut has_decimal = false;
    if bytes.get(index) == Some(&b'.') {
        has_decimal = true;
        index += 1;
        let fractional_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == fractional_start {
            return false;
        }
    }

    let mut has_exponent = false;
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        has_exponent = true;
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let exponent_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == exponent_start {
            return false;
        }
    }

    index == bytes.len() && (has_decimal || has_exponent)
}

/// Formats a binary64 value using the language's canonical textual boundary.
/// Rust's formatter already emits a shortest round-tripping finite decimal;
/// Loom additionally keeps integral finite values lexically distinguishable
/// from `Int` and gives special values their specified spellings.
fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value == f64::INFINITY {
        return "Infinity".into();
    }
    if value == f64::NEG_INFINITY {
        return "-Infinity".into();
    }

    let mut formatted = value.to_string();
    if !formatted.contains(['.', 'e', 'E']) {
        formatted.push_str(".0");
    }
    formatted
}

fn expect_bool(value: &Value, span: Span) -> Result<bool, ExecutionFailure> {
    if let Value::Bool { value } = value {
        Ok(*value)
    } else {
        Err(RuntimeFault {
            code: "LOOM_RUNTIME_INVALID_MIR".into(),
            message: "checked Bool expression produced another value".into(),
            span,
        }
        .into())
    }
}

fn referenced_task_ids(value: &Value, referenced: &mut Vec<u64>) {
    match value {
        Value::Task { id } => referenced.push(*id),
        Value::TaskJoin { tasks, .. } => referenced.extend(tasks),
        Value::Tuple { elements } | Value::List { elements } => {
            for element in elements {
                referenced_task_ids(element, referenced);
            }
        }
        Value::Record { fields, .. } => {
            for field in fields {
                referenced_task_ids(field, referenced);
            }
        }
        Value::Enum { payload, .. } => {
            for value in payload {
                referenced_task_ids(value, referenced);
            }
        }
        Value::Refined { value, .. } | Value::DynView { value, .. } => {
            referenced_task_ids(value, referenced);
        }
        Value::TaskOutcome {
            outcome: TaskOutcomeValue::Completed(value),
        } => referenced_task_ids(value, referenced),
        Value::Unit
        | Value::Bool { .. }
        | Value::Int { .. }
        | Value::Float { .. }
        | Value::Text { .. }
        | Value::Bytes { .. }
        | Value::ConstraintError { .. }
        | Value::TaskOutcome {
            outcome: TaskOutcomeValue::Faulted | TaskOutcomeValue::Cancelled,
        } => {}
    }
}

#[allow(clippy::float_cmp)] // IEEE equality is the language rule, not an approximation.
fn semantic_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool { value: left }, Value::Bool { value: right }) => left == right,
        (Value::Int { value: left }, Value::Int { value: right }) => left == right,
        (Value::Float { value: left }, Value::Float { value: right }) => left == right,
        (Value::Text { value: left }, Value::Text { value: right }) => left == right,
        (Value::Bytes { value: left }, Value::Bytes { value: right }) => left == right,
        (Value::Tuple { elements: left }, Value::Tuple { elements: right })
        | (Value::List { elements: left }, Value::List { elements: right }) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| semantic_equal(left, right))
        }
        (
            Value::Record {
                ty: left_ty,
                fields: left,
            },
            Value::Record {
                ty: right_ty,
                fields: right,
            },
        ) => {
            left_ty == right_ty
                && left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| semantic_equal(left, right))
        }
        (
            Value::Enum {
                ty: left_ty,
                variant: left_variant,
                payload: left,
            },
            Value::Enum {
                ty: right_ty,
                variant: right_variant,
                payload: right,
            },
        ) => {
            left_ty == right_ty
                && left_variant == right_variant
                && left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| semantic_equal(left, right))
        }
        (
            Value::Refined {
                ty: left_ty,
                value: left,
            },
            Value::Refined {
                ty: right_ty,
                value: right,
            },
        ) => left_ty == right_ty && semantic_equal(left, right),
        (Value::ConstraintError { value: left }, Value::ConstraintError { value: right }) => {
            left == right
        }
        (Value::TaskOutcome { outcome: left }, Value::TaskOutcome { outcome: right }) => {
            match (left, right) {
                (TaskOutcomeValue::Completed(left), TaskOutcomeValue::Completed(right)) => {
                    semantic_equal(left, right)
                }
                (TaskOutcomeValue::Faulted, TaskOutcomeValue::Faulted)
                | (TaskOutcomeValue::Cancelled, TaskOutcomeValue::Cancelled) => true,
                _ => false,
            }
        }
        _ => false,
    }
}

fn pattern_matches(pattern: &Pattern, value: &Value, bindings: &mut Vec<Value>) -> bool {
    match pattern {
        Pattern::Wildcard => true,
        Pattern::Binding => {
            bindings.push(value.clone());
            true
        }
        Pattern::Constant(constant) => semantic_equal(&Value::from(constant.clone()), value),
        Pattern::Variant {
            ty,
            variant,
            payload,
        } => {
            let Value::Enum {
                ty: value_ty,
                variant: value_variant,
                payload: values,
            } = value
            else {
                return false;
            };
            *ty == *value_ty
                && *variant == *value_variant
                && payload.len() == values.len()
                && payload
                    .iter()
                    .zip(values)
                    .all(|(pattern, value)| pattern_matches(pattern, value, bindings))
        }
    }
}

fn record_descriptor(fields: &[Value], span: Span) -> Result<i64, EvalAbort> {
    match fields.first() {
        Some(Value::Int { value }) => Ok(*value),
        _ => Err(EvalAbort::from(RuntimeFault {
            code: "LOOM_RUNTIME_INVALID_MIR".into(),
            message: "opaque standard value is missing its raw integer field".into(),
            span,
        })),
    }
}

fn checked_resource_handle(fields: &[Value], span: Span) -> Result<u64, ExecutionFailure> {
    let value = match fields.first() {
        Some(Value::Int { value }) => *value,
        _ => {
            return Err(RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "resource value is missing its descriptor".into(),
                span,
            }
            .into());
        }
    };
    if value < 0 {
        return Err(RuntimeFault {
            code: "ResourceClosed".into(),
            message: "resource is already closed".into(),
            span,
        }
        .into());
    }
    u64::try_from(value).map_err(|_| {
        RuntimeFault {
            code: "InvalidResourceHandle".into(),
            message: "resource handle exceeds the interpreter range".into(),
            span,
        }
        .into()
    })
}

fn io_failure(code: &str, error: &std::io::Error, span: Span) -> ExecutionFailure {
    RuntimeFault {
        code: code.into(),
        message: error.to_string(),
        span,
    }
    .into()
}

fn poll_socket_operation(pending: &mut PendingSocketIo) -> SocketIoPoll {
    match &mut pending.operation {
        SocketIoOperation::Read { bytes } => {
            let mut consumed = 0_usize;
            let mut buffer = [0_u8; 8 * 1024];
            while consumed < SOCKET_IO_BUDGET {
                let capacity = buffer.len().min(SOCKET_IO_BUDGET - consumed);
                match pending.socket.read(&mut buffer[..capacity]) {
                    Ok(0) => {
                        return match String::from_utf8(std::mem::take(bytes)) {
                            Ok(value) => SocketIoPoll::Completed(Value::Text { value }),
                            Err(_) => SocketIoPoll::Failed(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "socket response is not valid UTF-8",
                            )),
                        };
                    }
                    Ok(count) => {
                        bytes.extend_from_slice(&buffer[..count]);
                        consumed += count;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return SocketIoPoll::Pending;
                    }
                    Err(error) => return SocketIoPoll::Failed(error),
                }
            }
            SocketIoPoll::Pending
        }
        SocketIoOperation::Write { bytes, offset } => {
            let mut consumed = 0_usize;
            while *offset < bytes.len() && consumed < SOCKET_IO_BUDGET {
                let end = bytes.len().min(*offset + (SOCKET_IO_BUDGET - consumed));
                match pending.socket.write(&bytes[*offset..end]) {
                    Ok(0) => {
                        return SocketIoPoll::Failed(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "socket write made no progress",
                        ));
                    }
                    Ok(count) => {
                        *offset += count;
                        consumed += count;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        return SocketIoPoll::Pending;
                    }
                    Err(error) => return SocketIoPoll::Failed(error),
                }
            }
            if *offset == bytes.len() {
                SocketIoPoll::Completed(Value::Unit)
            } else {
                SocketIoPoll::Pending
            }
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // also serves directly as a `map_err` adapter
fn host_io_error(error: std::io::Error) -> HostIoError {
    use std::io::ErrorKind;
    let kind = match error.kind() {
        ErrorKind::NotFound => 0,
        ErrorKind::PermissionDenied => 1,
        ErrorKind::AlreadyExists => 2,
        ErrorKind::InvalidInput | ErrorKind::InvalidData | ErrorKind::Unsupported => 3,
        ErrorKind::ConnectionRefused => 4,
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe => 5,
        ErrorKind::TimedOut | ErrorKind::WouldBlock => 6,
        ErrorKind::UnexpectedEof => 7,
        ErrorKind::NotConnected => 8,
        _ => 9,
    };
    HostIoError {
        kind,
        message: error.to_string(),
    }
}

fn resource_closed(name: &str, span: Span) -> ExecutionFailure {
    RuntimeFault {
        code: "ResourceClosed".into(),
        message: format!("{name} is already closed"),
        span,
    }
    .into()
}

fn read_value_projection(
    value: &Value,
    projection: &[u32],
    span: Span,
) -> Result<Value, ExecutionFailure> {
    let mut current = value;
    for field in projection {
        while let Value::Refined { value, .. } = current {
            current = value;
        }
        match current {
            Value::Record { fields, .. } => {
                current = fields.get(*field as usize).ok_or_else(|| RuntimeFault {
                    code: "LOOM_RUNTIME_INVALID_MIR".into(),
                    message: "field projection is out of bounds".into(),
                    span,
                })?;
            }
            _ => {
                return Err(RuntimeFault {
                    code: "LOOM_RUNTIME_INVALID_MIR".into(),
                    message: "field projection targets a non-record value".into(),
                    span,
                }
                .into());
            }
        }
    }
    Ok(current.clone())
}

fn take_value_projection(
    mut value: Value,
    projection: &[u32],
    span: Span,
) -> Result<Value, ExecutionFailure> {
    for field in projection {
        value = unrefined(value);
        let Value::Record { fields, .. } = value else {
            return Err(RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "field projection targets a non-record value".into(),
                span,
            }
            .into());
        };
        value = fields
            .into_iter()
            .nth(*field as usize)
            .ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "field projection is out of bounds".into(),
                span,
            })?;
    }
    Ok(value)
}

fn write_value_projection(
    value: &mut Value,
    projection: &[u32],
    replacement: Value,
    span: Span,
) -> Result<(), ExecutionFailure> {
    let Some((&field, remainder)) = projection.split_first() else {
        *value = replacement;
        return Ok(());
    };
    match value {
        Value::Record { fields, .. } => {
            let field = fields.get_mut(field as usize).ok_or_else(|| RuntimeFault {
                code: "LOOM_RUNTIME_INVALID_MIR".into(),
                message: "field projection is out of bounds".into(),
                span,
            })?;
            write_value_projection(field, remainder, replacement, span)
        }
        _ => Err(RuntimeFault {
            code: "LOOM_RUNTIME_INVALID_MIR".into(),
            message: "field projection targets a non-record value".into(),
            span,
        }
        .into()),
    }
}

fn test_value_passed(value: &Value) -> bool {
    match value {
        Value::Unit => true,
        Value::Enum {
            variant, payload, ..
        } => variant.0 == 0 && matches!(payload.as_slice(), [Value::Unit]),
        _ => false,
    }
}

#[cfg(test)]
mod scoped_cleanup_tests {
    use super::*;
    use loom_mir::{CallPlan, ConstructionMode, FieldDef, LocalDecl, Statement, Type, TypeDef};

    fn expression(kind: ExprKind, ty: Type) -> Expr {
        Expr::new(kind, ty, Span::default())
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn faulting_scoped_initializer_does_not_register_its_disposal() {
        let file_ty = Type::Nominal(TypeId(0), Vec::new());
        let initializer = expression(
            ExprKind::Block(Block {
                statements: vec![Statement {
                    kind: StatementKind::Assert {
                        condition: expression(
                            ExprKind::Constant(Constant::Bool(false)),
                            Type::Bool,
                        ),
                    },
                    span: Span::default(),
                }],
                tail: Some(Box::new(expression(
                    ExprKind::Record {
                        ty: TypeId(0),
                        type_arguments: Vec::new(),
                        fields: vec![expression(ExprKind::Constant(Constant::Int(9)), Type::Int)],
                        construction: ConstructionMode::Plain,
                    },
                    file_ty.clone(),
                ))),
                span: Span::default(),
            }),
            file_ty.clone(),
        );
        let mut program = Program {
            types: vec![TypeDef {
                id: TypeId(0),
                name: "File".into(),
                span: Span::default(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".into(),
                        ty: Type::Int,
                        span: Span::default(),
                    }],
                    invariant: None,
                },
            }],
            functions: vec![Function {
                id: FunctionId(0),
                name: "main".into(),
                span: Span::default(),
                type_parameters: 0,
                is_async: false,
                suspension_points: Vec::new(),
                params: Vec::new(),
                witness_params: Vec::new(),
                witness_prefix_count: 0,
                locals: vec![LocalDecl {
                    id: LocalId(0),
                    name: "file".into(),
                    ty: file_ty.clone(),
                    mutable: true,
                    span: Span::default(),
                }],
                return_ty: Type::Unit,
                receiver: None,
                body: Block {
                    statements: vec![Statement {
                        kind: StatementKind::Scoped {
                            local: LocalId(0),
                            value: initializer,
                            disposal: ScopedDisposal::FileClose,
                        },
                        span: Span::default(),
                    }],
                    tail: Some(Box::new(expression(
                        ExprKind::Constant(Constant::Unit),
                        Type::Unit,
                    ))),
                    span: Span::default(),
                },
                call_plan: CallPlan::default(),
            }],
            prelude: loom_mir::PreludeIds {
                file: Some(TypeId(0)),
                ..loom_mir::PreludeIds::default()
            },
            ..Program::default()
        };
        program
            .renumber_expr_ids()
            .expect("renumber scoped initializer fixture");
        let program = program
            .into_checked()
            .expect("valid checked scoped initializer fixture");
        let body = program.functions[0].body.clone();
        let mut interpreter = Interpreter::new(&program);
        interpreter.frames.insert(
            1,
            Frame {
                // A stale slot makes an incorrectly pre-registered cleanup
                // observable without relying on language-level aliasing.
                slots: vec![Slot::Value(Value::Record {
                    ty: TypeId(0),
                    fields: vec![Value::Int { value: 9 }],
                })],
                witnesses: Vec::new(),
            },
        );
        interpreter
            .files
            .insert(9, tempfile::tempfile().expect("create file handle fixture"));

        let outcome = interpreter.eval_block(1, &body);
        assert!(matches!(outcome, Err(EvalAbort::Failure(_))));
        assert!(
            interpreter.files.contains_key(&9),
            "a Scoped disposal must register only after its initializer succeeds"
        );
    }
}

#[cfg(test)]
mod location_projection_tests {
    use super::*;

    #[test]
    fn alias_chain_projections_are_resolved_from_root_to_leaf() {
        let program = Program::default()
            .into_checked()
            .expect("empty checked-MIR fixture");
        let mut interpreter = Interpreter::new(&program);
        interpreter.frames.insert(
            1,
            Frame {
                slots: vec![Slot::Value(Value::Record {
                    ty: TypeId(0),
                    fields: vec![
                        Value::Int { value: 7 },
                        Value::Record {
                            ty: TypeId(1),
                            fields: vec![Value::Int { value: 11 }, Value::Int { value: 29 }],
                        },
                    ],
                })],
                witnesses: Vec::new(),
            },
        );
        interpreter.frames.insert(
            2,
            Frame {
                slots: vec![Slot::Alias(Location {
                    frame: 1,
                    local: LocalId(0),
                    projection: vec![1],
                })],
                witnesses: Vec::new(),
            },
        );
        interpreter.frames.insert(
            3,
            Frame {
                slots: vec![Slot::Alias(Location {
                    frame: 2,
                    local: LocalId(0),
                    projection: vec![0],
                })],
                witnesses: Vec::new(),
            },
        );

        let value = interpreter
            .read_place(&Location::local(3, LocalId(0)), Span::default())
            .expect("resolve nested projected receiver");
        assert_eq!(value, Value::Int { value: 11 });
    }
}

#[cfg(test)]
mod socket_readiness_tests {
    use super::*;

    #[test]
    fn cancelled_pending_socket_reads_do_not_starve_file_workers() {
        const PENDING_READS: usize = 8;

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("bind interpreter readiness fixture");
        let address = listener.local_addr().expect("fixture address");
        let program = Program::default()
            .into_checked()
            .expect("empty checked-MIR fixture");
        let mut interpreter = Interpreter::new(&program);
        let mut peers = Vec::with_capacity(PENDING_READS);
        let mut reads = Vec::with_capacity(PENDING_READS);
        for _ in 0..PENDING_READS {
            let client = TcpStream::connect(address).expect("connect readiness fixture");
            let (peer, _) = listener.accept().expect("accept readiness fixture");
            peers.push(peer);
            let Value::Task { id } = interpreter
                .spawn_socket_io_task(
                    client,
                    SocketIoOperation::Read { bytes: Vec::new() },
                    false,
                    Span::default(),
                )
                .expect("spawn pending socket read")
            else {
                panic!("socket read must produce a task");
            };
            reads.push(id);
        }
        assert_eq!(interpreter.socket_io.len(), PENDING_READS);
        assert!(!interpreter.poll_socket_io(Some(Duration::ZERO)));

        for task in &reads {
            interpreter.cancel_task(*task);
        }
        assert!(interpreter.socket_io.is_empty());
        for task in reads {
            interpreter.remove_ready_task(task);
            assert!(matches!(
                interpreter.resume_task(task).expect("finish cancellation"),
                TaskPoll::Failed
            ));
        }

        let timer = Expr {
            id: loom_mir::ExprId::UNASSIGNED,
            kind: ExprKind::Sleep {
                milliseconds: Box::new(Expr {
                    id: loom_mir::ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(Constant::Int(1_000)),
                    ty: loom_mir::Type::Int,
                    span: Span::default(),
                }),
            },
            ty: loom_mir::Type::Task(Box::new(loom_mir::Type::Unit)),
            span: Span::default(),
        };
        let Ok(Value::Task { id: timer_task }) = interpreter.eval_expr(0, &timer) else {
            panic!("sleep must produce a task");
        };
        assert_eq!(interpreter.ready.pop_front(), Some(timer_task));
        interpreter
            .tasks
            .get_mut(&timer_task)
            .expect("timer task exists")
            .queued = false;
        assert!(matches!(
            interpreter
                .resume_task(timer_task)
                .expect("arm regression timeout"),
            TaskPoll::Pending
        ));

        let io_fixture = tempfile::tempdir().expect("create portable I/O fixture");
        let empty_file = io_fixture.path().join("empty");
        std::fs::write(&empty_file, []).expect("write portable I/O fixture");
        let span = Span::default();
        let Value::Task { id: file_task } = interpreter
            .spawn_host_io_task(span, move || {
                std::fs::read(empty_file)
                    .map(|bytes| {
                        HostIoValue::Value(Value::Int {
                            value: i64::try_from(bytes.len()).expect("fixture length fits Int"),
                        })
                    })
                    .map_err(|error| io_failure("FileReadFault", &error, span))
            })
            .expect("spawn file task")
        else {
            panic!("file read must produce a task");
        };
        assert!(interpreter.wait_for_work());
        assert!(matches!(
            interpreter.tasks.get(&file_task).map(|task| &task.status),
            Some(TaskStatus::Completed(Value::Int { value: 0 }))
        ));
        drop(peers);
    }
}

#[cfg(test)]
mod builtin_value_tests {
    use super::*;
    use loom_mir::{FieldDef, Type, TypeDef, VariantDef};

    fn variant(id: u32, name: &str, payload: Vec<Type>) -> VariantDef {
        VariantDef {
            id: VariantId(id),
            name: name.into(),
            payload,
            span: Span::default(),
        }
    }

    fn builtin_program() -> Program {
        let mut types = Vec::new();
        types.push(TypeDef {
            id: TypeId(0),
            name: "Option".into(),
            span: Span::default(),
            type_parameters: 1,
            kind: TypeDefKind::Enum {
                variants: vec![
                    variant(0, "None", Vec::new()),
                    variant(1, "Some", vec![Type::Parameter(0)]),
                ],
            },
        });
        types.push(TypeDef {
            id: TypeId(1),
            name: "Result".into(),
            span: Span::default(),
            type_parameters: 2,
            kind: TypeDefKind::Enum {
                variants: vec![
                    variant(0, "Ok", vec![Type::Parameter(0)]),
                    variant(1, "Err", vec![Type::Parameter(1)]),
                ],
            },
        });
        for index in 2..9 {
            types.push(TypeDef {
                id: TypeId(index),
                name: format!("unused#{index}"),
                span: Span::default(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: Vec::new(),
                    invariant: None,
                },
            });
        }
        for (id, name) in [(9, "Bytes"), (10, "Path")] {
            types.push(TypeDef {
                id: TypeId(id),
                name: name.into(),
                span: Span::default(),
                type_parameters: 0,
                kind: TypeDefKind::Record {
                    fields: vec![FieldDef {
                        name: "raw".into(),
                        ty: Type::Text,
                        span: Span::default(),
                    }],
                    invariant: None,
                },
            });
        }
        types.push(TypeDef {
            id: TypeId(11),
            name: "DecodeTextError".into(),
            span: Span::default(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![variant(0, "InvalidUtf8", Vec::new())],
            },
        });
        types.push(TypeDef {
            id: TypeId(12),
            name: "PathError".into(),
            span: Span::default(),
            type_parameters: 0,
            kind: TypeDefKind::Enum {
                variants: vec![
                    variant(0, "ContainsNul", Vec::new()),
                    variant(1, "AbsoluteJoin", Vec::new()),
                ],
            },
        });
        Program {
            types,
            prelude: loom_mir::PreludeIds {
                option: Some(TypeId(0)),
                result: Some(TypeId(1)),
                bytes: Some(TypeId(9)),
                path: Some(TypeId(10)),
                decode_text_error: Some(TypeId(11)),
                path_error: Some(TypeId(12)),
                ..loom_mir::PreludeIds::default()
            },
            ..Program::default()
        }
    }

    #[test]
    fn unicode_scalars_invalid_utf8_and_lexical_paths_match_the_language_rules() {
        let program = builtin_program()
            .into_checked()
            .expect("valid standard-value checked-MIR fixture");
        let interpreter = Interpreter::new(&program);
        let span = Span::default();
        let text = Value::Text {
            value: "a界🙂".into(),
        };
        assert_eq!(
            interpreter
                .eval_builtin_value(Builtin::TextLength, std::slice::from_ref(&text), span)
                .unwrap(),
            Value::Int { value: 3 }
        );
        let scalar = interpreter
            .eval_builtin_value(Builtin::TextGet, &[text, Value::Int { value: 1 }], span)
            .unwrap();
        assert!(matches!(
            scalar,
            Value::Enum { variant: VariantId(1), payload, .. }
                if payload == vec![Value::Text { value: "界".into() }]
        ));

        let invalid = Value::Record {
            ty: TypeId(9),
            fields: vec![Value::Bytes {
                value: vec![0xff, 0xfe],
            }],
        };
        let decoded = interpreter
            .eval_builtin_value(Builtin::BytesDecodeUtf8, &[invalid], span)
            .unwrap();
        assert!(matches!(
            decoded,
            Value::Enum { ty: TypeId(1), variant: VariantId(1), payload, .. }
                if matches!(payload.as_slice(), [Value::Enum {
                    ty: TypeId(11), variant: VariantId(0), payload, ..
                }] if payload.is_empty())
        ));

        let valid_units = Value::List {
            elements: [65, 231, 149, 140]
                .into_iter()
                .map(|value| Value::Int { value })
                .collect(),
        };
        let rebuilt = interpreter
            .eval_builtin_value(Builtin::TextFromUtf8Units, &[valid_units], span)
            .unwrap();
        assert!(matches!(
            rebuilt,
            Value::Enum { ty: TypeId(1), variant: VariantId(0), payload, .. }
                if payload == vec![Value::Text { value: "A界".into() }]
        ));

        for invalid_units in [vec![-1], vec![256], vec![255]] {
            let invalid_units = Value::List {
                elements: invalid_units
                    .into_iter()
                    .map(|value| Value::Int { value })
                    .collect(),
            };
            let decoded = interpreter
                .eval_builtin_value(Builtin::TextFromUtf8Units, &[invalid_units], span)
                .unwrap();
            assert!(matches!(
                decoded,
                Value::Enum { ty: TypeId(1), variant: VariantId(1), payload, .. }
                    if matches!(payload.as_slice(), [Value::Enum {
                        ty: TypeId(11), variant: VariantId(0), payload, ..
                    }] if payload.is_empty())
            ));
        }

        let base = interpreter.path_value("root".into(), span).unwrap();
        let child = interpreter.path_value("child/file".into(), span).unwrap();
        let joined = interpreter
            .eval_builtin_value(Builtin::PathJoin, &[base, child], span)
            .unwrap();
        assert!(matches!(
            joined,
            Value::Enum { variant: VariantId(0), payload, .. }
                if matches!(payload.as_slice(), [Value::Record { fields, .. }]
                    if fields == &vec![Value::Text { value: "root/child/file".into() }])
        ));
    }

    #[test]
    fn process_argument_primitives_expose_the_snapshot_and_fail_closed_on_bad_indices() {
        let program = builtin_program()
            .into_checked()
            .expect("valid process checked-MIR fixture");
        let interpreter =
            Interpreter::new(&program).with_process_arguments(vec!["first".into(), "界🙂".into()]);
        let span = Span::default();

        assert_eq!(
            interpreter
                .eval_process_builtin(Builtin::ProcessArgumentCount, &[], span)
                .unwrap(),
            Value::Int { value: 2 }
        );
        assert_eq!(
            interpreter
                .eval_process_builtin(Builtin::ProcessArgumentAt, &[Value::Int { value: 1 }], span,)
                .unwrap(),
            Value::Text {
                value: "界🙂".into()
            }
        );

        for index in [-1, 2] {
            assert!(matches!(
                interpreter.eval_process_builtin(
                    Builtin::ProcessArgumentAt,
                    &[Value::Int { value: index }],
                    span,
                ),
                Err(ExecutionFailure::Defect { .. })
            ));
        }
    }
}
