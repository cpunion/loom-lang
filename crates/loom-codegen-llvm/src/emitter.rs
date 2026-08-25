use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::debug_info::{
    AsDIScope, DICompileUnit, DIFile, DIFlags, DIFlagsConstants, DWARFEmissionKind,
    DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{FlagBehavior, Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::FileType;
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType, PointerType, StructType,
};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, GlobalValue, IntValue, PointerValue,
    UnnamedAddress,
};
use inkwell::{FloatPredicate, IntPredicate};
use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, Constant, ConstructionMode, Contract,
    ContractArm, ContractExpr, ContractExprKind, ContractValue, Expr, ExprKind, Function,
    FunctionId, LocalId, MatchArm, Pattern, Place, Program, RequirementId, Statement,
    StatementKind, TaskJoinMode, Type, TypeDefKind, TypeId, UnaryOp, WitnessId, WitnessRef,
};
use loom_runtime_abi::{
    TEXT_OBJECT_ALIGNMENT, TEXT_OBJECT_FIELD_BYTE_LENGTH, TEXT_OBJECT_FIELD_BYTES,
    TEXT_OBJECT_FIELD_SCALAR_LENGTH, TEXT_OBJECT_HEADER_SIZE,
};

use crate::abi::{
    ARG_NODE_FIELD_NEXT, ARG_NODE_FIELD_VALUE, COROUTINE_ABI_VERSION, COROUTINE_FRAME_FIELD_RESULT,
    COROUTINE_FRAME_FIELD_STATE, DYN_FLAG_MUTABLE, DYN_FLAG_WRITEBACK, JOIN_RESULT_LIST,
    JOIN_RESULT_OUTCOME, JOIN_RESULT_OUTCOME_LIST, JOIN_RESULT_OUTCOME_TUPLE, JOIN_RESULT_SCALAR,
    JOIN_RESULT_TUPLE, READY_EVENT_COMPLETED, READY_EVENT_TIMER, READY_NOTIFICATION_FIELD_EVENTS,
    READY_NOTIFICATION_FIELD_FRAME, TASK_STEP_CANCELLED, TASK_STEP_COMPLETED, TASK_STEP_FAULTED,
    TASK_STEP_PENDING, TASK_VALUE_DIRECT, VALUE_FIELD_AUX, VALUE_FIELD_DATA, VALUE_FIELD_NOMINAL,
    VALUE_FIELD_SCALAR, VALUE_FIELD_TAG, VALUE_FIELD_WITNESS, VALUE_NODE_FIELD_NEXT,
    VALUE_NODE_FIELD_VALUE, VALUE_TAG_BOOL, VALUE_TAG_CONSTRAINT_ERROR, VALUE_TAG_DYN,
    VALUE_TAG_ENUM, VALUE_TAG_FLOAT, VALUE_TAG_INT, VALUE_TAG_LIST, VALUE_TAG_RECORD,
    VALUE_TAG_REFINED, VALUE_TAG_TASK, VALUE_TAG_TASK_OUTCOME, VALUE_TAG_TEXT, VALUE_TAG_TUPLE,
    VALUE_TAG_UNIT, WAIT_ABI_VERSION, WAIT_INTEREST_READABLE, WAIT_INTEREST_WRITABLE,
    WAIT_SOURCE_FIELD_ABI_VERSION, WAIT_SOURCE_FIELD_DEADLINE, WAIT_SOURCE_FIELD_HANDLE,
    WAIT_SOURCE_FIELD_INTERESTS, WAIT_SOURCE_FIELD_KIND, WAIT_SOURCE_FIELD_RESERVED,
    WAIT_SOURCE_KIND_COMPLETION, WAIT_SOURCE_KIND_FD, WAIT_SOURCE_KIND_TIMER,
    WITNESS_METHOD_FIELD_OFFSET, WITNESS_NODE_FIELD_NEXT, WITNESS_NODE_FIELD_VALUE,
};
use crate::codegen::{DebugSource, EmitKind, EmitOptions, NativeObjectArtifact};
use crate::native_layout::{
    NativeEffectAbi, NativeLayout, NativeScalar, NativeSignature, NativeSignatureShape,
};
use crate::native_storage::{NativeIntListAppendLoop, NativeIntListGetMatch, NativeIntListPlan};
use crate::requirements::RuntimeRequirementGraph;
use crate::target::create_target_machine;
use crate::{CodegenError, ReachableProgram, Roots};

pub(crate) struct Emitter;

const LIKELY_BRANCH_WEIGHT: u64 = 2_000;
const UNLIKELY_BRANCH_WEIGHT: u64 = 1;

#[derive(Clone, Copy)]
enum LikelyBranch {
    Then,
    Else,
}

fn native_fault_message(code: &str) -> &str {
    match code {
        "IntegerOverflow" => "integer arithmetic overflowed",
        "IntegerDivisionByZero" => "integer division by zero",
        "IntegerDivisionOverflow" => "integer division overflowed",
        "InvalidSleepDuration" => "sleep duration must not be negative",
        "SleepDurationOverflow" => "sleep duration overflowed",
        "InvalidPort" => "socket port must fit UInt16",
        "InvalidFileDescriptor" => "resource descriptor is invalid",
        "TaskAllocationFault" => "task allocation failed",
        "TaskJoinFault" => "task join failed",
        "ResourceCloseFault" => "resource close failed",
        _ => "runtime operation failed",
    }
}

fn needs_parameter_snapshots(function: &Function) -> bool {
    function.call_plan.receiver_invariant.is_some()
        || !function.call_plan.requires.is_empty()
        || !function.call_plan.ensures.is_empty()
}

fn block_contains_await(block: &Block) -> bool {
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expression_contains_await(value),
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                expression_contains_await(start)
                    || expression_contains_await(end)
                    || block_contains_await(body)
            }
            StatementKind::Assert { condition } => expression_contains_await(condition),
            StatementKind::Defer(cleanup) => block_contains_await(cleanup),
            StatementKind::Return(value) => value.as_ref().is_some_and(expression_contains_await),
        })
        || block.tail.as_deref().is_some_and(expression_contains_await)
}

fn expression_contains_await(expression: &Expr) -> bool {
    match &expression.kind {
        ExprKind::Await { .. } => true,
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => values.iter().any(expression_contains_await),
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::MakeView { value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        }
        | ExprKind::WaitFd {
            descriptor: value, ..
        } => expression_contains_await(value),
        ExprKind::Binary(_, left, right) => {
            expression_contains_await(left) || expression_contains_await(right)
        }
        ExprKind::Block(block) => block_contains_await(block),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expression_contains_await(condition)
                || block_contains_await(then_branch)
                || block_contains_await(else_branch)
        }
        ExprKind::Match { scrutinee, arms } => {
            expression_contains_await(scrutinee)
                || arms
                    .iter()
                    .any(|arm| expression_contains_await(&arm.value))
        }
        ExprKind::Record { fields, .. } => fields.iter().any(expression_contains_await),
        ExprKind::Variant { payload, .. } => payload.iter().any(expression_contains_await),
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| {
            matches!(argument, CallArgument::Value(value) if expression_contains_await(value))
        }),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

const MAX_STACK_RECORD_FIELDS: usize = 16;
const MAX_STACK_RECORD_NODES_PER_FUNCTION: usize = 64;
const INT_LIST_FIELD_DATA: u32 = 0;
const INT_LIST_FIELD_LENGTH: u32 = 1;
const INT_LIST_FIELD_CAPACITY: u32 = 2;

// Safety depends on the current runtime boundary: synchronous generated code has no GC
// safepoint, and checked MIR gives InOut/view carriers call-scoped lifetimes. Copies that can
// outlive this frame go through `loom.runtime.clone` and receive managed nodes. If allocation
// becomes a safepoint, GC becomes concurrent, or FFI can retain a source address, this fast path
// must gain stack-root metadata or be disabled.
fn is_stack_record_initializer(expression: &Expr, expected: TypeId) -> bool {
    match &expression.kind {
        ExprKind::Record {
            ty,
            construction: ConstructionMode::Plain | ConstructionMode::Proven,
            ..
        } => *ty == expected,
        ExprKind::Block(block) => block
            .tail
            .as_deref()
            .is_some_and(|tail| is_stack_record_initializer(tail, expected)),
        _ => false,
    }
}

fn stack_record_candidates(program: &Program, function: &Function) -> BTreeMap<LocalId, usize> {
    if function.is_async {
        return BTreeMap::new();
    }
    let eligible = function
        .locals
        .iter()
        .filter_map(|local| {
            let Type::Nominal(id, arguments) = &local.ty else {
                return None;
            };
            if !arguments.is_empty() {
                return None;
            }
            let definition = program.type_def(*id)?;
            if definition.type_parameters != 0 {
                return None;
            }
            let TypeDefKind::Record { fields, invariant } = &definition.kind else {
                return None;
            };
            if invariant.is_some()
                || fields.len() > MAX_STACK_RECORD_FIELDS
                || !fields.iter().all(|field| {
                    matches!(field.ty, Type::Unit | Type::Bool | Type::Int | Type::Float)
                })
            {
                return None;
            }
            Some((local.id, (*id, fields.len())))
        })
        .collect::<BTreeMap<_, _>>();
    if eligible.is_empty() {
        return BTreeMap::new();
    }
    let mut initialized = BTreeMap::<LocalId, usize>::new();
    let mut forbidden = BTreeSet::new();
    scan_stack_record_block(&function.body, &eligible, &mut initialized, &mut forbidden);
    let mut total_nodes = 0_usize;
    eligible
        .into_iter()
        .filter_map(|(local, (_, fields))| {
            if initialized.get(&local) != Some(&1) || forbidden.contains(&local) {
                return None;
            }
            let next_total = total_nodes.checked_add(fields)?;
            if next_total > MAX_STACK_RECORD_NODES_PER_FUNCTION {
                return None;
            }
            total_nodes = next_total;
            Some((local, fields))
        })
        .collect()
}

fn scan_stack_record_block(
    block: &Block,
    eligible: &BTreeMap<LocalId, (TypeId, usize)>,
    initialized: &mut BTreeMap<LocalId, usize>,
    forbidden: &mut BTreeSet<LocalId>,
) {
    for statement in &block.statements {
        match &statement.kind {
            StatementKind::Let { local, value } => {
                if let Some((expected, _)) = eligible.get(local) {
                    if is_stack_record_initializer(value, *expected) {
                        *initialized.entry(*local).or_default() += 1;
                    } else {
                        forbidden.insert(*local);
                    }
                }
                scan_stack_record_expr(value, eligible, initialized, forbidden);
            }
            StatementKind::LetTuple { locals, value } => {
                forbidden.extend(
                    locals
                        .iter()
                        .copied()
                        .filter(|local| eligible.contains_key(local)),
                );
                scan_stack_record_expr(value, eligible, initialized, forbidden);
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                scan_stack_record_expr(start, eligible, initialized, forbidden);
                scan_stack_record_expr(end, eligible, initialized, forbidden);
                scan_stack_record_block(body, eligible, initialized, forbidden);
            }
            StatementKind::Assign { place, value } => {
                if place.projection.is_empty() && eligible.contains_key(&place.local) {
                    forbidden.insert(place.local);
                }
                scan_stack_record_expr(value, eligible, initialized, forbidden);
            }
            StatementKind::Assert { condition } | StatementKind::Evaluate(condition) => {
                scan_stack_record_expr(condition, eligible, initialized, forbidden);
            }
            StatementKind::Defer(cleanup) => {
                scan_stack_record_block(cleanup, eligible, initialized, forbidden);
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    scan_stack_record_expr(value, eligible, initialized, forbidden);
                }
            }
        }
    }
    if let Some(tail) = &block.tail {
        scan_stack_record_expr(tail, eligible, initialized, forbidden);
    }
}

#[allow(clippy::too_many_lines)]
fn scan_stack_record_expr(
    expression: &Expr,
    eligible: &BTreeMap<LocalId, (TypeId, usize)>,
    initialized: &mut BTreeMap<LocalId, usize>,
    forbidden: &mut BTreeSet<LocalId>,
) {
    match &expression.kind {
        ExprKind::Constant(_) | ExprKind::Copy(_) => {}
        ExprKind::Move(place) => {
            if eligible.contains_key(&place.local) {
                forbidden.insert(place.local);
            }
        }
        ExprKind::Tuple(values)
        | ExprKind::List(values)
        | ExprKind::TaskJoin {
            arguments: values, ..
        } => {
            for value in values {
                scan_stack_record_expr(value, eligible, initialized, forbidden);
            }
        }
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        } => scan_stack_record_expr(value, eligible, initialized, forbidden),
        ExprKind::WaitFd { descriptor, .. } => {
            scan_stack_record_expr(descriptor, eligible, initialized, forbidden);
        }
        ExprKind::Binary(_, left, right) => {
            scan_stack_record_expr(left, eligible, initialized, forbidden);
            scan_stack_record_expr(right, eligible, initialized, forbidden);
        }
        ExprKind::Block(block) => {
            scan_stack_record_block(block, eligible, initialized, forbidden);
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_stack_record_expr(condition, eligible, initialized, forbidden);
            scan_stack_record_block(then_branch, eligible, initialized, forbidden);
            scan_stack_record_block(else_branch, eligible, initialized, forbidden);
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_stack_record_expr(scrutinee, eligible, initialized, forbidden);
            for arm in arms {
                scan_stack_record_expr(&arm.value, eligible, initialized, forbidden);
            }
        }
        ExprKind::Record { fields, .. } => {
            for field in fields {
                scan_stack_record_expr(field, eligible, initialized, forbidden);
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                scan_stack_record_expr(value, eligible, initialized, forbidden);
            }
        }
        ExprKind::Call { arguments, .. } => {
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => {
                        scan_stack_record_expr(value, eligible, initialized, forbidden);
                    }
                    CallArgument::InOut(_) => {
                        // Checked MIR makes an InOut loan call-scoped. Callees can mutate the
                        // value, but every observable escape is a deep value copy, so the
                        // backing nodes cannot outlive this synchronous frame.
                    }
                }
            }
        }
        ExprKind::MakeView {
            value, writeback, ..
        } => {
            scan_stack_record_expr(value, eligible, initialized, forbidden);
            if let Some(place) = writeback
                && eligible.contains_key(&place.local)
            {
                forbidden.insert(place.local);
            }
        }
        ExprKind::ReborrowView { owner, .. } => {
            if eligible.contains_key(&owner.local) {
                forbidden.insert(owner.local);
            }
        }
    }
}

impl Emitter {
    pub(crate) fn emit_object(
        program: &Program,
        reachable: &ReachableProgram,
        roots: &Roots,
        output: &Path,
        options: &EmitOptions,
    ) -> Result<NativeObjectArtifact, CodegenError> {
        let (triple, machine) =
            create_target_machine(options.target_triple.as_deref(), options.optimization)?;

        let context = Context::create();
        let requirements = RuntimeRequirementGraph::analyze(program, reachable)?;
        let mut backend = Backend::new(&context, program, reachable, roots, options, requirements);
        backend.module.set_triple(&triple);
        backend
            .module
            .set_data_layout(&machine.get_target_data().get_data_layout());
        backend.compile()?;
        backend.finalize_debug();
        backend
            .module
            .verify()
            .map_err(|message| CodegenError::new("LlvmVerificationFailed", message.to_string()))?;
        backend
            .module
            .run_passes(
                options.optimization.pipeline(),
                &machine,
                PassBuilderOptions::create(),
            )
            .map_err(|message| CodegenError::new("LlvmOptimizationFailed", message.to_string()))?;
        backend
            .module
            .verify()
            .map_err(|message| CodegenError::new("LlvmVerificationFailed", message.to_string()))?;

        if let Some(ir_path) = &options.emit_ir {
            backend.module.print_to_file(ir_path).map_err(|message| {
                CodegenError::new(
                    "LlvmIrWriteFailed",
                    format!("{}: {message}", ir_path.display()),
                )
            })?;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CodegenError::new(
                    "ArtifactWriteFailed",
                    format!("{}: {error}", parent.display()),
                )
            })?;
        }
        machine
            .write_to_file(&backend.module, FileType::Object, output)
            .map_err(|message| CodegenError::new("LlvmObjectWriteFailed", message.to_string()))?;

        Ok(NativeObjectArtifact {
            object: output.to_path_buf(),
            functions: reachable.functions.len(),
            witnesses: reachable.witnesses.len(),
        })
    }

    pub(crate) fn link_object(object: &Path, output: &Path) -> Result<(), CodegenError> {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                CodegenError::new(
                    "ArtifactWriteFailed",
                    format!("{}: {error}", parent.display()),
                )
            })?;
        }
        let runtime_objects = [materialize_rust_runtime()?];
        let runtime_paths = runtime_objects
            .iter()
            .map(tempfile::NamedTempFile::path)
            .collect::<Vec<_>>();
        link_objects(object, &runtime_paths, output)
    }
}

fn materialize_rust_runtime() -> Result<tempfile::NamedTempFile, CodegenError> {
    let archive = tempfile::Builder::new()
        .prefix("loom-runtime-")
        .suffix(".a")
        .tempfile()
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.to_string()))?;
    std::fs::write(archive.path(), native_runtime_bytes())
        .map_err(|error| CodegenError::new("ArtifactWriteFailed", error.to_string()))?;
    Ok(archive)
}

pub(crate) fn native_runtime_bytes() -> &'static [u8] {
    include_bytes!(concat!(env!("OUT_DIR"), "/libloom_runtime.a"))
}

fn link_objects(object: &Path, runtimes: &[&Path], output: &Path) -> Result<(), CodegenError> {
    let linker = native_linker_program();
    let mut command = Command::new(&linker);
    command.arg(object);
    for runtime in runtimes {
        command.arg(runtime);
    }
    #[cfg(target_os = "linux")]
    command.args(["-ldl", "-lpthread", "-lm", "-lrt", "-lutil"]);
    let result = command.arg("-o").arg(output).output().map_err(|error| {
        CodegenError::new(
            "NativeLinkerUnavailable",
            format!("{}: {error}", Path::new(&linker).display()),
        )
    })?;
    if result.status.success() {
        Ok(())
    } else {
        Err(CodegenError::new(
            "NativeLinkFailed",
            String::from_utf8_lossy(&result.stderr).trim().to_owned(),
        ))
    }
}

pub(crate) fn native_linker_program() -> std::ffi::OsString {
    std::env::var_os("LOOM_CC").unwrap_or_else(|| "clang".into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RootContextPlan {
    None,
    Runtime,
    Executor,
}

#[derive(Clone, Copy)]
struct RootContext<'ctx> {
    plan: RootContextPlan,
    runtime: PointerValue<'ctx>,
    executor: PointerValue<'ctx>,
}

impl<'ctx> RootContext<'ctx> {
    fn hidden(self) -> PointerValue<'ctx> {
        match self.plan {
            RootContextPlan::None | RootContextPlan::Runtime => self.runtime,
            RootContextPlan::Executor => self.executor,
        }
    }
}

struct NativeFunctionDecl<'ctx> {
    function: FunctionValue<'ctx>,
    signature: NativeSignature,
}

struct Backend<'ctx, 'program> {
    context: &'ctx Context,
    program: &'program Program,
    reachable: &'program ReachableProgram,
    roots: &'program Roots,
    options: &'program EmitOptions,
    requirements: RuntimeRequirementGraph,
    debug: DebugState<'ctx>,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    i64_type: IntType<'ctx>,
    ptr_type: PointerType<'ctx>,
    text_object_type: StructType<'ctx>,
    text_layout: GlobalValue<'ctx>,
    value_type: StructType<'ctx>,
    value_node_type: StructType<'ctx>,
    arg_node_type: StructType<'ctx>,
    int_list_type: StructType<'ctx>,
    witness_node_type: StructType<'ctx>,
    witness_type: StructType<'ctx>,
    wait_source_type: StructType<'ctx>,
    registration_type: StructType<'ctx>,
    ready_notification_type: StructType<'ctx>,
    coroutine_frame_type: StructType<'ctx>,
    coroutine_descriptor_type: StructType<'ctx>,
    loom_function_type: FunctionType<'ctx>,
    task_resume_type: FunctionType<'ctx>,
    functions: BTreeMap<FunctionId, FunctionValue<'ctx>>,
    native_functions: BTreeMap<FunctionId, NativeFunctionDecl<'ctx>>,
    task_resumes: BTreeMap<FunctionId, FunctionValue<'ctx>>,
    coroutine_descriptors: BTreeMap<FunctionId, GlobalValue<'ctx>>,
    witnesses: BTreeMap<WitnessId, GlobalValue<'ctx>>,
    names: Cell<u64>,
}

struct DebugState<'ctx> {
    builder: DebugInfoBuilder<'ctx>,
    unit: DICompileUnit<'ctx>,
    files: BTreeMap<u32, DIFile<'ctx>>,
    sources: BTreeMap<u32, DebugSource>,
}

impl<'ctx> DebugState<'ctx> {
    fn new(context: &'ctx Context, module: &Module<'ctx>, sources: &[DebugSource]) -> Self {
        let primary = sources
            .first()
            .map_or("<loom-generated>.loom", |source| source.path.as_str());
        module.set_source_file_name(primary);
        module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(3, false),
        );
        module.add_basic_value_flag(
            "Dwarf Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(4, false),
        );
        let (builder, unit) = module.create_debug_info_builder(
            true,
            DWARFSourceLanguage::C,
            primary,
            ".",
            concat!("loomc ", env!("CARGO_PKG_VERSION")),
            true,
            "",
            0,
            "",
            DWARFEmissionKind::Full,
            0,
            false,
            false,
            "",
            "",
        );
        let mut files = BTreeMap::new();
        let mut source_map = BTreeMap::new();
        for (index, source) in sources.iter().enumerate() {
            let file = if index == 0 {
                unit.get_file()
            } else {
                builder.create_file(&source.path, ".")
            };
            files.insert(source.file, file);
            source_map.insert(source.file, source.clone());
        }
        Self {
            builder,
            unit,
            files,
            sources: source_map,
        }
    }

    fn file(&self, id: u32) -> DIFile<'ctx> {
        self.files
            .get(&id)
            .copied()
            .unwrap_or_else(|| self.unit.get_file())
    }

    fn line_column(&self, file: u32, offset: u32) -> (u32, u32) {
        let Some(source) = self.sources.get(&file) else {
            return (1, 1);
        };
        let line = source
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = source.line_starts.get(line).copied().unwrap_or(0);
        (
            u32::try_from(line).unwrap_or(u32::MAX).saturating_add(1),
            offset.saturating_sub(start).saturating_add(1),
        )
    }

    fn attach_function(
        &self,
        function: FunctionValue<'ctx>,
        source_name: &str,
        file_id: u32,
        offset: u32,
    ) -> Result<(), CodegenError> {
        let file = self.file(file_id);
        let (line, _) = self.line_column(file_id, offset);
        let status = self
            .builder
            .create_basic_type("LoomStatus", 32, 0x05, DIFlags::PUBLIC)
            .map_err(|error| CodegenError::new("LlvmDebugInfoFailed", error.to_string()))?;
        let signature =
            self.builder
                .create_subroutine_type(file, Some(status.as_type()), &[], DIFlags::PUBLIC);
        let linkage = function.get_name().to_string_lossy();
        let scope = self.builder.create_function(
            file.as_debug_info_scope(),
            source_name,
            Some(linkage.as_ref()),
            file,
            line,
            signature,
            true,
            true,
            line,
            DIFlags::PUBLIC,
            true,
        );
        function.set_subprogram(scope);
        Ok(())
    }

    fn set_location(
        &self,
        context: &'ctx Context,
        ir_builder: &Builder<'ctx>,
        function: FunctionValue<'ctx>,
        file: u32,
        offset: u32,
    ) {
        if let Some(scope) = function.get_subprogram() {
            let (line, column) = self.line_column(file, offset);
            let location = self.builder.create_debug_location(
                context,
                line,
                column,
                scope.as_debug_info_scope(),
                None,
            );
            ir_builder.set_current_debug_location(location);
        }
    }
}

impl<'ctx, 'program> Backend<'ctx, 'program> {
    #[allow(clippy::too_many_lines)]
    fn new(
        context: &'ctx Context,
        program: &'program Program,
        reachable: &'program ReachableProgram,
        roots: &'program Roots,
        options: &'program EmitOptions,
        requirements: RuntimeRequirementGraph,
    ) -> Self {
        let module = context.create_module("loom.program");
        let builder = context.create_builder();
        let i64_type = context.i64_type();
        let i32_type = context.i32_type();
        let ptr_type = context.ptr_type(AddressSpace::default());
        let layout_descriptor_type = context.opaque_struct_type("loom.LayoutDescriptor");
        layout_descriptor_type.set_body(
            &[
                i32_type.into(),
                i32_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                i32_type.into(),
                i32_type.into(),
            ],
            false,
        );
        let text_object_type = context.opaque_struct_type("loom.TextObject");
        text_object_type.set_body(
            &[
                ptr_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                context.i8_type().array_type(0).into(),
            ],
            false,
        );
        let text_layout = module.add_global(layout_descriptor_type, None, "loom_layout_text_v1");
        text_layout.set_linkage(Linkage::External);
        let value_type = context.opaque_struct_type("loom.Value");
        value_type.set_body(
            &[
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                ptr_type.into(),
                ptr_type.into(),
            ],
            false,
        );
        let value_node_type = context.opaque_struct_type("loom.ValueNode");
        value_node_type.set_body(&[value_type.into(), ptr_type.into()], false);
        let arg_node_type = context.opaque_struct_type("loom.ArgNode");
        arg_node_type.set_body(&[ptr_type.into(), ptr_type.into()], false);
        let int_list_type = context.opaque_struct_type("loom.IntListStorage");
        int_list_type.set_body(&[ptr_type.into(), i64_type.into(), i64_type.into()], false);
        let witness_node_type = context.opaque_struct_type("loom.WitnessNode");
        witness_node_type.set_body(&[ptr_type.into(), ptr_type.into()], false);
        let witness_type = context.opaque_struct_type("loom.Witness");
        witness_type.set_body(
            &std::iter::repeat_n(ptr_type.into(), program.requirements.len() + 1)
                .collect::<Vec<_>>(),
            false,
        );
        let wait_source_type = context.opaque_struct_type("loom.WaitSource");
        wait_source_type.set_body(
            &[
                i32_type.into(),
                i32_type.into(),
                i64_type.into(),
                i32_type.into(),
                i32_type.into(),
                i64_type.into(),
            ],
            false,
        );
        let registration_type = context.opaque_struct_type("loom.Registration");
        registration_type.set_body(&[i64_type.into(), i64_type.into()], false);
        let ready_notification_type = context.opaque_struct_type("loom.ReadyNotification");
        ready_notification_type.set_body(
            &[
                registration_type.into(),
                ptr_type.into(),
                i32_type.into(),
                i32_type.into(),
            ],
            false,
        );
        let coroutine_frame_type = context.opaque_struct_type("loom.CoroutineFrame");
        coroutine_frame_type.set_body(&[i64_type.into(), ptr_type.into()], false);
        let coroutine_descriptor_type = context.opaque_struct_type("loom.CoroutineDescriptor");
        coroutine_descriptor_type.set_body(
            &[
                context.i32_type().into(),
                context.i32_type().into(),
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                i64_type.into(),
                ptr_type.into(),
            ],
            false,
        );
        let loom_function_type = context.i32_type().fn_type(
            &[
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
                ptr_type.into(),
            ],
            false,
        );
        let task_resume_type = context
            .i32_type()
            .fn_type(&[ptr_type.into(), ptr_type.into()], false);
        let debug = DebugState::new(context, &module, &options.debug_sources);
        Self {
            context,
            program,
            reachable,
            roots,
            options,
            requirements,
            debug,
            module,
            builder,
            i64_type,
            ptr_type,
            text_object_type,
            text_layout,
            value_type,
            value_node_type,
            arg_node_type,
            int_list_type,
            witness_node_type,
            witness_type,
            wait_source_type,
            registration_type,
            ready_notification_type,
            coroutine_frame_type,
            coroutine_descriptor_type,
            loom_function_type,
            task_resume_type,
            functions: BTreeMap::new(),
            native_functions: BTreeMap::new(),
            task_resumes: BTreeMap::new(),
            coroutine_descriptors: BTreeMap::new(),
            witnesses: BTreeMap::new(),
            names: Cell::new(0),
        }
    }

    fn compile(&mut self) -> Result<(), CodegenError> {
        self.declare_functions()?;
        self.declare_witnesses()?;
        self.emit_runtime_helpers()?;
        for function in &self.reachable.functions {
            let source = self.program.function(*function).ok_or_else(|| {
                CodegenError::new("InvalidFunctionReference", "reachable function is missing")
            })?;
            if source.is_async {
                self.emit_async_constructor(*function)?;
                self.emit_async_resume(*function)?;
            } else if self.native_functions.contains_key(function) {
                self.emit_native_function(*function)?;
                self.emit_native_wrapper(*function)?;
            } else {
                self.emit_function(*function)?;
            }
        }
        self.emit_main()
    }

    fn finalize_debug(&self) {
        self.builder.unset_current_debug_location();
        self.debug.builder.finalize();
    }

    fn set_debug_location(&self, function: FunctionValue<'ctx>, file: u32, offset: u32) {
        self.debug
            .set_location(self.context, &self.builder, function, file, offset);
    }

    fn native_value_type(&self, layout: NativeLayout) -> BasicTypeEnum<'ctx> {
        match layout {
            NativeLayout::Scalar(NativeScalar::Int) => self.i64_type.into(),
        }
    }

    fn native_status_result_type(
        &self,
        signature: &NativeSignature,
    ) -> Result<StructType<'ctx>, CodegenError> {
        if signature.effect() != NativeEffectAbi::RuntimeStatus {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "a native status result requires the runtime-status effect ABI",
            ));
        }
        Ok(self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.native_value_type(signature.shape().result()),
            ],
            false,
        ))
    }

    fn native_function_type(
        &self,
        signature: &NativeSignature,
    ) -> Result<FunctionType<'ctx>, CodegenError> {
        let mut parameters = signature
            .shape()
            .parameters()
            .iter()
            .copied()
            .map(|layout| BasicMetadataTypeEnum::from(self.native_value_type(layout)))
            .collect::<Vec<_>>();
        match signature.effect() {
            NativeEffectAbi::PureNoFault => Ok(self
                .native_value_type(signature.shape().result())
                .fn_type(&parameters, false)),
            NativeEffectAbi::RuntimeStatus => {
                parameters.push(self.ptr_type.into());
                Ok(self
                    .native_status_result_type(signature)?
                    .fn_type(&parameters, false))
            }
        }
    }

    fn declare_functions(&mut self) -> Result<(), CodegenError> {
        for id in &self.reachable.functions {
            let source = self.program.function(*id).ok_or_else(|| {
                CodegenError::new(
                    "InvalidFunctionReference",
                    format!("function #{} does not exist", id.0),
                )
            })?;
            let name = format!("loom.fn.{}.{}", id.0, mangle(&source.name));
            let function =
                self.module
                    .add_function(&name, self.loom_function_type, Some(Linkage::Internal));
            self.debug.attach_function(
                function,
                &source.name,
                source.span.file.0,
                source.span.range.start,
            )?;
            self.functions.insert(*id, function);
            if let Some(shape) = NativeSignatureShape::for_supported_function(source) {
                let requirements = self.requirements.function(*id)?.body;
                let effect = if requirements.is_pure_no_fault() {
                    NativeEffectAbi::PureNoFault
                } else {
                    NativeEffectAbi::RuntimeStatus
                };
                let signature = shape.with_effect(effect);
                let native = self.module.add_function(
                    &format!("loom.native.fn.{}.{}", id.0, mangle(&source.name)),
                    self.native_function_type(&signature)?,
                    Some(Linkage::Internal),
                );
                self.debug.attach_function(
                    native,
                    &format!("{}$native", source.name),
                    source.span.file.0,
                    source.span.range.start,
                )?;
                self.native_functions.insert(
                    *id,
                    NativeFunctionDecl {
                        function: native,
                        signature,
                    },
                );
            }
            if source.is_async {
                let resume = self.module.add_function(
                    &format!("loom.resume.{}.{}", id.0, mangle(&source.name)),
                    self.task_resume_type,
                    Some(Linkage::Internal),
                );
                self.debug.attach_function(
                    resume,
                    &format!("{}$resume", source.name),
                    source.span.file.0,
                    source.span.range.start,
                )?;
                self.task_resumes.insert(*id, resume);
                let descriptor = self.declare_coroutine_descriptor(*id, source, resume)?;
                self.coroutine_descriptors.insert(*id, descriptor);
            }
        }
        Ok(())
    }

    fn declare_coroutine_descriptor(
        &self,
        id: FunctionId,
        source: &Function,
        resume: FunctionValue<'ctx>,
    ) -> Result<GlobalValue<'ctx>, CodegenError> {
        let layout = AsyncLayout::new(source)?;
        let state_count = source
            .suspension_points
            .iter()
            .map(|point| u64::from(point.state))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many coroutine states"))?;
        let words = layout.slot_count.div_ceil(64);
        let total_words = state_count
            .checked_mul(words)
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| CodegenError::new("ProgramTooLarge", "live bitmap is too large"))?;
        let mut bitmap = vec![0_u64; total_words];
        let mut mark = |state: u64, slot: u64| -> Result<(), CodegenError> {
            let word = state
                .checked_mul(words)
                .and_then(|row| row.checked_add(slot / 64))
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "live slot is too large"))?;
            bitmap[word] |= 1_u64 << (slot % 64);
            Ok(())
        };
        for slot in layout.old_parameter_slots.values().copied() {
            for state in 0..state_count {
                mark(state, slot)?;
            }
        }
        for state in 0..state_count {
            mark(state, layout.result_slot)?;
            let suspension = u32::try_from(state).ok().and_then(|state| {
                source
                    .suspension_points
                    .iter()
                    .find(|point| point.state == state)
            });
            if state == 0 || suspension.is_none() {
                for slot in layout.local_slots.values().copied() {
                    mark(state, slot)?;
                }
            } else if let Some(suspension) = suspension {
                for local in &suspension.live_locals {
                    if let Some(slot) = layout.local_slots.get(local).copied() {
                        mark(state, slot)?;
                    }
                }
            }
        }
        let bitmap_values = bitmap
            .into_iter()
            .map(|word| self.i64_type.const_int(word, false))
            .collect::<Vec<_>>();
        let bitmap_type = self.i64_type.array_type(
            u32::try_from(bitmap_values.len())
                .map_err(|_| CodegenError::new("ProgramTooLarge", "live bitmap is too large"))?,
        );
        let bitmap_global =
            self.module
                .add_global(bitmap_type, None, &format!("loom.coroutine.live.{}", id.0));
        bitmap_global.set_initializer(&self.i64_type.const_array(&bitmap_values));
        bitmap_global.set_constant(true);
        bitmap_global.set_linkage(Linkage::Internal);

        let resume = resume.as_global_value().as_pointer_value();
        let trace = self
            .native_task_trace_live_slots()
            .as_global_value()
            .as_pointer_value();
        let fields = [
            self.context
                .i32_type()
                .const_int(COROUTINE_ABI_VERSION, false)
                .into(),
            self.context.i32_type().const_zero().into(),
            resume.into(),
            resume.into(),
            trace.into(),
            self.i64_type.const_int(layout.slot_count, false).into(),
            self.i64_type.const_int(layout.result_slot, false).into(),
            self.i64_type.const_int(state_count, false).into(),
            self.i64_type.const_int(words, false).into(),
            bitmap_global.as_pointer_value().into(),
        ];
        let descriptor = self.module.add_global(
            self.coroutine_descriptor_type,
            None,
            &format!("loom.coroutine.descriptor.{}", id.0),
        );
        descriptor.set_initializer(&self.coroutine_descriptor_type.const_named_struct(&fields));
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Internal);
        Ok(descriptor)
    }

    fn declare_witnesses(&mut self) -> Result<(), CodegenError> {
        for id in &self.reachable.witnesses {
            let global =
                self.module
                    .add_global(self.witness_type, None, &format!("loom.witness.{}", id.0));
            let mut fields = vec![self.ptr_type.const_null().into()];
            for requirement_index in 0..self.program.requirements.len() {
                let requirement =
                    loom_mir::RequirementId(u32::try_from(requirement_index).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "too many requirements")
                    })?);
                let pointer = self
                    .reachable
                    .witness_methods
                    .get(id)
                    .filter(|methods| methods.contains(&requirement))
                    .and_then(|_| self.program.witness(*id))
                    .and_then(|witness| witness.methods.get(&requirement))
                    .and_then(|function| self.functions.get(function))
                    .map_or_else(
                        || self.ptr_type.const_null(),
                        |function| function.as_global_value().as_pointer_value(),
                    );
                fields.push(pointer.into());
            }
            global.set_initializer(&self.witness_type.const_named_struct(&fields));
            global.set_constant(true);
            global.set_linkage(Linkage::Internal);
            self.witnesses.insert(*id, global);
        }
        Ok(())
    }

    fn emit_runtime_helpers(&self) -> Result<(), CodegenError> {
        self.emit_witness_helpers()?;
        self.emit_clone_helpers()?;
        self.emit_unwrap_helper()?;
        self.emit_equal_helpers()?;
        self.emit_print_helper()
    }

    #[allow(clippy::too_many_lines)]
    fn emit_witness_helpers(&self) -> Result<(), CodegenError> {
        let function_type = self
            .ptr_type
            .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
        let concatenate = self.module.add_function(
            "loom.runtime.concat_witnesses",
            function_type,
            Some(Linkage::Internal),
        );
        let entry = self.context.append_basic_block(concatenate, "entry");
        let empty = self.context.append_basic_block(concatenate, "empty");
        let loop_header = self.context.append_basic_block(concatenate, "loop");
        let copy = self.context.append_basic_block(concatenate, "copy");
        let first = self.context.append_basic_block(concatenate, "first");
        let append = self.context.append_basic_block(concatenate, "append");
        let advance = self.context.append_basic_block(concatenate, "advance");
        let finish = self.context.append_basic_block(concatenate, "finish");
        self.builder.position_at_end(entry);
        let prefix = parameter_pointer(concatenate, 0)?;
        let suffix = parameter_pointer(concatenate, 1)?;
        let current_slot = self
            .builder
            .build_alloca(self.ptr_type, "witness.current.slot")
            .map_err(builder_error)?;
        let head_slot = self
            .builder
            .build_alloca(self.ptr_type, "witness.head.slot")
            .map_err(builder_error)?;
        let tail_slot = self
            .builder
            .build_alloca(self.ptr_type, "witness.tail.slot")
            .map_err(builder_error)?;
        self.builder
            .build_store(current_slot, prefix)
            .map_err(builder_error)?;
        self.builder
            .build_store(head_slot, self.ptr_type.const_null())
            .map_err(builder_error)?;
        self.builder
            .build_store(tail_slot, self.ptr_type.const_null())
            .map_err(builder_error)?;
        let prefix_empty = self
            .builder
            .build_is_null(prefix, "prefix.empty")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(prefix_empty, empty, loop_header)
            .map_err(builder_error)?;

        self.builder.position_at_end(empty);
        self.builder
            .build_return(Some(&suffix))
            .map_err(builder_error)?;

        self.builder.position_at_end(loop_header);
        let current = self
            .builder
            .build_load(self.ptr_type, current_slot, "witness.current")
            .map_err(builder_error)?
            .into_pointer_value();
        let exhausted = self
            .builder
            .build_is_null(current, "witness.exhausted")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exhausted, finish, copy)
            .map_err(builder_error)?;

        self.builder.position_at_end(copy);
        let node = call_pointer(
            &self.builder,
            self.native_gc_alloc_witness_node(),
            &[],
            "witness.concat",
        )?;
        let value = self.load_pointer_field(
            self.witness_node_type,
            current,
            WITNESS_NODE_FIELD_VALUE,
            "witness.concat.value",
        )?;
        self.store_pointer_field(
            self.witness_node_type,
            node,
            WITNESS_NODE_FIELD_VALUE,
            value,
        )?;
        self.store_pointer_field(
            self.witness_node_type,
            node,
            WITNESS_NODE_FIELD_NEXT,
            self.ptr_type.const_null(),
        )?;
        let head = self
            .builder
            .build_load(self.ptr_type, head_slot, "witness.head")
            .map_err(builder_error)?
            .into_pointer_value();
        let head_empty = self
            .builder
            .build_is_null(head, "witness.head.empty")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(head_empty, first, append)
            .map_err(builder_error)?;

        self.builder.position_at_end(first);
        self.builder
            .build_store(head_slot, node)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(advance)
            .map_err(builder_error)?;

        self.builder.position_at_end(append);
        let tail = self
            .builder
            .build_load(self.ptr_type, tail_slot, "witness.tail")
            .map_err(builder_error)?
            .into_pointer_value();
        self.store_pointer_field(self.witness_node_type, tail, WITNESS_NODE_FIELD_NEXT, node)?;
        self.builder
            .build_unconditional_branch(advance)
            .map_err(builder_error)?;

        self.builder.position_at_end(advance);
        self.builder
            .build_store(tail_slot, node)
            .map_err(builder_error)?;
        let next = self.load_pointer_field(
            self.witness_node_type,
            current,
            WITNESS_NODE_FIELD_NEXT,
            "witness.concat.next",
        )?;
        self.builder
            .build_store(current_slot, next)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(loop_header)
            .map_err(builder_error)?;

        self.builder.position_at_end(finish);
        let tail = self
            .builder
            .build_load(self.ptr_type, tail_slot, "witness.finished.tail")
            .map_err(builder_error)?
            .into_pointer_value();
        self.store_pointer_field(
            self.witness_node_type,
            tail,
            WITNESS_NODE_FIELD_NEXT,
            suffix,
        )?;
        let head = self
            .builder
            .build_load(self.ptr_type, head_slot, "witness.finished.head")
            .map_err(builder_error)?
            .into_pointer_value();
        self.builder
            .build_return(Some(&head))
            .map_err(builder_error)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn emit_clone_helpers(&self) -> Result<(), CodegenError> {
        let clone_type = self
            .context
            .void_type()
            .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
        let nodes_type = self
            .ptr_type
            .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
        let clone =
            self.module
                .add_function("loom.runtime.clone", clone_type, Some(Linkage::Internal));
        let clone_nodes = self.module.add_function(
            "loom.runtime.clone_nodes",
            nodes_type,
            Some(Linkage::Internal),
        );

        let entry = self.context.append_basic_block(clone_nodes, "entry");
        let loop_header = self.context.append_basic_block(clone_nodes, "loop");
        let copy = self.context.append_basic_block(clone_nodes, "copy");
        let first = self.context.append_basic_block(clone_nodes, "first");
        let append = self.context.append_basic_block(clone_nodes, "append");
        let advance = self.context.append_basic_block(clone_nodes, "advance");
        let finished = self.context.append_basic_block(clone_nodes, "done");
        self.builder.position_at_end(entry);
        let source = parameter_pointer(clone_nodes, 0)?;
        let count = parameter_int(clone_nodes, 1)?;
        let source_slot = self
            .builder
            .build_alloca(self.ptr_type, "nodes.source.slot")
            .map_err(builder_error)?;
        let remaining_slot = self
            .builder
            .build_alloca(self.i64_type, "nodes.remaining.slot")
            .map_err(builder_error)?;
        let head_slot = self
            .builder
            .build_alloca(self.ptr_type, "nodes.head.slot")
            .map_err(builder_error)?;
        let tail_slot = self
            .builder
            .build_alloca(self.ptr_type, "nodes.tail.slot")
            .map_err(builder_error)?;
        self.builder
            .build_store(source_slot, source)
            .map_err(builder_error)?;
        self.builder
            .build_store(remaining_slot, count)
            .map_err(builder_error)?;
        self.builder
            .build_store(head_slot, self.ptr_type.const_null())
            .map_err(builder_error)?;
        self.builder
            .build_store(tail_slot, self.ptr_type.const_null())
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(loop_header)
            .map_err(builder_error)?;

        self.builder.position_at_end(loop_header);
        let remaining = self
            .builder
            .build_load(self.i64_type, remaining_slot, "nodes.remaining")
            .map_err(builder_error)?
            .into_int_value();
        let exhausted = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                remaining,
                self.i64_type.const_zero(),
                "nodes.done",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exhausted, finished, copy)
            .map_err(builder_error)?;

        self.builder.position_at_end(copy);
        let source = self
            .builder
            .build_load(self.ptr_type, source_slot, "nodes.source")
            .map_err(builder_error)?
            .into_pointer_value();
        let target = call_pointer(
            &self.builder,
            self.native_gc_alloc_value_node(),
            &[],
            "node.clone",
        )?;
        let source_value = self.struct_pointer(
            self.value_node_type,
            source,
            VALUE_NODE_FIELD_VALUE,
            "source.value",
        )?;
        let target_value = self.struct_pointer(
            self.value_node_type,
            target,
            VALUE_NODE_FIELD_VALUE,
            "target.value",
        )?;
        self.builder
            .build_call(
                clone,
                &[target_value.into(), source_value.into()],
                "clone.value",
            )
            .map_err(builder_error)?;
        self.store_pointer_field(
            self.value_node_type,
            target,
            VALUE_NODE_FIELD_NEXT,
            self.ptr_type.const_null(),
        )?;
        let head = self
            .builder
            .build_load(self.ptr_type, head_slot, "nodes.head")
            .map_err(builder_error)?
            .into_pointer_value();
        let head_empty = self
            .builder
            .build_is_null(head, "nodes.head.empty")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(head_empty, first, append)
            .map_err(builder_error)?;

        self.builder.position_at_end(first);
        self.builder
            .build_store(head_slot, target)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(advance)
            .map_err(builder_error)?;

        self.builder.position_at_end(append);
        let tail = self
            .builder
            .build_load(self.ptr_type, tail_slot, "nodes.tail")
            .map_err(builder_error)?
            .into_pointer_value();
        self.store_pointer_field(self.value_node_type, tail, VALUE_NODE_FIELD_NEXT, target)?;
        self.builder
            .build_unconditional_branch(advance)
            .map_err(builder_error)?;

        self.builder.position_at_end(advance);
        self.builder
            .build_store(tail_slot, target)
            .map_err(builder_error)?;
        let source_next = self.load_pointer_field(
            self.value_node_type,
            source,
            VALUE_NODE_FIELD_NEXT,
            "source.next",
        )?;
        self.builder
            .build_store(source_slot, source_next)
            .map_err(builder_error)?;
        let remaining = self
            .builder
            .build_int_sub(
                remaining,
                self.i64_type.const_int(1, false),
                "nodes.next.remaining",
            )
            .map_err(builder_error)?;
        self.builder
            .build_store(remaining_slot, remaining)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(loop_header)
            .map_err(builder_error)?;

        self.builder.position_at_end(finished);
        let head = self
            .builder
            .build_load(self.ptr_type, head_slot, "nodes.finished.head")
            .map_err(builder_error)?
            .into_pointer_value();
        self.builder
            .build_return(Some(&head))
            .map_err(builder_error)?;

        let entry = self.context.append_basic_block(clone, "entry");
        let aggregate = self.context.append_basic_block(clone, "aggregate");
        let enumeration = self.context.append_basic_block(clone, "enum");
        let refined = self.context.append_basic_block(clone, "refined");
        let dynamic = self.context.append_basic_block(clone, "dynamic");
        let outcome = self.context.append_basic_block(clone, "task.outcome");
        let outcome_completed = self
            .context
            .append_basic_block(clone, "task.outcome.completed");
        let done = self.context.append_basic_block(clone, "done");
        self.builder.position_at_end(entry);
        let target = parameter_pointer(clone, 0)?;
        let source = parameter_pointer(clone, 1)?;
        let value = self
            .builder
            .build_load(self.value_type, source, "clone.source")
            .map_err(builder_error)?;
        self.builder
            .build_store(target, value)
            .map_err(builder_error)?;
        let tag = self.load_i64_field(self.value_type, source, VALUE_FIELD_TAG, "clone.tag")?;
        self.builder
            .build_switch(
                tag,
                done,
                &[
                    (self.tag(VALUE_TAG_RECORD), aggregate),
                    (self.tag(VALUE_TAG_CONSTRAINT_ERROR), aggregate),
                    (self.tag(VALUE_TAG_TUPLE), aggregate),
                    (self.tag(VALUE_TAG_LIST), aggregate),
                    (self.tag(VALUE_TAG_ENUM), enumeration),
                    (self.tag(VALUE_TAG_REFINED), refined),
                    (self.tag(VALUE_TAG_DYN), dynamic),
                    (self.tag(VALUE_TAG_TASK_OUTCOME), outcome),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(aggregate);
        let count = self.load_i64_field(self.value_type, source, VALUE_FIELD_AUX, "field.count")?;
        self.clone_data_chain(source, target, count, clone_nodes, done)?;

        self.builder.position_at_end(enumeration);
        let count =
            self.load_i64_field(self.value_type, source, VALUE_FIELD_SCALAR, "payload.count")?;
        self.clone_data_chain(source, target, count, clone_nodes, done)?;

        self.builder.position_at_end(refined);
        let source_inner =
            self.load_pointer_field(self.value_type, source, VALUE_FIELD_DATA, "refined.source")?;
        let target_inner = call_pointer(
            &self.builder,
            self.native_gc_alloc_value(),
            &[],
            "refined.clone",
        )?;
        self.builder
            .build_call(
                clone,
                &[target_inner.into(), source_inner.into()],
                "clone.inner",
            )
            .map_err(builder_error)?;
        self.store_pointer_field(self.value_type, target, VALUE_FIELD_DATA, target_inner)?;
        self.builder
            .build_unconditional_branch(done)
            .map_err(builder_error)?;

        self.builder.position_at_end(dynamic);
        let source_inner =
            self.load_pointer_field(self.value_type, source, VALUE_FIELD_DATA, "dyn.source")?;
        let target_inner = call_pointer(
            &self.builder,
            self.native_gc_alloc_value(),
            &[],
            "dyn.clone",
        )?;
        self.builder
            .build_call(
                clone,
                &[target_inner.into(), source_inner.into()],
                "clone.dyn",
            )
            .map_err(builder_error)?;
        self.store_pointer_field(self.value_type, target, VALUE_FIELD_DATA, target_inner)?;
        self.store_i64_field(
            self.value_type,
            target,
            VALUE_FIELD_SCALAR,
            self.i64_type.const_zero(),
        )?;
        let flags = self.load_i64_field(self.value_type, source, VALUE_FIELD_AUX, "dyn.flags")?;
        let mutable = self
            .builder
            .build_and(flags, self.tag(DYN_FLAG_MUTABLE), "dyn.mutable")
            .map_err(builder_error)?;
        self.store_i64_field(self.value_type, target, VALUE_FIELD_AUX, mutable)?;
        self.builder
            .build_unconditional_branch(done)
            .map_err(builder_error)?;

        self.builder.position_at_end(outcome);
        let step = self.load_i64_field(self.value_type, source, VALUE_FIELD_AUX, "outcome.step")?;
        let completed = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                step,
                self.i64_type.const_int(TASK_STEP_COMPLETED, false),
                "outcome.completed",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(completed, outcome_completed, done)
            .map_err(builder_error)?;

        self.builder.position_at_end(outcome_completed);
        let source_inner =
            self.load_pointer_field(self.value_type, source, VALUE_FIELD_DATA, "outcome.source")?;
        let target_inner = call_pointer(
            &self.builder,
            self.native_gc_alloc_value(),
            &[],
            "outcome.clone",
        )?;
        self.builder
            .build_call(
                clone,
                &[target_inner.into(), source_inner.into()],
                "clone.outcome",
            )
            .map_err(builder_error)?;
        self.store_pointer_field(self.value_type, target, VALUE_FIELD_DATA, target_inner)?;
        self.builder
            .build_unconditional_branch(done)
            .map_err(builder_error)?;

        self.builder.position_at_end(done);
        self.builder.build_return(None).map_err(builder_error)?;
        Ok(())
    }

    fn clone_data_chain(
        &self,
        source: PointerValue<'ctx>,
        target: PointerValue<'ctx>,
        count: IntValue<'ctx>,
        clone_nodes: FunctionValue<'ctx>,
        done: inkwell::basic_block::BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let source_data = self.load_pointer_field(
            self.value_type,
            source,
            VALUE_FIELD_DATA,
            "aggregate.source",
        )?;
        let data = call_pointer(
            &self.builder,
            clone_nodes,
            &[source_data.into(), count.into()],
            "aggregate.clone",
        )?;
        self.store_pointer_field(self.value_type, target, VALUE_FIELD_DATA, data)?;
        self.builder
            .build_unconditional_branch(done)
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_unwrap_helper(&self) -> Result<(), CodegenError> {
        let function_type = self.ptr_type.fn_type(&[self.ptr_type.into()], false);
        let function = self.module.add_function(
            "loom.runtime.unwrap",
            function_type,
            Some(Linkage::Internal),
        );
        let entry = self.context.append_basic_block(function, "entry");
        let nested = self.context.append_basic_block(function, "nested");
        let done = self.context.append_basic_block(function, "done");
        self.builder.position_at_end(entry);
        let value = parameter_pointer(function, 0)?;
        let tag = self.load_i64_field(self.value_type, value, VALUE_FIELD_TAG, "unwrap.tag")?;
        let is_refined = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                self.tag(VALUE_TAG_REFINED),
                "is.refined",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(is_refined, nested, done)
            .map_err(builder_error)?;
        self.builder.position_at_end(nested);
        let inner =
            self.load_pointer_field(self.value_type, value, VALUE_FIELD_DATA, "unwrap.inner")?;
        let unwrapped = call_pointer(&self.builder, function, &[inner.into()], "unwrap.recursive")?;
        self.builder
            .build_return(Some(&unwrapped))
            .map_err(builder_error)?;
        self.builder.position_at_end(done);
        self.builder
            .build_return(Some(&value))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_equal_helpers(&self) -> Result<(), CodegenError> {
        let equal_type = self
            .context
            .bool_type()
            .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
        let nodes_type = self.context.bool_type().fn_type(
            &[
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.i64_type.into(),
            ],
            false,
        );
        let equal =
            self.module
                .add_function("loom.runtime.equal", equal_type, Some(Linkage::Internal));
        let equal_nodes = self.module.add_function(
            "loom.runtime.equal_nodes",
            nodes_type,
            Some(Linkage::Internal),
        );
        self.emit_equal_nodes(equal, equal_nodes)?;
        self.emit_equal_value(equal, equal_nodes)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_equal_nodes(
        &self,
        equal: FunctionValue<'ctx>,
        equal_nodes: FunctionValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let entry = self.context.append_basic_block(equal_nodes, "entry");
        let loop_header = self.context.append_basic_block(equal_nodes, "loop");
        let yes = self.context.append_basic_block(equal_nodes, "yes");
        let compare = self.context.append_basic_block(equal_nodes, "compare");
        let advance = self.context.append_basic_block(equal_nodes, "advance");
        let no = self.context.append_basic_block(equal_nodes, "no");
        self.builder.position_at_end(entry);
        let left = parameter_pointer(equal_nodes, 0)?;
        let right = parameter_pointer(equal_nodes, 1)?;
        let count = parameter_int(equal_nodes, 2)?;
        let left_slot = self
            .builder
            .build_alloca(self.ptr_type, "equal.left.slot")
            .map_err(builder_error)?;
        let right_slot = self
            .builder
            .build_alloca(self.ptr_type, "equal.right.slot")
            .map_err(builder_error)?;
        let remaining_slot = self
            .builder
            .build_alloca(self.i64_type, "equal.remaining.slot")
            .map_err(builder_error)?;
        self.builder
            .build_store(left_slot, left)
            .map_err(builder_error)?;
        self.builder
            .build_store(right_slot, right)
            .map_err(builder_error)?;
        self.builder
            .build_store(remaining_slot, count)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(loop_header)
            .map_err(builder_error)?;

        self.builder.position_at_end(loop_header);
        let remaining = self
            .builder
            .build_load(self.i64_type, remaining_slot, "equal.remaining")
            .map_err(builder_error)?
            .into_int_value();
        let done = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                remaining,
                self.i64_type.const_zero(),
                "equal.done",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(done, yes, compare)
            .map_err(builder_error)?;
        self.builder.position_at_end(compare);
        let left = self
            .builder
            .build_load(self.ptr_type, left_slot, "equal.left")
            .map_err(builder_error)?
            .into_pointer_value();
        let right = self
            .builder
            .build_load(self.ptr_type, right_slot, "equal.right")
            .map_err(builder_error)?
            .into_pointer_value();
        let left_value = self.struct_pointer(
            self.value_node_type,
            left,
            VALUE_NODE_FIELD_VALUE,
            "left.value",
        )?;
        let right_value = self.struct_pointer(
            self.value_node_type,
            right,
            VALUE_NODE_FIELD_VALUE,
            "right.value",
        )?;
        let item_equal = call_int(
            &self.builder,
            equal,
            &[left_value.into(), right_value.into()],
            "item.equal",
        )?;
        self.builder
            .build_conditional_branch(item_equal, advance, no)
            .map_err(builder_error)?;

        self.builder.position_at_end(advance);
        let left_next = self.load_pointer_field(
            self.value_node_type,
            left,
            VALUE_NODE_FIELD_NEXT,
            "left.next",
        )?;
        let right_next = self.load_pointer_field(
            self.value_node_type,
            right,
            VALUE_NODE_FIELD_NEXT,
            "right.next",
        )?;
        let remaining = self
            .builder
            .build_int_sub(
                remaining,
                self.i64_type.const_int(1, false),
                "equal.next.remaining",
            )
            .map_err(builder_error)?;
        self.builder
            .build_store(left_slot, left_next)
            .map_err(builder_error)?;
        self.builder
            .build_store(right_slot, right_next)
            .map_err(builder_error)?;
        self.builder
            .build_store(remaining_slot, remaining)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(loop_header)
            .map_err(builder_error)?;
        self.builder.position_at_end(yes);
        self.builder
            .build_return(Some(&self.context.bool_type().const_int(1, false)))
            .map_err(builder_error)?;
        self.builder.position_at_end(no);
        self.builder
            .build_return(Some(&self.context.bool_type().const_zero()))
            .map_err(builder_error)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn emit_equal_value(
        &self,
        equal: FunctionValue<'ctx>,
        equal_nodes: FunctionValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let entry = self.context.append_basic_block(equal, "entry");
        let dispatch = self.context.append_basic_block(equal, "dispatch");
        let yes = self.context.append_basic_block(equal, "yes");
        let no = self.context.append_basic_block(equal, "no");
        let scalar = self.context.append_basic_block(equal, "scalar");
        let float = self.context.append_basic_block(equal, "float");
        let text = self.context.append_basic_block(equal, "text");
        let record = self.context.append_basic_block(equal, "record");
        let enumeration = self.context.append_basic_block(equal, "enum");
        let refined = self.context.append_basic_block(equal, "refined");
        let outcome = self.context.append_basic_block(equal, "task.outcome");
        self.builder.position_at_end(entry);
        let left = parameter_pointer(equal, 0)?;
        let right = parameter_pointer(equal, 1)?;
        let left_tag = self.load_i64_field(self.value_type, left, VALUE_FIELD_TAG, "left.tag")?;
        let right_tag =
            self.load_i64_field(self.value_type, right, VALUE_FIELD_TAG, "right.tag")?;
        let same_tag = self
            .builder
            .build_int_compare(IntPredicate::EQ, left_tag, right_tag, "same.tag")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(same_tag, dispatch, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(dispatch);
        self.builder
            .build_switch(
                left_tag,
                no,
                &[
                    (self.tag(VALUE_TAG_UNIT), yes),
                    (self.tag(VALUE_TAG_BOOL), scalar),
                    (self.tag(VALUE_TAG_INT), scalar),
                    (self.tag(VALUE_TAG_FLOAT), float),
                    (self.tag(VALUE_TAG_TEXT), text),
                    (self.tag(VALUE_TAG_RECORD), record),
                    (self.tag(VALUE_TAG_TUPLE), record),
                    (self.tag(VALUE_TAG_LIST), record),
                    (self.tag(VALUE_TAG_ENUM), enumeration),
                    (self.tag(VALUE_TAG_REFINED), refined),
                    (self.tag(VALUE_TAG_CONSTRAINT_ERROR), record),
                    (self.tag(VALUE_TAG_TASK_OUTCOME), outcome),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(scalar);
        let left_scalar =
            self.load_i64_field(self.value_type, left, VALUE_FIELD_SCALAR, "left.scalar")?;
        let right_scalar =
            self.load_i64_field(self.value_type, right, VALUE_FIELD_SCALAR, "right.scalar")?;
        let values_equal = self
            .builder
            .build_int_compare(IntPredicate::EQ, left_scalar, right_scalar, "scalar.equal")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(values_equal, yes, no)
            .map_err(builder_error)?;

        self.builder.position_at_end(float);
        let left_bits =
            self.load_i64_field(self.value_type, left, VALUE_FIELD_SCALAR, "left.bits")?;
        let right_bits =
            self.load_i64_field(self.value_type, right, VALUE_FIELD_SCALAR, "right.bits")?;
        let left_float = self
            .builder
            .build_bit_cast(left_bits, self.context.f64_type(), "left.float")
            .map_err(builder_error)?
            .into_float_value();
        let right_float = self
            .builder
            .build_bit_cast(right_bits, self.context.f64_type(), "right.float")
            .map_err(builder_error)?
            .into_float_value();
        let values_equal = self
            .builder
            .build_float_compare(FloatPredicate::OEQ, left_float, right_float, "float.equal")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(values_equal, yes, no)
            .map_err(builder_error)?;

        self.builder.position_at_end(text);
        let (_, left_data, left_length) = self.sequence_parts(left, "left.text")?;
        let (_, right_data, right_length) = self.sequence_parts(right, "right.text")?;
        let same_length = self
            .builder
            .build_int_compare(IntPredicate::EQ, left_length, right_length, "same.length")
            .map_err(builder_error)?;
        let text_bytes = self.context.append_basic_block(equal, "text.bytes");
        self.builder
            .build_conditional_branch(same_length, text_bytes, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(text_bytes);
        let memcmp = self.libc_memcmp();
        let comparison = call_int(
            &self.builder,
            memcmp,
            &[left_data.into(), right_data.into(), left_length.into()],
            "memcmp",
        )?;
        let text_equal = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                comparison,
                self.context.i32_type().const_zero(),
                "text.equal",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(text_equal, yes, no)
            .map_err(builder_error)?;

        self.builder.position_at_end(record);
        let same_nominal = self.compare_i64_field(left, right, VALUE_FIELD_NOMINAL, "nominal")?;
        let record_details = self.context.append_basic_block(equal, "record.details");
        self.builder
            .build_conditional_branch(same_nominal, record_details, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(record_details);
        let count = self.load_i64_field(self.value_type, left, VALUE_FIELD_AUX, "field.count")?;
        let same_count = self.compare_i64_field(left, right, VALUE_FIELD_AUX, "field.count")?;
        let record_items = self.context.append_basic_block(equal, "record.items");
        self.builder
            .build_conditional_branch(same_count, record_items, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(record_items);
        self.emit_equal_chains(left, right, count, equal_nodes, yes, no)?;

        self.builder.position_at_end(enumeration);
        let same_nominal = self.compare_i64_field(left, right, VALUE_FIELD_NOMINAL, "enum.type")?;
        let same_variant = self.compare_i64_field(left, right, VALUE_FIELD_AUX, "enum.variant")?;
        let header_equal = self
            .builder
            .build_and(same_nominal, same_variant, "enum.header")
            .map_err(builder_error)?;
        let enum_details = self.context.append_basic_block(equal, "enum.details");
        self.builder
            .build_conditional_branch(header_equal, enum_details, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(enum_details);
        let count =
            self.load_i64_field(self.value_type, left, VALUE_FIELD_SCALAR, "payload.count")?;
        let same_count =
            self.compare_i64_field(left, right, VALUE_FIELD_SCALAR, "payload.count")?;
        let enum_items = self.context.append_basic_block(equal, "enum.items");
        self.builder
            .build_conditional_branch(same_count, enum_items, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(enum_items);
        self.emit_equal_chains(left, right, count, equal_nodes, yes, no)?;

        self.builder.position_at_end(refined);
        let same_nominal =
            self.compare_i64_field(left, right, VALUE_FIELD_NOMINAL, "refined.type")?;
        let refined_inner = self.context.append_basic_block(equal, "refined.inner");
        self.builder
            .build_conditional_branch(same_nominal, refined_inner, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(refined_inner);
        let left_inner =
            self.load_pointer_field(self.value_type, left, VALUE_FIELD_DATA, "left.inner")?;
        let right_inner =
            self.load_pointer_field(self.value_type, right, VALUE_FIELD_DATA, "right.inner")?;
        let inner_equal = call_int(
            &self.builder,
            equal,
            &[left_inner.into(), right_inner.into()],
            "inner.equal",
        )?;
        self.builder
            .build_conditional_branch(inner_equal, yes, no)
            .map_err(builder_error)?;

        self.builder.position_at_end(outcome);
        let same_step = self.compare_i64_field(left, right, VALUE_FIELD_AUX, "outcome.step")?;
        let outcome_details = self
            .context
            .append_basic_block(equal, "task.outcome.details");
        self.builder
            .build_conditional_branch(same_step, outcome_details, no)
            .map_err(builder_error)?;
        self.builder.position_at_end(outcome_details);
        let step = self.load_i64_field(self.value_type, left, VALUE_FIELD_AUX, "outcome.step")?;
        let completed = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                step,
                self.i64_type.const_int(TASK_STEP_COMPLETED, false),
                "outcome.completed",
            )
            .map_err(builder_error)?;
        let outcome_inner = self.context.append_basic_block(equal, "task.outcome.inner");
        self.builder
            .build_conditional_branch(completed, outcome_inner, yes)
            .map_err(builder_error)?;
        self.builder.position_at_end(outcome_inner);
        let left_inner =
            self.load_pointer_field(self.value_type, left, VALUE_FIELD_DATA, "left.outcome")?;
        let right_inner =
            self.load_pointer_field(self.value_type, right, VALUE_FIELD_DATA, "right.outcome")?;
        let inner_equal = call_int(
            &self.builder,
            equal,
            &[left_inner.into(), right_inner.into()],
            "outcome.inner.equal",
        )?;
        self.builder
            .build_conditional_branch(inner_equal, yes, no)
            .map_err(builder_error)?;

        self.builder.position_at_end(yes);
        self.builder
            .build_return(Some(&self.context.bool_type().const_int(1, false)))
            .map_err(builder_error)?;
        self.builder.position_at_end(no);
        self.builder
            .build_return(Some(&self.context.bool_type().const_zero()))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_equal_chains(
        &self,
        left: PointerValue<'ctx>,
        right: PointerValue<'ctx>,
        count: IntValue<'ctx>,
        equal_nodes: FunctionValue<'ctx>,
        yes: inkwell::basic_block::BasicBlock<'ctx>,
        no: inkwell::basic_block::BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let left_data =
            self.load_pointer_field(self.value_type, left, VALUE_FIELD_DATA, "left.items")?;
        let right_data =
            self.load_pointer_field(self.value_type, right, VALUE_FIELD_DATA, "right.items")?;
        let equal = call_int(
            &self.builder,
            equal_nodes,
            &[left_data.into(), right_data.into(), count.into()],
            "items.equal",
        )?;
        self.builder
            .build_conditional_branch(equal, yes, no)
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_print_helper(&self) -> Result<(), CodegenError> {
        let function_type = self
            .context
            .void_type()
            .fn_type(&[self.ptr_type.into()], false);
        let function =
            self.module
                .add_function("loom.runtime.print", function_type, Some(Linkage::Internal));
        let entry = self.context.append_basic_block(function, "entry");
        let unit = self.context.append_basic_block(function, "unit");
        let boolean = self.context.append_basic_block(function, "bool");
        let integer = self.context.append_basic_block(function, "int");
        let float = self.context.append_basic_block(function, "float");
        let text = self.context.append_basic_block(function, "text");
        let nominal = self.context.append_basic_block(function, "nominal");
        let dynamic = self.context.append_basic_block(function, "dyn");
        let done = self.context.append_basic_block(function, "done");
        self.builder.position_at_end(entry);
        let value = parameter_pointer(function, 0)?;
        let tag = self.load_i64_field(self.value_type, value, VALUE_FIELD_TAG, "print.tag")?;
        self.builder
            .build_switch(
                tag,
                nominal,
                &[
                    (self.tag(VALUE_TAG_UNIT), unit),
                    (self.tag(VALUE_TAG_BOOL), boolean),
                    (self.tag(VALUE_TAG_INT), integer),
                    (self.tag(VALUE_TAG_FLOAT), float),
                    (self.tag(VALUE_TAG_TEXT), text),
                    (self.tag(VALUE_TAG_DYN), dynamic),
                ],
            )
            .map_err(builder_error)?;
        self.builder.position_at_end(unit);
        self.puts("Unit")?;
        self.branch(done)?;
        self.builder.position_at_end(boolean);
        let scalar =
            self.load_i64_field(self.value_type, value, VALUE_FIELD_SCALAR, "bool.value")?;
        let is_true = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                scalar,
                self.i64_type.const_zero(),
                "bool.true",
            )
            .map_err(builder_error)?;
        let print_true = self.context.append_basic_block(function, "print.true");
        let print_false = self.context.append_basic_block(function, "print.false");
        self.builder
            .build_conditional_branch(is_true, print_true, print_false)
            .map_err(builder_error)?;
        self.builder.position_at_end(print_true);
        self.puts("true")?;
        self.branch(done)?;
        self.builder.position_at_end(print_false);
        self.puts("false")?;
        self.branch(done)?;
        self.builder.position_at_end(integer);
        let scalar =
            self.load_i64_field(self.value_type, value, VALUE_FIELD_SCALAR, "int.value")?;
        self.printf("%lld\n", &[scalar.into()])?;
        self.branch(done)?;
        self.builder.position_at_end(float);
        let bits = self.load_i64_field(self.value_type, value, VALUE_FIELD_SCALAR, "float.bits")?;
        let number = self
            .builder
            .build_bit_cast(bits, self.context.f64_type(), "float.value")
            .map_err(builder_error)?;
        self.printf("%.17g\n", &[number.into()])?;
        self.branch(done)?;
        self.builder.position_at_end(text);
        let (_, _, length) = self.sequence_parts(value, "print.text")?;
        self.printf("Text(bytes=%lld)\n", &[length.into()])?;
        self.branch(done)?;
        self.builder.position_at_end(nominal);
        let id = self.load_i64_field(self.value_type, value, VALUE_FIELD_NOMINAL, "nominal.id")?;
        self.printf("type#%lld\n", &[id.into()])?;
        self.branch(done)?;
        self.builder.position_at_end(dynamic);
        self.puts("<dyn>")?;
        self.branch(done)?;
        self.builder.position_at_end(done);
        self.builder.build_return(None).map_err(builder_error)?;
        Ok(())
    }

    fn libc_memcmp(&self) -> FunctionValue<'ctx> {
        self.module.get_function("memcmp").unwrap_or_else(|| {
            let function_type = self.context.i32_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            );
            self.module.add_function("memcmp", function_type, None)
        })
    }

    fn libc_puts(&self) -> FunctionValue<'ctx> {
        self.module.get_function("puts").unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into()], false);
            self.module.add_function("puts", function_type, None)
        })
    }

    fn libc_printf(&self) -> FunctionValue<'ctx> {
        self.module.get_function("printf").unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into()], true);
            self.module.add_function("printf", function_type, None)
        })
    }

    fn native_parse_float(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_parse_float")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_parse_float", function_type, None)
            })
    }

    fn native_parse_int(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_parse_int")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_parse_int", function_type, None)
            })
    }

    fn native_format_float(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_format_float")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[self.context.f64_type().into(), self.ptr_type.into()],
                    false,
                );
                self.module
                    .add_function("loom_runtime_format_float", function_type, None)
            })
    }

    fn native_gc_alloc_value(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_gc_alloc_value")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[], false);
                self.module
                    .add_function("loom_gc_alloc_value", function_type, None)
            })
    }

    fn native_gc_alloc_value_node(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_gc_alloc_value_node")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[], false);
                self.module
                    .add_function("loom_gc_alloc_value_node", function_type, None)
            })
    }

    fn native_list_add(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_list_add")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_runtime_list_add", function_type, None)
            })
    }

    fn native_list_get(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_list_get")
            .unwrap_or_else(|| {
                let function_type = self
                    .ptr_type
                    .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
                self.module
                    .add_function("loom_runtime_list_get", function_type, None)
            })
    }

    fn native_int_list_reserve(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_int_list_reserve_v1")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
                self.module
                    .add_function("loom_int_list_reserve_v1", function_type, None)
            })
    }

    fn native_int_list_drop(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_int_list_drop_v1")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_int_list_drop_v1", function_type, None)
            })
    }

    fn native_text_get(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_text_get")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_text_get", function_type, None)
            })
    }

    fn native_bytes_append(&self) -> FunctionValue<'ctx> {
        self.native_two_data_output("loom_runtime_bytes_append")
    }

    fn native_text_concat(&self) -> FunctionValue<'ctx> {
        self.native_two_data_output("loom_runtime_text_concat")
    }

    fn native_text_contains(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_text_contains")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_text_contains", function_type, None)
            })
    }

    fn native_bytes_get(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_bytes_get")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_bytes_get", function_type, None)
            })
    }

    fn native_bytes_decode_utf8(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_bytes_decode_utf8")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_bytes_decode_utf8", function_type, None)
            })
    }

    fn native_path_contains_nul(&self) -> FunctionValue<'ctx> {
        self.native_data_length_predicate("loom_runtime_path_contains_nul")
    }

    fn native_path_join(&self) -> FunctionValue<'ctx> {
        self.native_two_data_output("loom_runtime_path_join")
    }

    fn native_text_map_get(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_text_map_get")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_text_map_get", function_type, None)
            })
    }

    fn native_text_map_insert(&self) -> FunctionValue<'ctx> {
        self.native_four_pointer_status("loom_runtime_text_map_insert")
    }

    fn native_text_map_remove(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_text_map_remove")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_text_map_remove", function_type, None)
            })
    }

    fn native_json_parse(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_json_parse")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_json_parse", function_type, None)
            })
    }

    fn native_json_format(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_json_format")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_json_format", function_type, None)
            })
    }

    fn native_log_write(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_log")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.context.i32_type().into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_runtime_log", function_type, None)
            })
    }

    fn native_four_pointer_status(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self.context.i32_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            );
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_data_length_predicate(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_two_data_output(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self.context.i32_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            );
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_value_summary(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_value_summary")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_runtime_value_summary", function_type, None)
            })
    }

    fn native_set_arguments(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_set_arguments")
            .unwrap_or_else(|| {
                let function_type = self.context.void_type().fn_type(
                    &[self.context.i32_type().into(), self.ptr_type.into()],
                    false,
                );
                self.module
                    .add_function("loom_runtime_set_arguments", function_type, None)
            })
    }

    fn native_process_arguments(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_process_arguments")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_runtime_process_arguments", function_type, None)
            })
    }

    fn native_process_environment(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_process_environment")
            .unwrap_or_else(|| {
                let function_type = self
                    .ptr_type
                    .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
                self.module
                    .add_function("loom_runtime_process_environment", function_type, None)
            })
    }

    fn native_file_open_read(&self) -> FunctionValue<'ctx> {
        self.native_io_task_text("loom_file_open_read")
    }

    fn native_file_create(&self) -> FunctionValue<'ctx> {
        self.native_io_task_text("loom_file_create")
    }

    fn native_file_try_open_read(&self) -> FunctionValue<'ctx> {
        self.native_io_task_text("loom_file_try_open_read")
    }

    fn native_file_try_create(&self) -> FunctionValue<'ctx> {
        self.native_io_task_text("loom_file_try_create")
    }

    fn native_socket_connect(&self) -> FunctionValue<'ctx> {
        self.native_socket_connect_named("loom_socket_connect")
    }

    fn native_socket_try_connect(&self) -> FunctionValue<'ctx> {
        self.native_socket_connect_named("loom_socket_try_connect")
    }

    fn native_socket_connect_named(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.i64_type.into(),
                ],
                false,
            );
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_file_read_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_descriptor("loom_file_read_text")
    }

    fn native_file_try_read_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_descriptor("loom_file_try_read_text")
    }

    fn native_socket_read_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_descriptor("loom_socket_read_text")
    }

    fn native_socket_try_read_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_descriptor("loom_socket_try_read_text")
    }

    fn native_file_write_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_write("loom_file_write_text")
    }

    fn native_file_try_write_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_write("loom_file_try_write_text")
    }

    fn native_socket_write_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_write("loom_socket_write_text")
    }

    fn native_socket_try_write_text(&self) -> FunctionValue<'ctx> {
        self.native_io_task_write("loom_socket_try_write_text")
    }

    fn native_io_close(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_io_close")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_io_close", function_type, None)
            })
    }

    fn native_io_task_text(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            );
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_io_task_descriptor(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self
                .ptr_type
                .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_io_task_write(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self.ptr_type.fn_type(
                &[
                    self.ptr_type.into(),
                    self.i64_type.into(),
                    self.ptr_type.into(),
                    self.i64_type.into(),
                ],
                false,
            );
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_gc_alloc_witness_node(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_gc_alloc_witness_node")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[], false);
                self.module
                    .add_function("loom_gc_alloc_witness_node", function_type, None)
            })
    }

    fn native_wait_now_ns(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_wait_now_ns")
            .unwrap_or_else(|| {
                let function_type = self.i64_type.fn_type(&[], false);
                self.module
                    .add_function("loom_wait_now_ns", function_type, None)
            })
    }

    fn native_runtime_create(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_create_v1")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[], false);
                self.module
                    .add_function("loom_runtime_create_v1", function_type, None)
            })
    }

    fn native_runtime_activate(&self) -> FunctionValue<'ctx> {
        self.native_runtime_status_function("loom_runtime_activate_v1")
    }

    fn native_runtime_deactivate(&self) -> FunctionValue<'ctx> {
        self.native_runtime_status_function("loom_runtime_deactivate_v1")
    }

    fn native_runtime_destroy(&self) -> FunctionValue<'ctx> {
        self.native_runtime_status_function("loom_runtime_destroy_v1")
    }

    fn native_runtime_status_function(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into()], false);
            self.module.add_function(name, function_type, None)
        })
    }

    fn native_executor_create_for_runtime(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_create_for_runtime_v1")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_executor_create_for_runtime_v1", function_type, None)
            })
    }

    fn native_executor_destroy(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_destroy")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .void_type()
                    .fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_executor_destroy", function_type, None)
            })
    }

    fn native_executor_register(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_register")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_executor_register", function_type, None)
            })
    }

    fn native_executor_notify_completion(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_notify_completion")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.context.i32_type().into(),
                        self.context.i32_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_executor_notify_completion", function_type, None)
            })
    }

    fn native_executor_wait(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_wait")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_executor_wait", function_type, None)
            })
    }

    fn native_executor_pop_ready(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_pop_ready")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_executor_pop_ready", function_type, None)
            })
    }

    fn native_task_spawn_descriptor(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_spawn_descriptor")
            .unwrap_or_else(|| {
                let function_type = self
                    .ptr_type
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_spawn_descriptor", function_type, None)
            })
    }

    fn native_task_trace_live_slots(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_trace_live_slots")
            .unwrap_or_else(|| {
                let function_type = self.context.void_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_trace_live_slots", function_type, None)
            })
    }

    fn native_task_clone_witness(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_clone_witness")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_clone_witness", function_type, None)
            })
    }

    fn native_task_from_wait_source(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_from_wait_source")
            .unwrap_or_else(|| {
                let function_type = self
                    .ptr_type
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_from_wait_source", function_type, None)
            })
    }

    fn native_task_slot(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_slot")
            .unwrap_or_else(|| {
                let function_type = self
                    .ptr_type
                    .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
                self.module
                    .add_function("loom_task_slot", function_type, None)
            })
    }

    fn native_task_result(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_result")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_result", function_type, None)
            })
    }

    fn native_task_state(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_state")
            .unwrap_or_else(|| {
                let function_type = self.i64_type.fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_state", function_type, None)
            })
    }

    fn native_task_set_state(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_set_state")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .void_type()
                    .fn_type(&[self.ptr_type.into(), self.i64_type.into()], false);
                self.module
                    .add_function("loom_task_set_state", function_type, None)
            })
    }

    fn native_task_is_cancelled(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_is_cancelled")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_is_cancelled", function_type, None)
            })
    }

    fn native_task_set_fault(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_set_fault")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                        self.ptr_type.into(),
                        self.i64_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_set_fault", function_type, None)
            })
    }

    fn native_task_report_fault(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_report_fault")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_report_fault", function_type, None)
            })
    }

    fn native_context_raise_fault(&self) -> Result<FunctionValue<'ctx>, CodegenError> {
        if let Some(function) = self.module.get_function("loom_context_raise_fault_v1") {
            return Ok(function);
        }
        let function_type = self.context.i32_type().fn_type(
            &[
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.i64_type.into(),
                self.ptr_type.into(),
                self.i64_type.into(),
                self.ptr_type.into(),
                self.i64_type.into(),
                self.ptr_type.into(),
                self.i64_type.into(),
            ],
            false,
        );
        let function = self
            .module
            .add_function("loom_context_raise_fault_v1", function_type, None);
        mark_cold_noinline(self.context, function)?;
        Ok(function)
    }

    fn native_task_join_step(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_join_step")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_task_join_step", function_type, None)
            })
    }

    fn native_task_write_join_result(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_write_join_result")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.context.i32_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_write_join_result", function_type, None)
            })
    }

    fn native_join_create(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_join_create")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i32_type().into(),
                        self.context.i32_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_join_create", function_type, None)
            })
    }

    fn native_join_task(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_join_task")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[self.ptr_type.into()], false);
                self.module
                    .add_function("loom_join_task", function_type, None)
            })
    }

    fn native_join_add_task(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_join_add_task")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_join_add_task", function_type, None)
            })
    }

    fn native_join_add_list(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_join_add_list")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_join_add_list", function_type, None)
            })
    }

    fn native_task_suspend_value(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_suspend_value")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_suspend_value", function_type, None)
            })
    }

    fn native_executor_run(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_run")
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function("loom_executor_run", function_type, None)
            })
    }

    fn puts(&self, value: &str) -> Result<(), CodegenError> {
        let string = self
            .builder
            .build_global_string_ptr(value, &self.unique("string"))
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.libc_puts(),
                &[string.as_pointer_value().into()],
                "puts",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn set_task_fault(
        &self,
        task: PointerValue<'ctx>,
        code: &str,
        message: &str,
    ) -> Result<(), CodegenError> {
        let code_data = self
            .builder
            .build_global_string_ptr(code, &self.unique("fault.code"))
            .map_err(builder_error)?;
        let message_data = self
            .builder
            .build_global_string_ptr(message, &self.unique("fault.message"))
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.native_task_set_fault(),
                &[
                    task.into(),
                    code_data.as_pointer_value().into(),
                    self.i64_type.const_int(code.len() as u64, false).into(),
                    message_data.as_pointer_value().into(),
                    self.i64_type.const_int(message.len() as u64, false).into(),
                ],
                "task.fault.set",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn raise_fault(
        &self,
        context: PointerValue<'ctx>,
        code: &str,
        message: &str,
        display: &str,
        detail: &str,
    ) -> Result<(), CodegenError> {
        let code_data = self
            .builder
            .build_global_string_ptr(code, &self.unique("fault.code"))
            .map_err(builder_error)?;
        let message_data = self
            .builder
            .build_global_string_ptr(message, &self.unique("fault.message"))
            .map_err(builder_error)?;
        let display_data = self
            .builder
            .build_global_string_ptr(display, &self.unique("fault.display"))
            .map_err(builder_error)?;
        let detail_data = self
            .builder
            .build_global_string_ptr(detail, &self.unique("fault.detail"))
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.native_context_raise_fault()?,
                &[
                    context.into(),
                    code_data.as_pointer_value().into(),
                    self.i64_type.const_int(code.len() as u64, false).into(),
                    message_data.as_pointer_value().into(),
                    self.i64_type.const_int(message.len() as u64, false).into(),
                    display_data.as_pointer_value().into(),
                    self.i64_type.const_int(display.len() as u64, false).into(),
                    detail_data.as_pointer_value().into(),
                    self.i64_type.const_int(detail.len() as u64, false).into(),
                ],
                "fault.raise",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn printf(
        &self,
        format: &str,
        values: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<(), CodegenError> {
        let string = self
            .builder
            .build_global_string_ptr(format, &self.unique("format"))
            .map_err(builder_error)?;
        let mut arguments = Vec::with_capacity(values.len() + 1);
        arguments.push(string.as_pointer_value().into());
        arguments.extend_from_slice(values);
        self.builder
            .build_call(self.libc_printf(), &arguments, "printf")
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_function(&self, id: FunctionId) -> Result<(), CodegenError> {
        let source = self
            .program
            .function(id)
            .ok_or_else(|| CodegenError::new("InvalidFunctionReference", "function is missing"))?;
        self.set_debug_location(
            self.functions[&id],
            source.span.file.0,
            source.span.range.start,
        );
        let result = FunctionCompiler::new(self, id)?.compile();
        self.builder.unset_current_debug_location();
        result
    }

    fn emit_native_function(&self, id: FunctionId) -> Result<(), CodegenError> {
        let source = self.program.function(id).ok_or_else(|| {
            CodegenError::new("InvalidFunctionReference", "native function is missing")
        })?;
        self.set_debug_location(
            self.native_functions[&id].function,
            source.span.file.0,
            source.span.range.start,
        );
        let result = FunctionCompiler::new_native(self, id)?.compile();
        self.builder.unset_current_debug_location();
        result
    }

    fn emit_native_wrapper(&self, id: FunctionId) -> Result<(), CodegenError> {
        let source = self.program.function(id).ok_or_else(|| {
            CodegenError::new("InvalidFunctionReference", "native function is missing")
        })?;
        let wrapper = self.functions[&id];
        self.set_debug_location(wrapper, source.span.file.0, source.span.range.start);
        let output = parameter_pointer(wrapper, 0)?;
        let mut argument_node = parameter_pointer(wrapper, 1)?;
        let entry = self.context.append_basic_block(wrapper, "entry");
        self.builder.position_at_end(entry);
        self.builder
            .build_store(output, self.value_type.const_zero())
            .map_err(builder_error)?;

        let declaration = &self.native_functions[&id];
        let signature = &declaration.signature;
        let mut call_arguments = Vec::<BasicMetadataValueEnum<'ctx>>::with_capacity(
            source.params.len() + usize::from(signature.effect() == NativeEffectAbi::RuntimeStatus),
        );
        for _ in &source.params {
            let argument = self.load_pointer_field(
                self.arg_node_type,
                argument_node,
                ARG_NODE_FIELD_VALUE,
                "argument",
            )?;
            call_arguments.push(
                self.load_i64_field(
                    self.value_type,
                    argument,
                    VALUE_FIELD_SCALAR,
                    "argument.scalar",
                )?
                .into(),
            );
            argument_node = self.load_pointer_field(
                self.arg_node_type,
                argument_node,
                ARG_NODE_FIELD_NEXT,
                "argument.next",
            )?;
        }
        let scalar = if signature.effect() == NativeEffectAbi::PureNoFault {
            call_int(
                &self.builder,
                declaration.function,
                &call_arguments,
                "integer.call",
            )?
        } else {
            call_arguments.push(parameter_pointer(wrapper, 3)?.into());
            let (status, scalar) = call_native_status(
                &self.builder,
                declaration.function,
                signature,
                &call_arguments,
                "integer.call",
            )?;
            let success = self.context.append_basic_block(wrapper, "call.success");
            let failure = self.context.append_basic_block(wrapper, "call.failure");
            let ok = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    status,
                    self.context.i32_type().const_zero(),
                    "integer.call.ok",
                )
                .map_err(builder_error)?;
            build_weighted_conditional_branch(
                self.context,
                &self.builder,
                ok,
                success,
                failure,
                LikelyBranch::Then,
            )?;

            self.builder.position_at_end(failure);
            self.builder
                .build_return(Some(&status))
                .map_err(builder_error)?;
            self.builder.position_at_end(success);
            scalar
        };
        self.store_i64_field(
            self.value_type,
            output,
            VALUE_FIELD_TAG,
            self.tag(VALUE_TAG_INT),
        )?;
        self.store_i64_field(self.value_type, output, VALUE_FIELD_SCALAR, scalar)?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_zero()))
            .map_err(builder_error)?;
        self.builder.unset_current_debug_location();
        Ok(())
    }

    fn emit_async_resume(&self, id: FunctionId) -> Result<(), CodegenError> {
        let source = self.program.function(id).ok_or_else(|| {
            CodegenError::new("InvalidFunctionReference", "async function is missing")
        })?;
        self.set_debug_location(
            self.task_resumes[&id],
            source.span.file.0,
            source.span.range.start,
        );
        let result = FunctionCompiler::new_async(self, id)?.compile();
        self.builder.unset_current_debug_location();
        result
    }

    #[allow(clippy::too_many_lines)]
    fn emit_async_constructor(&self, id: FunctionId) -> Result<(), CodegenError> {
        let source = self.program.function(id).ok_or_else(|| {
            CodegenError::new("InvalidFunctionReference", "async constructor is missing")
        })?;
        let layout = AsyncLayout::new(source)?;
        let constructor = self.functions[&id];
        self.set_debug_location(constructor, source.span.file.0, source.span.range.start);
        let output = parameter_pointer(constructor, 0)?;
        let mut argument = parameter_pointer(constructor, 1)?;
        let mut witness_argument = parameter_pointer(constructor, 2)?;
        let executor = parameter_pointer(constructor, 3)?;
        let entry = self.context.append_basic_block(constructor, "entry");
        let ready = self.context.append_basic_block(constructor, "task.ready");
        let failed = self.context.append_basic_block(constructor, "task.failed");
        self.builder.position_at_end(entry);
        let descriptor = self.coroutine_descriptors[&id].as_pointer_value();
        let task = call_pointer(
            &self.builder,
            self.native_task_spawn_descriptor(),
            &[executor.into(), descriptor.into()],
            "task.spawn",
        )?;
        let exists = self
            .builder
            .build_is_not_null(task, "task.exists")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exists, ready, failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(failed);
        self.puts("RuntimeFault: task allocation failed")?;
        self.builder
            .build_return(Some(
                &self.context.i32_type().const_int(TASK_STEP_FAULTED, false),
            ))
            .map_err(builder_error)?;

        self.builder.position_at_end(ready);
        let clone = self
            .module
            .get_function("loom.runtime.clone")
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "clone helper is missing"))?;
        for parameter in &source.params {
            let value = self.load_pointer_field(
                self.arg_node_type,
                argument,
                ARG_NODE_FIELD_VALUE,
                "task.argument",
            )?;
            let slot = call_pointer(
                &self.builder,
                self.native_task_slot(),
                &[
                    task.into(),
                    self.i64_type
                        .const_int(layout.local_slots[&parameter.id], false)
                        .into(),
                ],
                "task.parameter.slot",
            )?;
            self.builder
                .build_call(clone, &[slot.into(), value.into()], "task.parameter.clone")
                .map_err(builder_error)?;
            let old = call_pointer(
                &self.builder,
                self.native_task_slot(),
                &[
                    task.into(),
                    self.i64_type
                        .const_int(layout.old_parameter_slots[&parameter.id], false)
                        .into(),
                ],
                "task.old.slot",
            )?;
            self.builder
                .build_call(clone, &[old.into(), value.into()], "task.old.clone")
                .map_err(builder_error)?;
            argument = self.load_pointer_field(
                self.arg_node_type,
                argument,
                ARG_NODE_FIELD_NEXT,
                "task.argument.next",
            )?;
        }
        let witness_field_count = u64::try_from(self.program.requirements.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many requirements"))?;
        for index in 0..source.witness_params.len() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many task witnesses"))?;
            let witness = self.load_pointer_field(
                self.witness_node_type,
                witness_argument,
                WITNESS_NODE_FIELD_VALUE,
                "task.witness",
            )?;
            let witness = call_pointer(
                &self.builder,
                self.native_task_clone_witness(),
                &[
                    task.into(),
                    witness.into(),
                    self.i64_type.const_int(witness_field_count, false).into(),
                ],
                "task.witness.clone",
            )?;
            let slot = call_pointer(
                &self.builder,
                self.native_task_slot(),
                &[
                    task.into(),
                    self.i64_type
                        .const_int(layout.witness_slots[&index], false)
                        .into(),
                ],
                "task.witness.slot",
            )?;
            self.store_pointer_field(self.value_type, slot, VALUE_FIELD_DATA, witness)?;
            witness_argument = self.load_pointer_field(
                self.witness_node_type,
                witness_argument,
                WITNESS_NODE_FIELD_NEXT,
                "task.witness.next",
            )?;
        }
        self.builder
            .build_store(output, self.value_type.const_zero())
            .map_err(builder_error)?;
        self.store_i64_field(
            self.value_type,
            output,
            VALUE_FIELD_TAG,
            self.tag(VALUE_TAG_TASK),
        )?;
        self.store_pointer_field(self.value_type, output, VALUE_FIELD_DATA, task)?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_zero()))
            .map_err(builder_error)?;
        self.builder.unset_current_debug_location();
        Ok(())
    }

    fn root_context_plan(&self, root: FunctionId) -> Result<RootContextPlan, CodegenError> {
        let requirements = self.requirements.function(root)?.invocation;
        // The first context-free root slice is intentionally restricted to a
        // private typed ABI whose wrapper only unboxes/boxes scalar words.
        // Universal Value bodies may hide representation allocations even
        // when their source operations look pure, so they retain a standalone
        // runtime until their typed layout exists.
        if self.native_functions.get(&root).is_some_and(|declaration| {
            declaration.signature.effect() == NativeEffectAbi::PureNoFault
        }) && requirements.is_pure_no_fault()
        {
            return Ok(RootContextPlan::None);
        }
        if requirements.needs_executor()
            || self
                .program
                .function(root)
                .is_some_and(|function| function.is_async)
        {
            Ok(RootContextPlan::Executor)
        } else {
            Ok(RootContextPlan::Runtime)
        }
    }

    #[allow(clippy::too_many_lines)]
    fn create_root_context(
        &self,
        plan: RootContextPlan,
    ) -> Result<RootContext<'ctx>, CodegenError> {
        let null = self.ptr_type.const_null();
        if plan == RootContextPlan::None {
            return Ok(RootContext {
                plan,
                runtime: null,
                executor: null,
            });
        }
        let function = self
            .builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_parent)
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "root has no function"))?;
        let runtime = call_pointer(
            &self.builder,
            self.native_runtime_create(),
            &[],
            "runtime.root",
        )?;
        let ready = self
            .context
            .append_basic_block(function, "runtime.root.ready");
        let failed = self
            .context
            .append_basic_block(function, "runtime.root.failed");
        let exists = self
            .builder
            .build_is_not_null(runtime, "runtime.root.exists")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exists, ready, failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(failed);
        self.puts("RuntimeFault: runtime creation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        self.builder.position_at_end(ready);
        let activation = call_int(
            &self.builder,
            self.native_runtime_activate(),
            &[runtime.into()],
            "runtime.root.activate",
        )?;
        let activated = self
            .context
            .append_basic_block(function, "runtime.root.activated");
        let activation_failed = self
            .context
            .append_basic_block(function, "runtime.root.activation.failed");
        let activation_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                activation,
                self.context.i32_type().const_zero(),
                "runtime.root.activation.ok",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(activation_ok, activated, activation_failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(activation_failed);
        self.builder
            .build_call(
                self.native_runtime_destroy(),
                &[runtime.into()],
                "runtime.root.activation.destroy",
            )
            .map_err(builder_error)?;
        self.puts("RuntimeFault: runtime activation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        self.builder.position_at_end(activated);
        if plan == RootContextPlan::Runtime {
            return Ok(RootContext {
                plan,
                runtime,
                executor: null,
            });
        }

        let executor = call_pointer(
            &self.builder,
            self.native_executor_create_for_runtime(),
            &[runtime.into()],
            "executor.root",
        )?;
        let executor_ready = self
            .context
            .append_basic_block(function, "executor.root.ready");
        let executor_failed = self
            .context
            .append_basic_block(function, "executor.root.failed");
        let executor_exists = self
            .builder
            .build_is_not_null(executor, "executor.root.exists")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(executor_exists, executor_ready, executor_failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(executor_failed);
        self.builder
            .build_call(
                self.native_runtime_deactivate(),
                &[runtime.into()],
                "executor.root.failure.deactivate",
            )
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.native_runtime_destroy(),
                &[runtime.into()],
                "executor.root.failure.destroy.runtime",
            )
            .map_err(builder_error)?;
        self.puts("RuntimeFault: executor creation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        self.builder.position_at_end(executor_ready);
        Ok(RootContext {
            plan,
            runtime,
            executor,
        })
    }

    fn destroy_root_context(&self, context: RootContext<'ctx>) -> Result<(), CodegenError> {
        if context.plan == RootContextPlan::Executor {
            self.builder
                .build_call(
                    self.native_executor_destroy(),
                    &[context.executor.into()],
                    "executor.root.destroy",
                )
                .map_err(builder_error)?;
        }
        if context.plan != RootContextPlan::None {
            self.builder
                .build_call(
                    self.native_runtime_deactivate(),
                    &[context.runtime.into()],
                    "runtime.root.deactivate",
                )
                .map_err(builder_error)?;
            self.builder
                .build_call(
                    self.native_runtime_destroy(),
                    &[context.runtime.into()],
                    "runtime.root.destroy",
                )
                .map_err(builder_error)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn emit_main(&self) -> Result<(), CodegenError> {
        let main_type = self.context.i32_type().fn_type(
            &[self.context.i32_type().into(), self.ptr_type.into()],
            false,
        );
        let main = self.module.add_function("main", main_type, None);
        let entry = self.context.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        let argument_count = parameter_int(main, 0)?;
        let argument_vector = parameter_pointer(main, 1)?;
        self.builder
            .build_call(
                self.native_set_arguments(),
                &[argument_count.into(), argument_vector.into()],
                "process.arguments.initialize",
            )
            .map_err(builder_error)?;
        let result = self
            .builder
            .build_alloca(self.value_type, "result")
            .map_err(builder_error)?;
        let null = self.ptr_type.const_null();
        match &self.options.kind {
            EmitKind::Run { .. } => {
                let root = self
                    .roots
                    .functions()
                    .iter()
                    .next()
                    .copied()
                    .ok_or_else(|| {
                        CodegenError::new("NoCompilationRoots", "run harness has no root")
                    })?;
                let context = self.create_root_context(self.root_context_plan(root)?)?;
                let mut status = call_int(
                    &self.builder,
                    self.functions[&root],
                    &[
                        result.into(),
                        null.into(),
                        null.into(),
                        context.hidden().into(),
                    ],
                    "run",
                )?;
                if self
                    .program
                    .function(root)
                    .is_some_and(|function| function.is_async)
                {
                    status = self.drive_async_root(status, result, context.executor)?;
                }
                let success = self.context.append_basic_block(main, "run.success");
                let failure = self.context.append_basic_block(main, "run.failure");
                let ok = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        status,
                        self.context.i32_type().const_zero(),
                        "run.ok",
                    )
                    .map_err(builder_error)?;
                self.builder
                    .build_conditional_branch(ok, success, failure)
                    .map_err(builder_error)?;
                self.builder.position_at_end(success);
                let print = self
                    .module
                    .get_function("loom.runtime.print")
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "print helper is missing"))?;
                self.builder
                    .build_call(print, &[result.into()], "print.result")
                    .map_err(builder_error)?;
                // The root result can contain pointers into the moving heap
                // owned by this execution context. Consume it before tearing
                // that context down; destroying first would leave print with
                // dangling aggregate/Text/container payloads.
                self.destroy_root_context(context)?;
                self.builder
                    .build_return(Some(&self.context.i32_type().const_zero()))
                    .map_err(builder_error)?;
                self.builder.position_at_end(failure);
                self.destroy_root_context(context)?;
                self.builder
                    .build_return(Some(&status))
                    .map_err(builder_error)?;
            }
            EmitKind::Tests => {
                let failed = self
                    .builder
                    .build_alloca(self.context.i32_type(), "tests.failed")
                    .map_err(builder_error)?;
                self.builder
                    .build_store(failed, self.context.i32_type().const_zero())
                    .map_err(builder_error)?;
                for root in self.roots.functions() {
                    let context = self.create_root_context(self.root_context_plan(*root)?)?;
                    let mut status = call_int(
                        &self.builder,
                        self.functions[root],
                        &[
                            result.into(),
                            null.into(),
                            null.into(),
                            context.hidden().into(),
                        ],
                        "test",
                    )?;
                    if self
                        .program
                        .function(*root)
                        .is_some_and(|function| function.is_async)
                    {
                        status = self.drive_async_root(status, result, context.executor)?;
                    }
                    let status_ok = self
                        .builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            status,
                            self.context.i32_type().const_zero(),
                            "test.status.ok",
                        )
                        .map_err(builder_error)?;
                    let value_ok = self.test_value_passed(result)?;
                    let passed = self
                        .builder
                        .build_and(status_ok, value_ok, "test.passed")
                        .map_err(builder_error)?;
                    // `test_value_passed` may inspect a heap-backed root
                    // result. Finish that inspection before releasing the
                    // per-test execution context.
                    self.destroy_root_context(context)?;
                    let pass = self.context.append_basic_block(main, "test.pass");
                    let fail = self.context.append_basic_block(main, "test.fail");
                    let next = self.context.append_basic_block(main, "test.next");
                    self.builder
                        .build_conditional_branch(passed, pass, fail)
                        .map_err(builder_error)?;
                    let name = &self
                        .program
                        .function(*root)
                        .ok_or_else(|| {
                            CodegenError::new("InvalidFunctionReference", "test root is missing")
                        })?
                        .name;
                    self.builder.position_at_end(pass);
                    self.puts(&format!("passed {name}"))?;
                    self.branch(next)?;
                    self.builder.position_at_end(fail);
                    self.puts(&format!("failed {name}"))?;
                    self.builder
                        .build_store(failed, self.context.i32_type().const_int(1, false))
                        .map_err(builder_error)?;
                    self.branch(next)?;
                    self.builder.position_at_end(next);
                }
                let status = self
                    .builder
                    .build_load(self.context.i32_type(), failed, "tests.status")
                    .map_err(builder_error)?
                    .into_int_value();
                self.builder
                    .build_return(Some(&status))
                    .map_err(builder_error)?;
            }
        }
        Ok(())
    }

    fn drive_async_root(
        &self,
        constructor_status: IntValue<'ctx>,
        result: PointerValue<'ctx>,
        executor: PointerValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_parent)
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "root has no function"))?;
        let run = self.context.append_basic_block(function, "task.root.run");
        let constructor_failed = self
            .context
            .append_basic_block(function, "task.root.constructor.failed");
        let copy = self.context.append_basic_block(function, "task.root.copy");
        let run_failed = self
            .context
            .append_basic_block(function, "task.root.run.failed");
        let merge = self.context.append_basic_block(function, "task.root.merge");
        let constructed = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                constructor_status,
                self.context.i32_type().const_zero(),
                "task.root.constructed",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(constructed, run, constructor_failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(constructor_failed);
        self.branch(merge)?;

        self.builder.position_at_end(run);
        let task = self.load_pointer_field(
            self.value_type,
            result,
            VALUE_FIELD_DATA,
            "task.root.pointer",
        )?;
        let run_status = call_int(
            &self.builder,
            self.native_executor_run(),
            &[executor.into(), task.into()],
            "task.root.status",
        )?;
        let completed = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                run_status,
                self.context
                    .i32_type()
                    .const_int(TASK_STEP_COMPLETED, false),
                "task.root.completed",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(completed, copy, run_failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(copy);
        let task_result = call_pointer(
            &self.builder,
            self.native_task_result(),
            &[task.into()],
            "task.root.result",
        )?;
        let clone = self
            .module
            .get_function("loom.runtime.clone")
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "clone helper is missing"))?;
        self.builder
            .build_call(
                clone,
                &[result.into(), task_result.into()],
                "task.root.copy",
            )
            .map_err(builder_error)?;
        self.branch(merge)?;
        self.builder.position_at_end(run_failed);
        self.builder
            .build_call(
                self.native_task_report_fault(),
                &[task.into()],
                "task.root.fault.report",
            )
            .map_err(builder_error)?;
        self.branch(merge)?;

        self.builder.position_at_end(merge);
        let status = self
            .builder
            .build_phi(self.context.i32_type(), "task.root.merged.status")
            .map_err(builder_error)?;
        status.add_incoming(&[
            (&constructor_status, constructor_failed),
            (&run_status, copy),
            (&run_status, run_failed),
        ]);
        Ok(status.as_basic_value().into_int_value())
    }

    #[allow(clippy::too_many_lines)]
    fn test_value_passed(&self, value: PointerValue<'ctx>) -> Result<IntValue<'ctx>, CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "test harness has no function")
            })?;
        let tag = self.load_i64_field(self.value_type, value, VALUE_FIELD_TAG, "test.tag")?;
        let is_unit = self
            .builder
            .build_int_compare(IntPredicate::EQ, tag, self.tag(VALUE_TAG_UNIT), "test.unit")
            .map_err(builder_error)?;
        let unit_block = self.context.append_basic_block(function, "test.value.unit");
        let non_unit = self
            .context
            .append_basic_block(function, "test.value.non_unit");
        let enum_block = self.context.append_basic_block(function, "test.value.enum");
        let other_block = self
            .context
            .append_basic_block(function, "test.value.other");
        let merge = self
            .context
            .append_basic_block(function, "test.value.merge");
        self.builder
            .build_conditional_branch(is_unit, unit_block, non_unit)
            .map_err(builder_error)?;
        self.builder.position_at_end(unit_block);
        self.branch(merge)?;
        self.builder.position_at_end(non_unit);
        let is_enum = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                tag,
                self.tag(VALUE_TAG_ENUM),
                "test.result",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(is_enum, enum_block, other_block)
            .map_err(builder_error)?;
        self.builder.position_at_end(other_block);
        self.branch(merge)?;
        self.builder.position_at_end(enum_block);
        let variant =
            self.load_i64_field(self.value_type, value, VALUE_FIELD_AUX, "test.variant")?;
        let ok_variant = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                variant,
                self.i64_type.const_zero(),
                "test.ok.variant",
            )
            .map_err(builder_error)?;
        let count = self.load_i64_field(
            self.value_type,
            value,
            VALUE_FIELD_SCALAR,
            "test.payload.count",
        )?;
        let one_payload = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                count,
                self.i64_type.const_int(1, false),
                "test.one.payload",
            )
            .map_err(builder_error)?;
        let data =
            self.load_pointer_field(self.value_type, value, VALUE_FIELD_DATA, "test.payload")?;
        let payload_value = self.struct_pointer(
            self.value_node_type,
            data,
            VALUE_NODE_FIELD_VALUE,
            "test.payload.value",
        )?;
        let payload_tag = self.load_i64_field(
            self.value_type,
            payload_value,
            VALUE_FIELD_TAG,
            "test.payload.tag",
        )?;
        let unit_payload = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                payload_tag,
                self.tag(VALUE_TAG_UNIT),
                "test.unit.payload",
            )
            .map_err(builder_error)?;
        let result = self
            .builder
            .build_and(ok_variant, one_payload, "test.result.one")
            .map_err(builder_error)?;
        let result = self
            .builder
            .build_and(result, unit_payload, "test.result.unit")
            .map_err(builder_error)?;
        self.branch(merge)?;
        self.builder.position_at_end(merge);
        let phi = self
            .builder
            .build_phi(self.context.bool_type(), "test.value.passed")
            .map_err(builder_error)?;
        let yes = self.context.bool_type().const_int(1, false);
        let no = self.context.bool_type().const_zero();
        phi.add_incoming(&[
            (&yes, unit_block),
            (&no, other_block),
            (&result, enum_block),
        ]);
        Ok(phi.as_basic_value().into_int_value())
    }

    fn tag(&self, tag: u64) -> IntValue<'ctx> {
        self.i64_type.const_int(tag, false)
    }

    fn signed_i64(&self, value: i64) -> IntValue<'ctx> {
        self.i64_type
            .const_int(u64::from_ne_bytes(value.to_ne_bytes()), true)
    }

    fn struct_pointer<T: BasicType<'ctx>>(
        &self,
        structure: T,
        pointer: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.builder
            .build_struct_gep(structure, pointer, field, name)
            .map_err(builder_error)
    }

    fn load_i64_field(
        &self,
        structure: StructType<'ctx>,
        pointer: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let field = self.struct_pointer(structure, pointer, field, name)?;
        self.builder
            .build_load(self.i64_type, field, name)
            .map_err(builder_error)
            .map(BasicValueEnum::into_int_value)
    }

    fn store_i64_field(
        &self,
        structure: StructType<'ctx>,
        pointer: PointerValue<'ctx>,
        field: u32,
        value: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let field = self.struct_pointer(structure, pointer, field, "field")?;
        self.builder
            .build_store(field, value)
            .map_err(builder_error)?;
        Ok(())
    }

    fn store_i32_field(
        &self,
        structure: StructType<'ctx>,
        pointer: PointerValue<'ctx>,
        field: u32,
        value: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let field = self.struct_pointer(structure, pointer, field, "field")?;
        self.builder
            .build_store(field, value)
            .map_err(builder_error)?;
        Ok(())
    }

    fn load_i32_field(
        &self,
        structure: StructType<'ctx>,
        pointer: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let field = self.struct_pointer(structure, pointer, field, name)?;
        self.builder
            .build_load(self.context.i32_type(), field, name)
            .map_err(builder_error)
            .map(BasicValueEnum::into_int_value)
    }

    fn load_pointer_field(
        &self,
        structure: StructType<'ctx>,
        pointer: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let field = self.struct_pointer(structure, pointer, field, name)?;
        self.builder
            .build_load(self.ptr_type, field, name)
            .map_err(builder_error)
            .map(BasicValueEnum::into_pointer_value)
    }

    fn sequence_parts(
        &self,
        value: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let object = self.load_pointer_field(
            self.value_type,
            value,
            VALUE_FIELD_DATA,
            &format!("{name}.object"),
        )?;
        let length = self.load_i64_field(
            self.text_object_type,
            object,
            TEXT_OBJECT_FIELD_BYTE_LENGTH,
            &format!("{name}.length"),
        )?;
        let data = self.struct_pointer(
            self.text_object_type,
            object,
            TEXT_OBJECT_FIELD_BYTES,
            &format!("{name}.data"),
        )?;
        Ok((object, data, length))
    }

    fn text_scalar_length(
        &self,
        value: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let object = self.load_pointer_field(
            self.value_type,
            value,
            VALUE_FIELD_DATA,
            &format!("{name}.object"),
        )?;
        self.load_i64_field(
            self.text_object_type,
            object,
            TEXT_OBJECT_FIELD_SCALAR_LENGTH,
            &format!("{name}.scalar_length"),
        )
    }

    fn store_pointer_field(
        &self,
        structure: StructType<'ctx>,
        pointer: PointerValue<'ctx>,
        field: u32,
        value: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let field = self.struct_pointer(structure, pointer, field, "field")?;
        self.builder
            .build_store(field, value)
            .map_err(builder_error)?;
        Ok(())
    }

    fn compare_i64_field(
        &self,
        left: PointerValue<'ctx>,
        right: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let left = self.load_i64_field(self.value_type, left, field, &format!("left.{name}"))?;
        let right = self.load_i64_field(self.value_type, right, field, &format!("right.{name}"))?;
        self.builder
            .build_int_compare(IntPredicate::EQ, left, right, &format!("same.{name}"))
            .map_err(builder_error)
    }

    fn branch(&self, target: inkwell::basic_block::BasicBlock<'ctx>) -> Result<(), CodegenError> {
        self.builder
            .build_unconditional_branch(target)
            .map_err(builder_error)?;
        Ok(())
    }

    fn unique(&self, prefix: &str) -> String {
        let value = self.names.get();
        self.names.set(value + 1);
        format!("{prefix}.{value}")
    }
}

#[derive(Clone, Copy)]
struct CompletionWait<'ctx> {
    executor: PointerValue<'ctx>,
    frame: PointerValue<'ctx>,
    registration: PointerValue<'ctx>,
}

struct AsyncLayout {
    local_slots: BTreeMap<LocalId, u64>,
    old_parameter_slots: BTreeMap<LocalId, u64>,
    witness_slots: BTreeMap<u32, u64>,
    result_slot: u64,
    slot_count: u64,
}

impl AsyncLayout {
    fn new(function: &Function) -> Result<Self, CodegenError> {
        let mut next = 0_u64;
        let mut local_slots = BTreeMap::new();
        for local in function.params.iter().chain(&function.locals) {
            local_slots.insert(local.id, next);
            next = next
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many task slots"))?;
        }
        let mut old_parameter_slots = BTreeMap::new();
        for parameter in &function.params {
            old_parameter_slots.insert(parameter.id, next);
            next = next
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many task slots"))?;
        }
        let mut witness_slots = BTreeMap::new();
        for index in 0..function.witness_params.len() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many task witnesses"))?;
            witness_slots.insert(index, next);
            next = next
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many task slots"))?;
        }
        let result_slot = next;
        let slot_count = next
            .checked_add(1)
            .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many task slots"))?;
        Ok(Self {
            local_slots,
            old_parameter_slots,
            witness_slots,
            result_slot,
            slot_count,
        })
    }
}

struct FunctionCompiler<'backend, 'ctx, 'program> {
    backend: &'backend Backend<'ctx, 'program>,
    source: &'program Function,
    function: FunctionValue<'ctx>,
    output: PointerValue<'ctx>,
    native_signature: Option<&'backend NativeSignature>,
    witness_parameters: BTreeMap<u32, PointerValue<'ctx>>,
    runtime_context: PointerValue<'ctx>,
    task: Option<PointerValue<'ctx>>,
    loop_depth: Cell<u32>,
    active_range_local: Cell<Option<LocalId>>,
    resume_blocks: BTreeMap<u32, inkwell::basic_block::BasicBlock<'ctx>>,
    locals: BTreeMap<LocalId, PointerValue<'ctx>>,
    stack_record_nodes: BTreeMap<LocalId, Vec<PointerValue<'ctx>>>,
    native_int_list_plan: NativeIntListPlan,
    native_int_lists: BTreeMap<LocalId, PointerValue<'ctx>>,
    old_parameters: BTreeMap<LocalId, PointerValue<'ctx>>,
    body_done: inkwell::basic_block::BasicBlock<'ctx>,
    cleanups: RefCell<Vec<Block>>,
    cancellation_block: Option<inkwell::basic_block::BasicBlock<'ctx>>,
    cancellation_cleanups: RefCell<BTreeMap<u32, Vec<Block>>>,
    unwind_status: Cell<Option<u64>>,
}

impl<'backend, 'ctx, 'program> FunctionCompiler<'backend, 'ctx, 'program> {
    #[allow(clippy::too_many_lines)]
    fn new(
        backend: &'backend Backend<'ctx, 'program>,
        id: FunctionId,
    ) -> Result<Self, CodegenError> {
        let source = backend.program.function(id).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("function #{} does not exist", id.0),
            )
        })?;
        let function = backend.functions[&id];
        let output = parameter_pointer(function, 0)?;
        let arguments = parameter_pointer(function, 1)?;
        let mut witness_argument = parameter_pointer(function, 2)?;
        let runtime_context = parameter_pointer(function, 3)?;
        let entry = backend.context.append_basic_block(function, "entry");
        let body_done = backend.context.append_basic_block(function, "body.done");
        backend.builder.position_at_end(entry);

        let mut locals = BTreeMap::new();
        let mut argument_node = arguments;
        for parameter in &source.params {
            let pointer = backend.load_pointer_field(
                backend.arg_node_type,
                argument_node,
                ARG_NODE_FIELD_VALUE,
                "argument",
            )?;
            locals.insert(parameter.id, pointer);
            argument_node = backend.load_pointer_field(
                backend.arg_node_type,
                argument_node,
                ARG_NODE_FIELD_NEXT,
                "argument.next",
            )?;
        }
        for local in &source.locals {
            let pointer = backend
                .builder
                .build_alloca(
                    backend.value_type,
                    &format!("local.{}.{}", local.id.0, local.name),
                )
                .map_err(builder_error)?;
            backend
                .builder
                .build_store(pointer, backend.value_type.const_zero())
                .map_err(builder_error)?;
            locals.insert(local.id, pointer);
        }
        backend
            .builder
            .build_store(output, backend.value_type.const_zero())
            .map_err(builder_error)?;
        let stack_record_nodes = Self::allocate_stack_record_nodes(backend, source)?;
        let native_int_list_plan = NativeIntListPlan::analyze(backend.program, source);
        let native_int_lists = Self::allocate_native_int_lists(backend, &native_int_list_plan)?;
        let mut old_parameters = BTreeMap::new();
        if needs_parameter_snapshots(source) {
            let clone = backend
                .module
                .get_function("loom.runtime.clone")
                .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "clone helper is missing"))?;
            for parameter in &source.params {
                let snapshot = backend
                    .builder
                    .build_alloca(backend.value_type, &format!("old.{}", parameter.id.0))
                    .map_err(builder_error)?;
                backend
                    .builder
                    .build_call(
                        clone,
                        &[snapshot.into(), locals[&parameter.id].into()],
                        "snapshot",
                    )
                    .map_err(builder_error)?;
                old_parameters.insert(parameter.id, snapshot);
            }
        }
        let mut witness_parameters = BTreeMap::new();
        for index in 0..source.witness_params.len() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many witnesses"))?;
            let witness = backend.load_pointer_field(
                backend.witness_node_type,
                witness_argument,
                WITNESS_NODE_FIELD_VALUE,
                "witness.value",
            )?;
            witness_parameters.insert(index, witness);
            witness_argument = backend.load_pointer_field(
                backend.witness_node_type,
                witness_argument,
                WITNESS_NODE_FIELD_NEXT,
                "witness.next",
            )?;
        }
        Ok(Self {
            backend,
            source,
            function,
            output,
            native_signature: None,
            witness_parameters,
            runtime_context,
            task: None,
            loop_depth: Cell::new(0),
            active_range_local: Cell::new(None),
            resume_blocks: BTreeMap::new(),
            locals,
            stack_record_nodes,
            native_int_list_plan,
            native_int_lists,
            old_parameters,
            body_done,
            cleanups: RefCell::new(Vec::new()),
            cancellation_block: None,
            cancellation_cleanups: RefCell::new(BTreeMap::new()),
            unwind_status: Cell::new(None),
        })
    }

    #[allow(clippy::too_many_lines)]
    fn new_native(
        backend: &'backend Backend<'ctx, 'program>,
        id: FunctionId,
    ) -> Result<Self, CodegenError> {
        let source = backend.program.function(id).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("function #{} does not exist", id.0),
            )
        })?;
        if NativeSignatureShape::for_supported_function(source).is_none() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "unsupported function selected for the native ABI",
            ));
        }
        let declaration = &backend.native_functions[&id];
        let function = declaration.function;
        let signature = &declaration.signature;
        let runtime_context = if signature.effect() == NativeEffectAbi::RuntimeStatus {
            let context_index = u32::try_from(source.params.len())
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many integer parameters"))?;
            parameter_pointer(function, context_index)?
        } else {
            backend.ptr_type.const_null()
        };
        let entry = backend.context.append_basic_block(function, "entry");
        let body_done = backend.context.append_basic_block(function, "body.done");
        backend.builder.position_at_end(entry);

        let output = backend
            .builder
            .build_alloca(backend.value_type, "integer.output.value")
            .map_err(builder_error)?;
        backend
            .builder
            .build_store(output, backend.value_type.const_zero())
            .map_err(builder_error)?;

        let mut locals = BTreeMap::new();
        for (index, parameter) in source.params.iter().enumerate() {
            let pointer = backend
                .builder
                .build_alloca(
                    backend.value_type,
                    &format!("parameter.{}.{}", parameter.id.0, parameter.name),
                )
                .map_err(builder_error)?;
            backend
                .builder
                .build_store(pointer, backend.value_type.const_zero())
                .map_err(builder_error)?;
            backend.store_i64_field(
                backend.value_type,
                pointer,
                VALUE_FIELD_TAG,
                backend.tag(VALUE_TAG_INT),
            )?;
            let parameter_index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many integer parameters"))?;
            backend.store_i64_field(
                backend.value_type,
                pointer,
                VALUE_FIELD_SCALAR,
                parameter_int(function, parameter_index)?,
            )?;
            locals.insert(parameter.id, pointer);
        }
        for local in &source.locals {
            let pointer = backend
                .builder
                .build_alloca(
                    backend.value_type,
                    &format!("local.{}.{}", local.id.0, local.name),
                )
                .map_err(builder_error)?;
            backend
                .builder
                .build_store(pointer, backend.value_type.const_zero())
                .map_err(builder_error)?;
            locals.insert(local.id, pointer);
        }
        let stack_record_nodes = Self::allocate_stack_record_nodes(backend, source)?;
        let native_int_list_plan = NativeIntListPlan::analyze(backend.program, source);
        let native_int_lists = Self::allocate_native_int_lists(backend, &native_int_list_plan)?;

        let mut old_parameters = BTreeMap::new();
        if needs_parameter_snapshots(source) {
            let clone = backend
                .module
                .get_function("loom.runtime.clone")
                .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "clone helper is missing"))?;
            for parameter in &source.params {
                let snapshot = backend
                    .builder
                    .build_alloca(backend.value_type, &format!("old.{}", parameter.id.0))
                    .map_err(builder_error)?;
                backend
                    .builder
                    .build_call(
                        clone,
                        &[snapshot.into(), locals[&parameter.id].into()],
                        "snapshot",
                    )
                    .map_err(builder_error)?;
                old_parameters.insert(parameter.id, snapshot);
            }
        }

        Ok(Self {
            backend,
            source,
            function,
            output,
            native_signature: Some(signature),
            witness_parameters: BTreeMap::new(),
            runtime_context,
            task: None,
            loop_depth: Cell::new(0),
            active_range_local: Cell::new(None),
            resume_blocks: BTreeMap::new(),
            locals,
            stack_record_nodes,
            native_int_list_plan,
            native_int_lists,
            old_parameters,
            body_done,
            cleanups: RefCell::new(Vec::new()),
            cancellation_block: None,
            cancellation_cleanups: RefCell::new(BTreeMap::new()),
            unwind_status: Cell::new(None),
        })
    }

    fn allocate_stack_record_nodes(
        backend: &'backend Backend<'ctx, 'program>,
        source: &Function,
    ) -> Result<BTreeMap<LocalId, Vec<PointerValue<'ctx>>>, CodegenError> {
        let mut storage = BTreeMap::new();
        let candidates = stack_record_candidates(backend.program, source);
        for (local, field_count) in candidates {
            let mut nodes = Vec::with_capacity(field_count);
            for field in 0..field_count {
                let node = backend
                    .builder
                    .build_alloca(
                        backend.value_node_type,
                        &format!("record.local.{}.field.{field}", local.0),
                    )
                    .map_err(builder_error)?;
                backend
                    .builder
                    .build_store(node, backend.value_node_type.const_zero())
                    .map_err(builder_error)?;
                nodes.push(node);
            }
            for (index, node) in nodes.iter().copied().enumerate() {
                let next = nodes
                    .get(index + 1)
                    .copied()
                    .unwrap_or_else(|| backend.ptr_type.const_null());
                backend.store_pointer_field(
                    backend.value_node_type,
                    node,
                    VALUE_NODE_FIELD_NEXT,
                    next,
                )?;
            }
            storage.insert(local, nodes);
        }
        Ok(storage)
    }

    fn allocate_native_int_lists(
        backend: &'backend Backend<'ctx, 'program>,
        plan: &NativeIntListPlan,
    ) -> Result<BTreeMap<LocalId, PointerValue<'ctx>>, CodegenError> {
        plan.locals()
            .map(|local| {
                let storage = backend
                    .builder
                    .build_alloca(
                        backend.int_list_type,
                        &format!("int.list.local.{}", local.0),
                    )
                    .map_err(builder_error)?;
                backend
                    .builder
                    .build_store(storage, backend.int_list_type.const_zero())
                    .map_err(builder_error)?;
                Ok((local, storage))
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    fn new_async(
        backend: &'backend Backend<'ctx, 'program>,
        id: FunctionId,
    ) -> Result<Self, CodegenError> {
        let source = backend.program.function(id).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("function #{} does not exist", id.0),
            )
        })?;
        let layout = AsyncLayout::new(source)?;
        let function = backend.task_resumes[&id];
        let task = parameter_pointer(function, 0)?;
        let executor = parameter_pointer(function, 1)?;
        let entry = backend.context.append_basic_block(function, "entry");
        let dispatch = backend
            .context
            .append_basic_block(function, "state.dispatch");
        let start = backend.context.append_basic_block(function, "state.start");
        let cancelled = backend
            .context
            .append_basic_block(function, "state.cancelled");
        let invalid = backend
            .context
            .append_basic_block(function, "state.invalid");
        let body_done = backend.context.append_basic_block(function, "body.done");
        let mut resume_blocks = BTreeMap::new();
        for point in &source.suspension_points {
            resume_blocks.insert(
                point.state,
                backend
                    .context
                    .append_basic_block(function, &format!("state.resume.{}", point.state)),
            );
        }
        backend.builder.position_at_end(entry);
        let output = call_pointer(
            &backend.builder,
            backend.native_task_result(),
            &[task.into()],
            "task.result",
        )?;
        let mut locals = BTreeMap::new();
        for local in source.params.iter().chain(&source.locals) {
            let slot = call_pointer(
                &backend.builder,
                backend.native_task_slot(),
                &[
                    task.into(),
                    backend
                        .i64_type
                        .const_int(layout.local_slots[&local.id], false)
                        .into(),
                ],
                &format!("task.local.{}", local.id.0),
            )?;
            locals.insert(local.id, slot);
        }
        let mut old_parameters = BTreeMap::new();
        for parameter in &source.params {
            let slot = call_pointer(
                &backend.builder,
                backend.native_task_slot(),
                &[
                    task.into(),
                    backend
                        .i64_type
                        .const_int(layout.old_parameter_slots[&parameter.id], false)
                        .into(),
                ],
                &format!("task.old.{}", parameter.id.0),
            )?;
            old_parameters.insert(parameter.id, slot);
        }
        let mut witness_parameters = BTreeMap::new();
        for index in 0..source.witness_params.len() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many task witnesses"))?;
            let slot = call_pointer(
                &backend.builder,
                backend.native_task_slot(),
                &[
                    task.into(),
                    backend
                        .i64_type
                        .const_int(layout.witness_slots[&index], false)
                        .into(),
                ],
                "task.witness.slot",
            )?;
            let witness = backend.load_pointer_field(
                backend.value_type,
                slot,
                VALUE_FIELD_DATA,
                "task.witness",
            )?;
            witness_parameters.insert(index, witness);
        }
        let is_cancelled = call_int(
            &backend.builder,
            backend.native_task_is_cancelled(),
            &[task.into()],
            "task.cancelled",
        )?;
        let cancellation = backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                is_cancelled,
                backend.context.i32_type().const_zero(),
                "task.cancellation.requested",
            )
            .map_err(builder_error)?;
        backend
            .builder
            .build_conditional_branch(cancellation, cancelled, dispatch)
            .map_err(builder_error)?;

        backend.builder.position_at_end(dispatch);
        let state = call_int(
            &backend.builder,
            backend.native_task_state(),
            &[task.into()],
            "task.state",
        )?;
        let mut cases = Vec::with_capacity(resume_blocks.len() + 1);
        cases.push((backend.i64_type.const_zero(), start));
        for (state, block) in &resume_blocks {
            cases.push((backend.i64_type.const_int(u64::from(*state), false), *block));
        }
        backend
            .builder
            .build_switch(state, invalid, &cases)
            .map_err(builder_error)?;

        backend.builder.position_at_end(invalid);
        backend.set_task_fault(
            task,
            "LOOM_RUNTIME_INVALID_COROUTINE_STATE",
            "invalid coroutine state",
        )?;
        backend
            .builder
            .build_return(Some(
                &backend
                    .context
                    .i32_type()
                    .const_int(TASK_STEP_FAULTED, false),
            ))
            .map_err(builder_error)?;
        backend.builder.position_at_end(start);

        Ok(Self {
            backend,
            source,
            function,
            output,
            native_signature: None,
            witness_parameters,
            runtime_context: executor,
            task: Some(task),
            loop_depth: Cell::new(0),
            active_range_local: Cell::new(None),
            resume_blocks,
            locals,
            stack_record_nodes: BTreeMap::new(),
            native_int_list_plan: NativeIntListPlan::default(),
            native_int_lists: BTreeMap::new(),
            old_parameters,
            body_done,
            cleanups: RefCell::new(Vec::new()),
            cancellation_block: Some(cancelled),
            cancellation_cleanups: RefCell::new(BTreeMap::new()),
            unwind_status: Cell::new(None),
        })
    }

    fn compile(&self) -> Result<(), CodegenError> {
        self.emit_entry_contracts()?;
        let continues = self.emit_block(&self.source.body, self.output)?;
        if continues {
            self.backend.branch(self.body_done)?;
        }
        self.backend.builder.position_at_end(self.body_done);
        self.emit_exit_contracts()?;
        if !self.current_block_terminated() {
            let scalar = self
                .native_signature
                .is_some()
                .then(|| {
                    self.backend.load_i64_field(
                        self.backend.value_type,
                        self.output,
                        VALUE_FIELD_SCALAR,
                        "integer.result",
                    )
                })
                .transpose()?;
            self.emit_status_return(self.backend.context.i32_type().const_zero(), scalar)?;
        }
        self.emit_cancellation_dispatch()?;
        Ok(())
    }

    fn emit_status_return(
        &self,
        status: IntValue<'ctx>,
        scalar: Option<IntValue<'ctx>>,
    ) -> Result<(), CodegenError> {
        // Source defers and exit contracts have already run at every caller.
        // Native list storage is compiler-owned and therefore released last,
        // after the result/status has been fully materialized.
        for storage in self.native_int_lists.values().copied() {
            self.backend
                .builder
                .build_call(
                    self.backend.native_int_list_drop(),
                    &[storage.into()],
                    "int.list.drop",
                )
                .map_err(builder_error)?;
        }
        match self.native_signature {
            Some(signature) => match signature.effect() {
                NativeEffectAbi::PureNoFault => {
                    self.backend
                        .builder
                        .build_return(Some(&scalar.ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                "pure native return is missing its value",
                            )
                        })?))
                        .map_err(builder_error)?;
                }
                NativeEffectAbi::RuntimeStatus => {
                    let aggregate = self
                        .backend
                        .native_status_result_type(signature)?
                        .get_undef();
                    let aggregate = self
                        .backend
                        .builder
                        .build_insert_value(aggregate, status, 0, "integer.status")
                        .map_err(builder_error)?
                        .into_struct_value();
                    let aggregate = self
                        .backend
                        .builder
                        .build_insert_value(
                            aggregate,
                            scalar.unwrap_or_else(|| self.backend.i64_type.const_zero()),
                            1,
                            "integer.value",
                        )
                        .map_err(builder_error)?
                        .into_struct_value();
                    self.backend
                        .builder
                        .build_return(Some(&aggregate))
                        .map_err(builder_error)?;
                }
            },
            None => {
                self.backend
                    .builder
                    .build_return(Some(&status))
                    .map_err(builder_error)?;
            }
        }
        Ok(())
    }

    fn emit_cancellation_dispatch(&self) -> Result<(), CodegenError> {
        let Some(cancelled) = self.cancellation_block else {
            return Ok(());
        };
        let task = self
            .task
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "async cancellation has no task"))?;
        let snapshots = self.cancellation_cleanups.borrow().clone();
        let done = self.append_block("cancel.done");
        let cleanup_blocks = snapshots
            .keys()
            .map(|state| {
                (
                    *state,
                    self.backend
                        .context
                        .append_basic_block(self.function, &format!("cancel.state.{state}")),
                )
            })
            .collect::<BTreeMap<_, _>>();

        self.backend.builder.position_at_end(cancelled);
        let state = call_int(
            &self.backend.builder,
            self.backend.native_task_state(),
            &[task.into()],
            "cancel.state",
        )?;
        let cases = cleanup_blocks
            .iter()
            .map(|(state, block)| {
                (
                    self.backend.i64_type.const_int(u64::from(*state), false),
                    *block,
                )
            })
            .collect::<Vec<_>>();
        self.backend
            .builder
            .build_switch(state, done, &cases)
            .map_err(builder_error)?;

        for (state, block) in cleanup_blocks {
            self.backend.builder.position_at_end(block);
            let previous = self.unwind_status.replace(Some(TASK_STEP_CANCELLED));
            let cleanup_result = self.emit_cleanup_sequence(&snapshots[&state]);
            self.unwind_status.set(previous);
            cleanup_result?;
            if !self.current_block_terminated() {
                self.backend.branch(done)?;
            }
        }
        self.backend.builder.position_at_end(done);
        self.backend
            .builder
            .build_return(Some(
                &self
                    .backend
                    .context
                    .i32_type()
                    .const_int(TASK_STEP_CANCELLED, false),
            ))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_entry_contracts(&self) -> Result<(), CodegenError> {
        if let Some(contract) = &self.source.call_plan.receiver_invariant {
            self.emit_contract_check(contract, "InvariantFault", None)?;
        }
        for contract in &self.source.call_plan.requires {
            self.emit_contract_check(contract, "PreconditionFault", None)?;
        }
        Ok(())
    }

    fn emit_exit_contracts(&self) -> Result<(), CodegenError> {
        if let Some(contract) = &self.source.call_plan.receiver_invariant {
            self.emit_contract_check(contract, "InvariantFault", Some(self.output))?;
        }
        for contract in &self.source.call_plan.ensures {
            self.emit_contract_check(contract, "PostconditionFault", Some(self.output))?;
        }
        Ok(())
    }

    fn emit_contract_check(
        &self,
        contract: &Contract,
        category: &str,
        result: Option<PointerValue<'ctx>>,
    ) -> Result<(), CodegenError> {
        let condition = self.alloc_value("contract");
        let context = self.contract_context(result)?;
        if !self.emit_contract_expr(&contract.expression, &context, condition)? {
            return Ok(());
        }
        let accepted = self.bool_value(condition)?;
        let pass = self.append_block("contract.pass");
        let fail = self.append_block("contract.fail");
        self.backend
            .builder
            .build_conditional_branch(accepted, pass, fail)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(fail);
        let message = format!("contract `{}` was not satisfied", contract.code);
        let category_name = match category {
            "PreconditionFault" => "precondition",
            "PostconditionFault" => "postcondition",
            "InvariantFault" => "invariant",
            _ => "assertion",
        };
        let blame_span = if category == "PreconditionFault" {
            self.source.span
        } else {
            contract.span
        };
        let detail = serde_json::to_string(&serde_json::json!({
            "channel": "contract",
            "fault": {
                "code": category,
                "category": category_name,
                "message": message,
                "contractSpan": contract.span,
                "blameSpan": blame_span,
            },
        }))
        .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
        self.record_or_print_fault_with_detail(
            category,
            &message,
            &format!("{category}: {}", contract.code),
            &detail,
        )?;
        self.emit_all_cleanups()?;
        self.emit_status_return(self.failure_status(), None)?;
        self.backend.builder.position_at_end(pass);
        Ok(())
    }

    fn contract_context(
        &self,
        result: Option<PointerValue<'ctx>>,
    ) -> Result<ContractContext<'ctx>, CodegenError> {
        let parameters = self
            .source
            .params
            .iter()
            .map(|parameter| {
                self.locals
                    .get(&parameter.id)
                    .copied()
                    .map(|pointer| TypedPointer {
                        pointer,
                        ty: parameter.ty.clone(),
                    })
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("parameter local #{} is missing", parameter.id.0),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (receiver, arguments) = if self.source.receiver.is_some() {
            let (receiver, arguments) = parameters.split_first().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "receiver function has no receiver")
            })?;
            (Some(receiver.clone()), arguments.to_vec())
        } else {
            (None, parameters)
        };
        let old_parameters = self
            .source
            .params
            .iter()
            .map(|parameter| TypedPointer {
                pointer: self.old_parameters[&parameter.id],
                ty: parameter.ty.clone(),
            })
            .collect::<Vec<_>>();
        let (old_receiver, old_arguments) = if self.source.receiver.is_some() {
            let (receiver, arguments) = old_parameters.split_first().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "receiver snapshot is missing")
            })?;
            (
                Some(receiver.clone()),
                arguments.iter().cloned().map(Some).collect(),
            )
        } else {
            (None, old_parameters.into_iter().map(Some).collect())
        };
        Ok(ContractContext {
            receiver,
            result: result.map(|pointer| TypedPointer {
                pointer,
                ty: self.source.return_ty.clone(),
            }),
            arguments,
            old_receiver,
            old_arguments,
            bindings: Vec::new(),
        })
    }

    fn emit_block(
        &self,
        block: &Block,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        self.backend
            .set_debug_location(self.function, block.span.file.0, block.span.range.start);
        let cleanup_base = self.cleanups.borrow().len();
        for statement in &block.statements {
            if let StatementKind::Defer(cleanup) = &statement.kind {
                self.cleanups.borrow_mut().push(cleanup.clone());
                continue;
            }
            if !self.emit_statement(statement)? {
                self.cleanups.borrow_mut().truncate(cleanup_base);
                return Ok(false);
            }
        }
        let continues = if let Some(tail) = &block.tail {
            self.emit_expr(tail, destination)
        } else {
            self.emit_constant(&Constant::Unit, destination)?;
            Ok(true)
        }?;
        if continues {
            self.emit_cleanups_from(cleanup_base)?;
        }
        self.cleanups.borrow_mut().truncate(cleanup_base);
        Ok(continues)
    }

    fn emit_cleanups_from(&self, base: usize) -> Result<(), CodegenError> {
        let cleanups = self.cleanups.borrow()[base..].to_vec();
        self.emit_cleanup_sequence(&cleanups)
    }

    fn emit_cleanup_sequence(&self, cleanups: &[Block]) -> Result<(), CodegenError> {
        let saved = self.cleanups.replace(cleanups.to_vec());
        for (index, cleanup) in cleanups.iter().enumerate().rev() {
            self.cleanups.replace(cleanups[..index].to_vec());
            let ignored = self.alloc_value("cleanup");
            let result = self.emit_block(cleanup, ignored);
            if let Err(error) = result {
                self.cleanups.replace(saved);
                return Err(error);
            }
            if !result.expect("checked above") {
                self.cleanups.replace(saved);
                return Err(CodegenError::new(
                    "InvalidCleanupControlFlow",
                    "checked cleanup unexpectedly terminated its enclosing function",
                ));
            }
        }
        self.cleanups.replace(saved);
        Ok(())
    }

    fn emit_all_cleanups(&self) -> Result<(), CodegenError> {
        self.emit_cleanups_from(0)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_statement(&self, statement: &Statement) -> Result<bool, CodegenError> {
        self.backend.set_debug_location(
            self.function,
            statement.span.file.0,
            statement.span.range.start,
        );
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let destination = self.local(*local)?;
                if self.native_int_lists.contains_key(local) {
                    if !matches!(&value.kind, ExprKind::List(elements) if elements.is_empty()) {
                        return Err(CodegenError::new(
                            "LlvmAbiDefect",
                            "native Int list candidate does not have an empty initializer",
                        ));
                    }
                    Ok(true)
                } else if let Some(nodes) = self.stack_record_nodes.get(local) {
                    self.emit_stack_record_initializer(value, destination, nodes)
                } else {
                    self.emit_expr(value, destination)
                }
            }
            StatementKind::LetTuple { locals, value } => {
                let tuple = self.alloc_value("tuple.binding");
                if !self.emit_expr(value, tuple)? {
                    return Ok(false);
                }
                let data = self.backend.load_pointer_field(
                    self.backend.value_type,
                    tuple,
                    VALUE_FIELD_DATA,
                    "tuple.data",
                )?;
                for (index, local) in locals.iter().enumerate() {
                    let index = u32::try_from(index).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "tuple binding index exceeds u32")
                    })?;
                    let node = self.value_node_at(data, index)?;
                    let element = self.backend.struct_pointer(
                        self.backend.value_node_type,
                        node,
                        VALUE_NODE_FIELD_VALUE,
                        "tuple.element",
                    )?;
                    self.clone_value(self.local(*local)?, element)?;
                }
                Ok(true)
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => self.emit_for_range(*local, start, end, body),
            StatementKind::Assign { place, value } => {
                let temporary = self.alloc_value("assign");
                if !self.emit_expr(value, temporary)? {
                    return Ok(false);
                }
                let destination = self.place(place)?;
                if !place.projection.is_empty() && self.static_place_type(place) == Some(Type::Int)
                {
                    let scalar = self.backend.load_i64_field(
                        self.backend.value_type,
                        temporary,
                        VALUE_FIELD_SCALAR,
                        "assign.scalar",
                    )?;
                    self.backend.store_i64_field(
                        self.backend.value_type,
                        destination,
                        VALUE_FIELD_SCALAR,
                        scalar,
                    )?;
                } else {
                    self.shallow_copy(destination, temporary)?;
                }
                Ok(true)
            }
            StatementKind::Assert { condition } => {
                let temporary = self.alloc_value("assert");
                if !self.emit_expr(condition, temporary)? {
                    return Ok(false);
                }
                let accepted = self.bool_value(temporary)?;
                let pass = self.append_block("assert.pass");
                let fail = self.append_block("assert.fail");
                self.backend
                    .builder
                    .build_conditional_branch(accepted, pass, fail)
                    .map_err(builder_error)?;
                self.backend.builder.position_at_end(fail);
                let detail = serde_json::to_string(&serde_json::json!({
                    "channel": "contract",
                    "fault": {
                        "code": "AssertionFault",
                        "category": "assertion",
                        "message": "assertion was not satisfied",
                        "contractSpan": statement.span,
                        "blameSpan": statement.span,
                    },
                }))
                .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
                self.record_or_print_fault_with_detail(
                    "AssertionFault",
                    "assertion was not satisfied",
                    "AssertionFault",
                    &detail,
                )?;
                self.emit_all_cleanups()?;
                self.emit_status_return(self.failure_status(), None)?;
                self.backend.builder.position_at_end(pass);
                Ok(true)
            }
            StatementKind::Evaluate(value) => {
                let temporary = self.alloc_value("evaluate");
                self.emit_expr(value, temporary)
            }
            StatementKind::Defer(_) => Err(CodegenError::new(
                "LlvmAbiDefect",
                "defer registration escaped lexical block emission",
            )),
            StatementKind::Return(value) => {
                let continues = if let Some(value) = value {
                    self.emit_expr(value, self.output)?
                } else {
                    self.emit_constant(&Constant::Unit, self.output)?;
                    true
                };
                if continues {
                    self.emit_all_cleanups()?;
                    self.backend.branch(self.body_done)?;
                }
                Ok(false)
            }
        }
    }

    fn emit_for_range(
        &self,
        local: LocalId,
        start: &Expr,
        end: &Expr,
        body: &Block,
    ) -> Result<bool, CodegenError> {
        let start_value = self.alloc_value("range.start");
        if !self.emit_expr(start, start_value)? {
            return Ok(false);
        }
        let end_value = self.alloc_value("range.end");
        if !self.emit_expr(end, end_value)? {
            return Ok(false);
        }
        let current = self.local(local)?;
        self.shallow_copy(current, start_value)?;

        if let Some(append) = self.native_int_list_plan.direct_append_loop(body) {
            // The proof also requires the block tail to be the literal Unit,
            // so this specialized path may omit materializing that otherwise
            // effect-free value after the append.
            return self.emit_native_int_list_append_range(local, current, end_value, append);
        }

        let header = self.append_block("range.header");
        let iteration = self.append_block("range.iteration");
        let exit = self.append_block("range.exit");
        self.backend.branch(header)?;

        self.backend.builder.position_at_end(header);
        let current_scalar = self.int_scalar(current)?;
        let end_scalar = self.int_scalar(end_value)?;
        let has_next = self
            .backend
            .builder
            .build_int_compare(IntPredicate::SLT, current_scalar, end_scalar, "range.more")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(has_next, iteration, exit)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(iteration);
        let outer_loop_depth = self.loop_depth.get();
        let iteration_loop_depth = outer_loop_depth.checked_add(1).ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "loop nesting exceeds the compiler limit")
        })?;
        self.loop_depth.set(iteration_loop_depth);
        let outer_range_local = self.active_range_local.replace(Some(local));
        let ignored = self.alloc_value("range.body");
        let body_result = self.emit_block(body, ignored);
        self.active_range_local.set(outer_range_local);
        self.loop_depth.set(outer_loop_depth);
        if body_result? {
            let current_scalar = self.int_scalar(current)?;
            let next = self
                .backend
                .builder
                .build_int_add(
                    current_scalar,
                    self.backend.i64_type.const_int(1, false),
                    "range.next",
                )
                .map_err(builder_error)?;
            self.backend.store_i64_field(
                self.backend.value_type,
                current,
                VALUE_FIELD_SCALAR,
                next,
            )?;
            self.backend.branch(header)?;
        }
        self.backend.builder.position_at_end(exit);
        Ok(true)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_native_int_list_append_range(
        &self,
        range_local: LocalId,
        current: PointerValue<'ctx>,
        end_value: PointerValue<'ctx>,
        append: NativeIntListAppendLoop<'_>,
    ) -> Result<bool, CodegenError> {
        let storage = self
            .native_int_lists
            .get(&append.local)
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "proved native Int list append has no private storage",
                )
            })?;

        // Bounds have already been evaluated. Capture the authoritative
        // header only now, so effects in either bound cannot make the cached
        // state stale before the loop starts.
        let initial_data = self.backend.load_pointer_field(
            self.backend.int_list_type,
            storage,
            INT_LIST_FIELD_DATA,
            "int.list.loop.initial.data",
        )?;
        let initial_length = self.backend.load_i64_field(
            self.backend.int_list_type,
            storage,
            INT_LIST_FIELD_LENGTH,
            "int.list.loop.initial.length",
        )?;
        let initial_capacity = self.backend.load_i64_field(
            self.backend.int_list_type,
            storage,
            INT_LIST_FIELD_CAPACITY,
            "int.list.loop.initial.capacity",
        )?;
        let preheader = self
            .backend
            .builder
            .get_insert_block()
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "range has no preheader"))?;

        let header = self.append_block("range.header");
        let iteration = self.append_block("range.iteration");
        let exit = self.append_block("range.exit");
        self.backend.branch(header)?;

        self.backend.builder.position_at_end(header);
        let data_phi = self
            .backend
            .builder
            .build_phi(self.backend.ptr_type, "int.list.loop.data")
            .map_err(builder_error)?;
        let length_phi = self
            .backend
            .builder
            .build_phi(self.backend.i64_type, "int.list.loop.length")
            .map_err(builder_error)?;
        let capacity_phi = self
            .backend
            .builder
            .build_phi(self.backend.i64_type, "int.list.loop.capacity")
            .map_err(builder_error)?;
        data_phi.add_incoming(&[(&initial_data, preheader)]);
        length_phi.add_incoming(&[(&initial_length, preheader)]);
        capacity_phi.add_incoming(&[(&initial_capacity, preheader)]);
        let data = data_phi.as_basic_value().into_pointer_value();
        let length = length_phi.as_basic_value().into_int_value();
        let capacity = capacity_phi.as_basic_value().into_int_value();

        let current_scalar = self.int_scalar(current)?;
        let end_scalar = self.int_scalar(end_value)?;
        let has_next = self
            .backend
            .builder
            .build_int_compare(IntPredicate::SLT, current_scalar, end_scalar, "range.more")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(has_next, iteration, exit)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(iteration);
        let outer_loop_depth = self.loop_depth.get();
        let iteration_loop_depth = outer_loop_depth.checked_add(1).ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "loop nesting exceeds the compiler limit")
        })?;
        self.loop_depth.set(iteration_loop_depth);
        let outer_range_local = self.active_range_local.replace(Some(range_local));

        let append_result: Result<
            Option<(
                PointerValue<'ctx>,
                IntValue<'ctx>,
                IntValue<'ctx>,
                BasicBlock<'ctx>,
            )>,
            CodegenError,
        > = (|| {
            // A direct local receiver has no evaluation effects. Evaluate the
            // element before checking/growing capacity, matching List.add's
            // established source order while the memory header is coherent.
            let element = self.alloc_value("int.list.add.value");
            if !self.emit_expr(append.value, element)? {
                return Ok(None);
            }
            let scalar = self.int_scalar(element)?;
            let full = self
                .backend
                .builder
                .build_int_compare(IntPredicate::EQ, length, capacity, "int.list.add.full")
                .map_err(builder_error)?;
            let grow = self.append_block("int.list.add.grow");
            let ready = self.append_block("int.list.add.ready");
            self.backend
                .builder
                .build_conditional_branch(full, grow, ready)
                .map_err(builder_error)?;
            let no_grow = self.backend.builder.get_insert_block().ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "append has no capacity-test block")
            })?;

            self.backend.builder.position_at_end(grow);
            let minimum = self
                .backend
                .builder
                .build_int_add(
                    length,
                    self.backend.i64_type.const_int(1, false),
                    "int.list.add.minimum",
                )
                .map_err(builder_error)?;
            let status = call_int(
                &self.backend.builder,
                self.backend.native_int_list_reserve(),
                &[storage.into(), minimum.into()],
                "int.list.reserve",
            )?;
            let invalid = self
                .backend
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    status,
                    self.backend.context.i32_type().const_zero(),
                    "int.list.reserve.invalid",
                )
                .map_err(builder_error)?;
            self.fail_if(invalid, "ListRuntimeFault")?;

            // reserve preserves length. It is the only operation allowed to
            // replace the allocation, so only data/capacity are reloaded on
            // its successful edge before those values rejoin loop SSA.
            let grown_data = self.backend.load_pointer_field(
                self.backend.int_list_type,
                storage,
                INT_LIST_FIELD_DATA,
                "int.list.loop.grown.data",
            )?;
            let grown_capacity = self.backend.load_i64_field(
                self.backend.int_list_type,
                storage,
                INT_LIST_FIELD_CAPACITY,
                "int.list.loop.grown.capacity",
            )?;
            let reserve_success_block =
                self.backend.builder.get_insert_block().ok_or_else(|| {
                    CodegenError::new("LlvmBuilderFailed", "append growth has no success block")
                })?;
            self.backend.branch(ready)?;

            self.backend.builder.position_at_end(ready);
            let ready_data_phi = self
                .backend
                .builder
                .build_phi(self.backend.ptr_type, "int.list.add.ready.data")
                .map_err(builder_error)?;
            let ready_capacity_phi = self
                .backend
                .builder
                .build_phi(self.backend.i64_type, "int.list.add.ready.capacity")
                .map_err(builder_error)?;
            ready_data_phi.add_incoming(&[(&data, no_grow), (&grown_data, reserve_success_block)]);
            ready_capacity_phi.add_incoming(&[
                (&capacity, no_grow),
                (&grown_capacity, reserve_success_block),
            ]);
            let ready_data = ready_data_phi.as_basic_value().into_pointer_value();
            let ready_capacity = ready_capacity_phi.as_basic_value().into_int_value();

            let slot =
                self.native_int_list_element_pointer(ready_data, length, "int.list.add.slot")?;
            self.backend
                .builder
                .build_store(slot, scalar)
                .map_err(builder_error)?;
            let next_length = self
                .backend
                .builder
                .build_int_add(
                    length,
                    self.backend.i64_type.const_int(1, false),
                    "int.list.add.next.length",
                )
                .map_err(builder_error)?;
            // Commit immediately after the infallible Int slot store. Every
            // later fault/return/drop edge can therefore trust the memory
            // header even though the hot path carries its values in SSA.
            self.backend.store_i64_field(
                self.backend.int_list_type,
                storage,
                INT_LIST_FIELD_LENGTH,
                next_length,
            )?;

            let current_scalar = self.int_scalar(current)?;
            let next = self
                .backend
                .builder
                .build_int_add(
                    current_scalar,
                    self.backend.i64_type.const_int(1, false),
                    "range.next",
                )
                .map_err(builder_error)?;
            self.backend.store_i64_field(
                self.backend.value_type,
                current,
                VALUE_FIELD_SCALAR,
                next,
            )?;
            let backedge = self.backend.builder.get_insert_block().ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "append range has no backedge")
            })?;
            self.backend.branch(header)?;
            Ok(Some((ready_data, next_length, ready_capacity, backedge)))
        })();

        self.active_range_local.set(outer_range_local);
        self.loop_depth.set(outer_loop_depth);
        if let Some((next_data, next_length, next_capacity, backedge)) = append_result? {
            data_phi.add_incoming(&[(&next_data, backedge)]);
            length_phi.add_incoming(&[(&next_length, backedge)]);
            capacity_phi.add_incoming(&[(&next_capacity, backedge)]);
        }

        self.backend.builder.position_at_end(exit);
        Ok(true)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_expr(
        &self,
        expression: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        self.backend.set_debug_location(
            self.function,
            expression.span.file.0,
            expression.span.range.start,
        );
        match &expression.kind {
            ExprKind::Constant(value) => {
                self.emit_constant(value, destination)?;
                Ok(true)
            }
            ExprKind::Tuple(elements) => self.emit_tuple(elements, destination),
            ExprKind::List(elements) => self.emit_list(elements, destination),
            ExprKind::Copy(place) => {
                if place.projection.is_empty() && self.stack_record_nodes.contains_key(&place.local)
                {
                    self.emit_stack_record_copy(place.local, destination)?;
                    return Ok(true);
                }
                let source = self.place(place)?;
                if expression.ty == Type::Int {
                    let scalar = self.backend.load_i64_field(
                        self.backend.value_type,
                        source,
                        VALUE_FIELD_SCALAR,
                        "copy.scalar",
                    )?;
                    self.initialize(destination, VALUE_TAG_INT)?;
                    self.backend.store_i64_field(
                        self.backend.value_type,
                        destination,
                        VALUE_FIELD_SCALAR,
                        scalar,
                    )?;
                } else {
                    self.clone_value(destination, source)?;
                }
                Ok(true)
            }
            ExprKind::Move(place) => {
                let source = self.place(place)?;
                self.shallow_copy(destination, source)?;
                Ok(true)
            }
            ExprKind::Unary(operator, value) => self.emit_unary(*operator, value, destination),
            ExprKind::Binary(operator, left, right) => {
                self.emit_binary(*operator, left, right, destination)
            }
            ExprKind::Block(block) => self.emit_block(block, destination),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.emit_if(condition, then_branch, else_branch, destination),
            ExprKind::Match { scrutinee, arms } => {
                if let Some(matched) = self.native_int_list_plan.direct_get_match(
                    self.active_range_local.get(),
                    scrutinee,
                    arms,
                ) {
                    self.emit_native_int_list_get_match(matched, destination)
                } else {
                    self.emit_match(scrutinee, arms, destination)
                }
            }
            ExprKind::Record {
                ty,
                fields,
                construction,
                ..
            } => self.emit_record(*ty, fields, *construction, destination),
            ExprKind::Variant {
                ty,
                variant,
                payload,
                ..
            } => self.emit_variant(*ty, variant.0, payload, destination),
            ExprKind::Refine {
                ty,
                value,
                construction,
            } => self.emit_refine(*ty, value, *construction, destination),
            ExprKind::Unrefine(value) => {
                let refined = self.alloc_value("refined");
                if !self.emit_expr(value, refined)? {
                    return Ok(false);
                }
                let inner = self.backend.load_pointer_field(
                    self.backend.value_type,
                    refined,
                    VALUE_FIELD_DATA,
                    "refined.inner",
                )?;
                self.clone_value(destination, inner)?;
                Ok(true)
            }
            ExprKind::Call {
                target,
                arguments,
                witnesses,
                ..
            } => self.emit_call(target, arguments, witnesses, destination),
            ExprKind::MakeView {
                value,
                writeback,
                witness,
                mutable,
                ..
            } => {
                let source = self.alloc_value("dyn.source");
                if !self.emit_expr(value, source)? {
                    return Ok(false);
                }
                let data = call_pointer(
                    &self.backend.builder,
                    self.backend.native_gc_alloc_value(),
                    &[],
                    "dyn.data",
                )?;
                self.clone_value(data, source)?;
                let witness = self.resolve_witness(witness)?;
                let writeback = writeback
                    .as_ref()
                    .map(|writeback| self.place(writeback))
                    .transpose()?;
                self.initialize(destination, VALUE_TAG_DYN)?;
                let mut flags = u64::from(*mutable) * DYN_FLAG_MUTABLE;
                if writeback.is_some() {
                    flags |= DYN_FLAG_WRITEBACK;
                }
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_AUX,
                    self.backend.tag(flags),
                )?;
                self.backend.store_pointer_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_DATA,
                    data,
                )?;
                self.backend.store_pointer_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_WITNESS,
                    witness,
                )?;
                let writeback = if let Some(writeback) = writeback {
                    self.backend
                        .builder
                        .build_ptr_to_int(writeback, self.backend.i64_type, "dyn.writeback")
                        .map_err(builder_error)?
                } else {
                    self.backend.i64_type.const_zero()
                };
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    writeback,
                )?;
                Ok(true)
            }
            ExprKind::ReborrowView { owner, mutable, .. } => {
                let source = self.place(owner)?;
                let data = self.backend.load_pointer_field(
                    self.backend.value_type,
                    source,
                    VALUE_FIELD_DATA,
                    "dyn.reborrow.data",
                )?;
                let witness = self.backend.load_pointer_field(
                    self.backend.value_type,
                    source,
                    VALUE_FIELD_WITNESS,
                    "dyn.reborrow.witness",
                )?;
                let writeback = self.backend.load_i64_field(
                    self.backend.value_type,
                    source,
                    VALUE_FIELD_SCALAR,
                    "dyn.reborrow.writeback",
                )?;
                let source_flags = self.backend.load_i64_field(
                    self.backend.value_type,
                    source,
                    VALUE_FIELD_AUX,
                    "dyn.reborrow.flags",
                )?;
                let writeback_flag = self
                    .backend
                    .builder
                    .build_and(
                        source_flags,
                        self.backend.tag(DYN_FLAG_WRITEBACK),
                        "dyn.reborrow.writeback.flag",
                    )
                    .map_err(builder_error)?;
                let mutable_flag = self.backend.tag(u64::from(*mutable) * DYN_FLAG_MUTABLE);
                let flags = self
                    .backend
                    .builder
                    .build_or(mutable_flag, writeback_flag, "dyn.reborrow.flags")
                    .map_err(builder_error)?;
                self.initialize(destination, VALUE_TAG_DYN)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_AUX,
                    flags,
                )?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    writeback,
                )?;
                self.backend.store_pointer_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_DATA,
                    data,
                )?;
                self.backend.store_pointer_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_WITNESS,
                    witness,
                )?;
                Ok(true)
            }
            ExprKind::Await { state, task } => {
                if self.task.is_some() {
                    return self.emit_resumable_await(*state, task, destination);
                }
                if let ExprKind::Sleep { milliseconds } = &task.kind {
                    return self.emit_sleep_await(*state, milliseconds, destination);
                }
                if let ExprKind::Tuple(tasks) = &task.kind
                    && tasks
                        .iter()
                        .all(|task| matches!(&task.kind, ExprKind::Sleep { .. }))
                {
                    return self.emit_sleep_join(*state, tasks, destination);
                }
                // Child bodies in the current linear slice are still eligible
                // for Task elision, but completion follows the same ABI as an
                // external wait: the compiler frame is registered, notified,
                // and recovered from the runtime ready queue before execution
                // continues at the MIR suspension state.
                let completion = self.begin_completion_wait(*state, destination)?;
                if !self.emit_expr(task, destination)? {
                    return Ok(false);
                }
                self.finish_completion_wait(completion)?;
                Ok(true)
            }
            ExprKind::Sleep { milliseconds } => self.emit_wait_task(milliseconds, destination),
            ExprKind::WaitFd {
                descriptor,
                writable,
            } => self.emit_fd_wait_task(descriptor, *writable, destination),
            ExprKind::TaskJoin { mode, arguments } => {
                self.emit_task_join(*mode, arguments, &expression.ty, destination)
            }
        }
    }

    fn emit_resumable_await(
        &self,
        state: u32,
        task_expression: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let task = self.task.ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "resumable await has no task frame")
        })?;
        let awaited = self.alloc_value("task.awaited");
        if !self.emit_expr(task_expression, awaited)? {
            return Ok(false);
        }
        self.cancellation_cleanups
            .borrow_mut()
            .insert(state, self.cleanups.borrow().clone());
        let suspend = call_int(
            &self.backend.builder,
            self.backend.native_task_suspend_value(),
            &[self.runtime_context.into(), task.into(), awaited.into()],
            "task.value.suspend",
        )?;
        let invalid = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                suspend,
                self.backend.context.i32_type().const_zero(),
                "task.value.invalid",
            )
            .map_err(builder_error)?;
        self.fail_if(invalid, "TaskJoinFault")?;
        let pending = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                suspend,
                self.backend.context.i32_type().const_int(1, false),
                "task.value.pending",
            )
            .map_err(builder_error)?;
        let suspend_block = self.append_block("task.value.return.pending");
        let resume_block = self.resume_blocks.get(&state).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("coroutine resume state {state} has no block"),
            )
        })?;
        self.backend
            .builder
            .build_conditional_branch(pending, suspend_block, resume_block)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(suspend_block);
        self.set_resume_state_and_suspend(task, state)?;
        self.backend.builder.position_at_end(resume_block);
        let step = call_int(
            &self.backend.builder,
            self.backend.native_task_join_step(),
            &[task.into()],
            "task.join.step",
        )?;
        let completed = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                step,
                self.backend
                    .context
                    .i32_type()
                    .const_int(TASK_STEP_COMPLETED, false),
                "task.join.completed",
            )
            .map_err(builder_error)?;
        let ready = self.append_block("task.join.ready");
        let failed = self.append_block("task.join.failed");
        self.backend
            .builder
            .build_conditional_branch(completed, ready, failed)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(failed);
        self.emit_all_cleanups()?;
        self.backend
            .builder
            .build_return(Some(&step))
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(ready);
        let write = call_int(
            &self.backend.builder,
            self.backend.native_task_write_join_result(),
            &[
                task.into(),
                destination.into(),
                self.backend
                    .context
                    .i32_type()
                    .const_int(JOIN_RESULT_SCALAR, false)
                    .into(),
            ],
            "task.join.write.result",
        )?;
        self.propagate_runtime_status(write, self.runtime_context, "task.join.write.result")?;
        Ok(true)
    }

    fn emit_wait_task(
        &self,
        milliseconds: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let duration_value = self.alloc_value("sleep.task.duration");
        if !self.emit_expr(milliseconds, duration_value)? {
            return Ok(false);
        }
        let milliseconds = self.duration_scalar(milliseconds, duration_value)?;
        let negative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                milliseconds,
                self.backend.i64_type.const_zero(),
                "sleep.task.duration.negative",
            )
            .map_err(builder_error)?;
        self.fail_if(negative, "InvalidSleepDuration")?;
        let nanoseconds = self.checked_timer_multiply(milliseconds)?;
        let now = call_int(
            &self.backend.builder,
            self.backend.native_wait_now_ns(),
            &[],
            "sleep.task.now",
        )?;
        let deadline = self.checked_timer_add(now, nanoseconds)?;
        let source = self.alloc_timer_wait_source(deadline)?;
        let task = call_pointer(
            &self.backend.builder,
            self.backend.native_task_from_wait_source(),
            &[self.runtime_context.into(), source.into()],
            "sleep.task",
        )?;
        let missing = self
            .backend
            .builder
            .build_is_null(task, "sleep.task.missing")
            .map_err(builder_error)?;
        self.fail_if(missing, "TaskAllocationFault")?;
        self.initialize(destination, VALUE_TAG_TASK)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(TASK_VALUE_DIRECT),
        )?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            task,
        )?;
        Ok(true)
    }

    fn emit_fd_wait_task(
        &self,
        descriptor: &Expr,
        writable: bool,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let descriptor_value = self.alloc_value("fd.task.descriptor");
        if !self.emit_expr(descriptor, descriptor_value)? {
            return Ok(false);
        }
        let descriptor = self.int_scalar(descriptor_value)?;
        let negative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                descriptor,
                self.backend.i64_type.const_zero(),
                "fd.task.descriptor.negative",
            )
            .map_err(builder_error)?;
        self.fail_if(negative, "InvalidFileDescriptor")?;
        let too_large = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SGT,
                descriptor,
                self.backend.i64_type.const_int(i32::MAX as u64, false),
                "fd.task.descriptor.too_large",
            )
            .map_err(builder_error)?;
        self.fail_if(too_large, "InvalidFileDescriptor")?;
        let interests = if writable {
            WAIT_INTEREST_WRITABLE
        } else {
            WAIT_INTEREST_READABLE
        };
        let source = self.alloc_fd_wait_source(descriptor, interests)?;
        let task = call_pointer(
            &self.backend.builder,
            self.backend.native_task_from_wait_source(),
            &[self.runtime_context.into(), source.into()],
            "fd.task",
        )?;
        let missing = self
            .backend
            .builder
            .build_is_null(task, "fd.task.missing")
            .map_err(builder_error)?;
        self.fail_if(missing, "TaskAllocationFault")?;
        self.initialize(destination, VALUE_TAG_TASK)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(TASK_VALUE_DIRECT),
        )?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            task,
        )?;
        Ok(true)
    }

    fn emit_task_join(
        &self,
        mode: TaskJoinMode,
        arguments: &[Expr],
        result_ty: &Type,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let shape = join_result_shape(mode, result_ty);
        let mode = match mode {
            TaskJoinMode::All => 0,
            TaskJoinMode::Settled => 1,
            TaskJoinMode::Any => 2,
            TaskJoinMode::Race => 3,
        };
        let join = call_pointer(
            &self.backend.builder,
            self.backend.native_join_create(),
            &[
                self.runtime_context.into(),
                self.backend
                    .context
                    .i32_type()
                    .const_int(mode, false)
                    .into(),
                self.backend
                    .context
                    .i32_type()
                    .const_int(shape, false)
                    .into(),
            ],
            "task.join.create",
        )?;
        let missing = self
            .backend
            .builder
            .build_is_null(join, "task.join.missing")
            .map_err(builder_error)?;
        self.fail_if(missing, "TaskAllocationFault")?;

        let dynamic = matches!(
            arguments,
            [Expr {
                ty: Type::List(element),
                ..
            }] if matches!(element.as_ref(), Type::Task(_))
        );
        if dynamic {
            let list = self.alloc_value("task.join.list");
            if !self.emit_expr(&arguments[0], list)? {
                return Ok(false);
            }
            let status = call_int(
                &self.backend.builder,
                self.backend.native_join_add_list(),
                &[join.into(), list.into()],
                "task.join.add.list",
            )?;
            self.propagate_runtime_status(status, self.runtime_context, "task.join.add.list")?;
        } else {
            for argument in arguments {
                let value = self.alloc_value("task.join.argument");
                if !self.emit_expr(argument, value)? {
                    return Ok(false);
                }
                let task = self.backend.load_pointer_field(
                    self.backend.value_type,
                    value,
                    VALUE_FIELD_DATA,
                    "task.join.argument.pointer",
                )?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_join_add_task(),
                    &[join.into(), task.into()],
                    "task.join.add.task",
                )?;
                self.propagate_runtime_status(status, self.runtime_context, "task.join.add.task")?;
            }
        }
        let task = call_pointer(
            &self.backend.builder,
            self.backend.native_join_task(),
            &[join.into()],
            "task.join.task",
        )?;
        let missing = self
            .backend
            .builder
            .build_is_null(task, "task.join.task.missing")
            .map_err(builder_error)?;
        self.fail_if(missing, "TaskAllocationFault")?;
        self.initialize(destination, VALUE_TAG_TASK)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(TASK_VALUE_DIRECT),
        )?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            task,
        )?;
        Ok(true)
    }

    fn set_resume_state_and_suspend(
        &self,
        task: PointerValue<'ctx>,
        state: u32,
    ) -> Result<(), CodegenError> {
        self.backend
            .builder
            .build_call(
                self.backend.native_task_set_state(),
                &[
                    task.into(),
                    self.backend
                        .i64_type
                        .const_int(u64::from(state), false)
                        .into(),
                ],
                "task.state.set",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_return(Some(
                &self
                    .backend
                    .context
                    .i32_type()
                    .const_int(TASK_STEP_PENDING, false),
            ))
            .map_err(builder_error)?;
        Ok(())
    }

    fn begin_completion_wait(
        &self,
        state: u32,
        result: PointerValue<'ctx>,
    ) -> Result<CompletionWait<'ctx>, CodegenError> {
        let frame = self.alloc_coroutine_frame(state, result)?;
        let executor = self.runtime_context;
        let source = self.alloc_completion_wait_source()?;
        let registration =
            self.alloc_temporary(self.backend.registration_type, "wait.registration")?;
        let register_status = call_int(
            &self.backend.builder,
            self.backend.native_executor_register(),
            &[
                executor.into(),
                source.into(),
                frame.into(),
                registration.into(),
            ],
            "wait.register",
        )?;
        self.propagate_runtime_status(register_status, executor, "wait.register")?;
        Ok(CompletionWait {
            executor,
            frame,
            registration,
        })
    }

    fn emit_sleep_await(
        &self,
        state: u32,
        milliseconds: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let duration_value = self.alloc_value("sleep.duration");
        if !self.emit_expr(milliseconds, duration_value)? {
            return Ok(false);
        }
        let milliseconds = self.duration_scalar(milliseconds, duration_value)?;
        let negative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                milliseconds,
                self.backend.i64_type.const_zero(),
                "sleep.duration.negative",
            )
            .map_err(builder_error)?;
        self.fail_if(negative, "InvalidSleepDuration")?;
        let nanoseconds = self.checked_timer_multiply(milliseconds)?;
        let now = call_int(
            &self.backend.builder,
            self.backend.native_wait_now_ns(),
            &[],
            "wait.now",
        )?;
        let deadline = self.checked_timer_add(now, nanoseconds)?;

        let frame = self.alloc_coroutine_frame(state, destination)?;
        let source = self.alloc_timer_wait_source(deadline)?;
        let registration =
            self.alloc_temporary(self.backend.registration_type, "wait.registration")?;
        let register_status = call_int(
            &self.backend.builder,
            self.backend.native_executor_register(),
            &[
                self.runtime_context.into(),
                source.into(),
                frame.into(),
                registration.into(),
            ],
            "timer.register",
        )?;
        self.propagate_runtime_status(register_status, self.runtime_context, "timer.register")?;

        let pending = self.append_block("coroutine.pending.timer");
        self.backend.branch(pending)?;
        self.backend.builder.position_at_end(pending);
        let ready_count = self.alloc_temporary(self.backend.context.i32_type(), "ready.count")?;
        let wait_status = call_int(
            &self.backend.builder,
            self.backend.native_executor_wait(),
            &[
                self.runtime_context.into(),
                self.backend.i64_type.const_int(u64::MAX, false).into(),
                ready_count.into(),
            ],
            "timer.wait",
        )?;
        self.propagate_runtime_status(wait_status, self.runtime_context, "timer.wait")?;
        self.consume_ready_frame(self.runtime_context, frame, READY_EVENT_TIMER)?;
        self.emit_constant(&Constant::Unit, destination)?;
        Ok(true)
    }

    fn emit_sleep_join(
        &self,
        state: u32,
        tasks: &[Expr],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let frame = self.alloc_coroutine_frame(state, destination)?;
        for task in tasks {
            let ExprKind::Sleep { milliseconds } = &task.kind else {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "non-timer task reached the static timer join",
                ));
            };
            let duration_value = self.alloc_value("sleep.join.duration");
            if !self.emit_expr(milliseconds, duration_value)? {
                return Ok(false);
            }
            let milliseconds = self.duration_scalar(milliseconds, duration_value)?;
            let negative = self
                .backend
                .builder
                .build_int_compare(
                    IntPredicate::SLT,
                    milliseconds,
                    self.backend.i64_type.const_zero(),
                    "sleep.join.duration.negative",
                )
                .map_err(builder_error)?;
            self.fail_if(negative, "InvalidSleepDuration")?;
            let nanoseconds = self.checked_timer_multiply(milliseconds)?;
            let now = call_int(
                &self.backend.builder,
                self.backend.native_wait_now_ns(),
                &[],
                "sleep.join.now",
            )?;
            let deadline = self.checked_timer_add(now, nanoseconds)?;
            let source = self.alloc_timer_wait_source(deadline)?;
            let registration =
                self.alloc_temporary(self.backend.registration_type, "sleep.join.registration")?;
            let status = call_int(
                &self.backend.builder,
                self.backend.native_executor_register(),
                &[
                    self.runtime_context.into(),
                    source.into(),
                    frame.into(),
                    registration.into(),
                ],
                "sleep.join.register",
            )?;
            self.propagate_runtime_status(status, self.runtime_context, "sleep.join.register")?;
        }

        for _ in tasks {
            let pending = self.append_block("coroutine.pending.timer.join");
            self.backend.branch(pending)?;
            self.backend.builder.position_at_end(pending);
            let ready_count =
                self.alloc_temporary(self.backend.context.i32_type(), "sleep.join.ready.count")?;
            let status = call_int(
                &self.backend.builder,
                self.backend.native_executor_wait(),
                &[
                    self.runtime_context.into(),
                    self.backend.i64_type.const_int(u64::MAX, false).into(),
                    ready_count.into(),
                ],
                "sleep.join.wait",
            )?;
            self.propagate_runtime_status(status, self.runtime_context, "sleep.join.wait")?;
            self.consume_ready_frame(self.runtime_context, frame, READY_EVENT_TIMER)?;
        }

        let values = tasks
            .iter()
            .map(|task| Expr {
                kind: ExprKind::Constant(Constant::Unit),
                ty: Type::Unit,
                span: task.span,
            })
            .collect::<Vec<_>>();
        self.emit_tuple(&values, destination)
    }

    fn checked_timer_multiply(
        &self,
        milliseconds: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.smul.with.overflow")
            .and_then(|intrinsic| {
                intrinsic.get_declaration(&self.backend.module, &[self.backend.i64_type.into()])
            })
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "missing llvm.smul.with.overflow for Task.sleep",
                )
            })?;
        let aggregate = self
            .backend
            .builder
            .build_call(
                intrinsic,
                &[
                    milliseconds.into(),
                    self.backend.i64_type.const_int(1_000_000, false).into(),
                ],
                "sleep.nanoseconds.checked",
            )
            .map_err(builder_error)?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sleep multiply returned void"))?
            .into_struct_value();
        let value = self
            .backend
            .builder
            .build_extract_value(aggregate, 0, "sleep.nanoseconds")
            .map_err(builder_error)?
            .into_int_value();
        let overflow = self
            .backend
            .builder
            .build_extract_value(aggregate, 1, "sleep.duration.overflow")
            .map_err(builder_error)?
            .into_int_value();
        self.fail_if(overflow, "SleepDurationOverflow")?;
        Ok(value)
    }

    fn checked_timer_add(
        &self,
        now: IntValue<'ctx>,
        duration: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.uadd.with.overflow")
            .and_then(|intrinsic| {
                intrinsic.get_declaration(&self.backend.module, &[self.backend.i64_type.into()])
            })
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "missing llvm.uadd.with.overflow for Task.sleep",
                )
            })?;
        let aggregate = self
            .backend
            .builder
            .build_call(
                intrinsic,
                &[now.into(), duration.into()],
                "sleep.deadline.checked",
            )
            .map_err(builder_error)?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sleep deadline add returned void"))?
            .into_struct_value();
        let value = self
            .backend
            .builder
            .build_extract_value(aggregate, 0, "sleep.deadline")
            .map_err(builder_error)?
            .into_int_value();
        let overflow = self
            .backend
            .builder
            .build_extract_value(aggregate, 1, "sleep.deadline.overflow")
            .map_err(builder_error)?
            .into_int_value();
        self.fail_if(overflow, "SleepDurationOverflow")?;
        Ok(value)
    }

    fn finish_completion_wait(&self, wait: CompletionWait<'ctx>) -> Result<(), CodegenError> {
        let notify_status = call_int(
            &self.backend.builder,
            self.backend.native_executor_notify_completion(),
            &[
                wait.executor.into(),
                wait.registration.into(),
                self.backend
                    .context
                    .i32_type()
                    .const_int(READY_EVENT_COMPLETED, false)
                    .into(),
                self.backend.context.i32_type().const_zero().into(),
            ],
            "wait.notify",
        )?;
        self.propagate_runtime_status(notify_status, wait.executor, "wait.notify")?;
        let ready_count = self.alloc_temporary(self.backend.context.i32_type(), "ready.count")?;
        let wait_status = call_int(
            &self.backend.builder,
            self.backend.native_executor_wait(),
            &[
                wait.executor.into(),
                self.backend.i64_type.const_zero().into(),
                ready_count.into(),
            ],
            "wait.poll",
        )?;
        self.propagate_runtime_status(wait_status, wait.executor, "wait.poll")?;
        self.consume_ready_frame(wait.executor, wait.frame, READY_EVENT_COMPLETED)
    }

    fn alloc_coroutine_frame(
        &self,
        state: u32,
        result: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let frame = self.alloc_temporary(self.backend.coroutine_frame_type, "coroutine.frame")?;
        self.backend.store_i64_field(
            self.backend.coroutine_frame_type,
            frame,
            COROUTINE_FRAME_FIELD_STATE,
            self.backend.i64_type.const_int(u64::from(state), false),
        )?;
        self.backend.store_pointer_field(
            self.backend.coroutine_frame_type,
            frame,
            COROUTINE_FRAME_FIELD_RESULT,
            result,
        )?;
        Ok(frame)
    }

    fn alloc_completion_wait_source(&self) -> Result<PointerValue<'ctx>, CodegenError> {
        let source = self.alloc_temporary(self.backend.wait_source_type, "wait.source")?;
        self.backend
            .builder
            .build_store(source, self.backend.wait_source_type.const_zero())
            .map_err(builder_error)?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_ABI_VERSION,
            self.backend
                .context
                .i32_type()
                .const_int(WAIT_ABI_VERSION, false),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_KIND,
            self.backend
                .context
                .i32_type()
                .const_int(WAIT_SOURCE_KIND_COMPLETION, false),
        )?;
        self.backend.store_i64_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_HANDLE,
            self.backend.signed_i64(-1),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_INTERESTS,
            self.backend.context.i32_type().const_zero(),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_RESERVED,
            self.backend.context.i32_type().const_zero(),
        )?;
        self.backend.store_i64_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_DEADLINE,
            self.backend.i64_type.const_zero(),
        )?;
        Ok(source)
    }

    fn alloc_timer_wait_source(
        &self,
        deadline: IntValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let source = self.alloc_temporary(self.backend.wait_source_type, "wait.source.timer")?;
        self.backend
            .builder
            .build_store(source, self.backend.wait_source_type.const_zero())
            .map_err(builder_error)?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_ABI_VERSION,
            self.backend
                .context
                .i32_type()
                .const_int(WAIT_ABI_VERSION, false),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_KIND,
            self.backend
                .context
                .i32_type()
                .const_int(WAIT_SOURCE_KIND_TIMER, false),
        )?;
        self.backend.store_i64_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_HANDLE,
            self.backend.signed_i64(-1),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_INTERESTS,
            self.backend.context.i32_type().const_zero(),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_RESERVED,
            self.backend.context.i32_type().const_zero(),
        )?;
        self.backend.store_i64_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_DEADLINE,
            deadline,
        )?;
        Ok(source)
    }

    fn alloc_fd_wait_source(
        &self,
        descriptor: IntValue<'ctx>,
        interests: u64,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let source = self.alloc_temporary(self.backend.wait_source_type, "wait.source.fd")?;
        self.backend
            .builder
            .build_store(source, self.backend.wait_source_type.const_zero())
            .map_err(builder_error)?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_ABI_VERSION,
            self.backend
                .context
                .i32_type()
                .const_int(WAIT_ABI_VERSION, false),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_KIND,
            self.backend
                .context
                .i32_type()
                .const_int(WAIT_SOURCE_KIND_FD, false),
        )?;
        self.backend.store_i64_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_HANDLE,
            descriptor,
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_INTERESTS,
            self.backend.context.i32_type().const_int(interests, false),
        )?;
        self.backend.store_i32_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_RESERVED,
            self.backend.context.i32_type().const_zero(),
        )?;
        self.backend.store_i64_field(
            self.backend.wait_source_type,
            source,
            WAIT_SOURCE_FIELD_DEADLINE,
            self.backend.i64_type.const_zero(),
        )?;
        Ok(source)
    }

    fn consume_ready_frame(
        &self,
        executor: PointerValue<'ctx>,
        frame: PointerValue<'ctx>,
        expected_event: u64,
    ) -> Result<(), CodegenError> {
        let notification =
            self.alloc_temporary(self.backend.ready_notification_type, "ready.notification")?;
        let pop_status = call_int(
            &self.backend.builder,
            self.backend.native_executor_pop_ready(),
            &[executor.into(), notification.into()],
            "ready.pop",
        )?;
        let ready_frame = self.backend.load_pointer_field(
            self.backend.ready_notification_type,
            notification,
            READY_NOTIFICATION_FIELD_FRAME,
            "ready.frame",
        )?;
        let popped = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                pop_status,
                self.backend.context.i32_type().const_int(1, false),
                "ready.popped",
            )
            .map_err(builder_error)?;
        let same_frame = self
            .backend
            .builder
            .build_int_compare(IntPredicate::EQ, ready_frame, frame, "ready.same.frame")
            .map_err(builder_error)?;
        let events = self.backend.load_i32_field(
            self.backend.ready_notification_type,
            notification,
            READY_NOTIFICATION_FIELD_EVENTS,
            "ready.events",
        )?;
        let expected = self
            .backend
            .context
            .i32_type()
            .const_int(expected_event, false);
        let relevant = self
            .backend
            .builder
            .build_and(events, expected, "ready.relevant.events")
            .map_err(builder_error)?;
        let has_expected_event = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                relevant,
                self.backend.context.i32_type().const_zero(),
                "ready.expected.event",
            )
            .map_err(builder_error)?;
        let valid_identity = self
            .backend
            .builder
            .build_and(popped, same_frame, "ready.valid")
            .map_err(builder_error)?;
        let valid = self
            .backend
            .builder
            .build_and(valid_identity, has_expected_event, "ready.valid.event")
            .map_err(builder_error)?;
        let resume = self.append_block("coroutine.resume");
        let invalid = self.append_block("ready.invalid");
        self.backend
            .builder
            .build_conditional_branch(valid, resume, invalid)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(invalid);
        self.backend
            .puts("RuntimeFault: invalid wait notification")?;
        self.emit_all_cleanups()?;
        self.emit_status_return(self.failure_status(), None)?;
        self.backend.builder.position_at_end(resume);
        Ok(())
    }

    fn propagate_runtime_status(
        &self,
        status: IntValue<'ctx>,
        _runtime_context: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let success = self.append_block(&format!("{name}.success"));
        let failure = self.append_block(&format!("{name}.failure"));
        let ok = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_zero(),
                &format!("{name}.ok"),
            )
            .map_err(builder_error)?;
        build_weighted_conditional_branch(
            self.backend.context,
            &self.backend.builder,
            ok,
            success,
            failure,
            LikelyBranch::Then,
        )?;
        self.backend.builder.position_at_end(failure);
        self.emit_all_cleanups()?;
        let status = if self.task.is_some() {
            self.failure_status()
        } else {
            status
        };
        self.emit_status_return(status, None)?;
        self.backend.builder.position_at_end(success);
        Ok(())
    }

    fn emit_unary(
        &self,
        operator: UnaryOp,
        expression: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let value = self.alloc_value("unary");
        if !self.emit_expr(expression, value)? {
            return Ok(false);
        }
        match operator {
            UnaryOp::Not => {
                let boolean = self.bool_value(value)?;
                let result = self
                    .backend
                    .builder
                    .build_not(boolean, "not")
                    .map_err(builder_error)?;
                self.initialize(destination, VALUE_TAG_BOOL)?;
                let extended = self
                    .backend
                    .builder
                    .build_int_z_extend(result, self.backend.i64_type, "bool.i64")
                    .map_err(builder_error)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    extended,
                )?;
            }
            UnaryOp::Negate => match self.numeric_kind(&expression.ty)? {
                NumericKind::Int => {
                    let scalar = self.int_scalar(value)?;
                    let overflow = self
                        .backend
                        .builder
                        .build_int_compare(
                            IntPredicate::EQ,
                            scalar,
                            self.backend.signed_i64(i64::MIN),
                            "negate.overflow",
                        )
                        .map_err(builder_error)?;
                    self.fail_if(overflow, "IntegerOverflow")?;
                    let result = self
                        .backend
                        .builder
                        .build_int_sub(self.backend.i64_type.const_zero(), scalar, "negate")
                        .map_err(builder_error)?;
                    self.initialize(destination, VALUE_TAG_INT)?;
                    self.backend.store_i64_field(
                        self.backend.value_type,
                        destination,
                        VALUE_FIELD_SCALAR,
                        result,
                    )?;
                }
                NumericKind::Float => {
                    let scalar = self.float_scalar(value)?;
                    let result = self
                        .backend
                        .builder
                        .build_float_neg(scalar, "negate")
                        .map_err(builder_error)?;
                    self.store_float(destination, result)?;
                }
            },
        }
        Ok(true)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_binary(
        &self,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        if matches!(operator, BinaryOp::And | BinaryOp::Or) {
            return self.emit_logical(operator, left, right, destination);
        }
        let left_value = self.alloc_value("left");
        if !self.emit_expr(left, left_value)? {
            return Ok(false);
        }
        let right_value = self.alloc_value("right");
        if !self.emit_expr(right, right_value)? {
            return Ok(false);
        }
        match operator {
            BinaryOp::Equal | BinaryOp::NotEqual => {
                let equal = self
                    .backend
                    .module
                    .get_function("loom.runtime.equal")
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "equal helper is missing"))?;
                let mut result = call_int(
                    &self.backend.builder,
                    equal,
                    &[left_value.into(), right_value.into()],
                    "equal",
                )?;
                if operator == BinaryOp::NotEqual {
                    result = self
                        .backend
                        .builder
                        .build_not(result, "not.equal")
                        .map_err(builder_error)?;
                }
                self.store_bool(destination, result)?;
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                match self.numeric_kind(&left.ty)? {
                    NumericKind::Int => {
                        let left = self.int_scalar(left_value)?;
                        let right = self.int_scalar(right_value)?;
                        let result = self.emit_checked_integer(operator, left, right)?;
                        self.initialize(destination, VALUE_TAG_INT)?;
                        self.backend.store_i64_field(
                            self.backend.value_type,
                            destination,
                            VALUE_FIELD_SCALAR,
                            result,
                        )?;
                    }
                    NumericKind::Float => {
                        let left = self.float_scalar(left_value)?;
                        let right = self.float_scalar(right_value)?;
                        let result = match operator {
                            BinaryOp::Add => {
                                self.backend.builder.build_float_add(left, right, "add")
                            }
                            BinaryOp::Subtract => {
                                self.backend.builder.build_float_sub(left, right, "sub")
                            }
                            BinaryOp::Multiply => {
                                self.backend.builder.build_float_mul(left, right, "mul")
                            }
                            BinaryOp::Divide => {
                                self.backend.builder.build_float_div(left, right, "div")
                            }
                            _ => unreachable!(),
                        }
                        .map_err(builder_error)?;
                        self.store_float(destination, result)?;
                    }
                }
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                let result = match self.numeric_kind(&left.ty)? {
                    NumericKind::Int => {
                        let left = self.int_scalar(left_value)?;
                        let right = self.int_scalar(right_value)?;
                        let predicate = match operator {
                            BinaryOp::Less => IntPredicate::SLT,
                            BinaryOp::LessEqual => IntPredicate::SLE,
                            BinaryOp::Greater => IntPredicate::SGT,
                            BinaryOp::GreaterEqual => IntPredicate::SGE,
                            _ => unreachable!(),
                        };
                        self.backend
                            .builder
                            .build_int_compare(predicate, left, right, "compare")
                            .map_err(builder_error)?
                    }
                    NumericKind::Float => {
                        let left = self.float_scalar(left_value)?;
                        let right = self.float_scalar(right_value)?;
                        let predicate = match operator {
                            BinaryOp::Less => FloatPredicate::OLT,
                            BinaryOp::LessEqual => FloatPredicate::OLE,
                            BinaryOp::Greater => FloatPredicate::OGT,
                            BinaryOp::GreaterEqual => FloatPredicate::OGE,
                            _ => unreachable!(),
                        };
                        self.backend
                            .builder
                            .build_float_compare(predicate, left, right, "compare")
                            .map_err(builder_error)?
                    }
                };
                self.store_bool(destination, result)?;
            }
            BinaryOp::And | BinaryOp::Or => unreachable!(),
        }
        Ok(true)
    }

    fn emit_logical(
        &self,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let left_value = self.alloc_value("logical.left");
        if !self.emit_expr(left, left_value)? {
            return Ok(false);
        }
        let condition = self.bool_value(left_value)?;
        let evaluate_right = self.append_block("logical.right");
        let constant = self.append_block("logical.constant");
        let merge = self.append_block("logical.merge");
        let (true_block, false_block, constant_value) = if operator == BinaryOp::And {
            (evaluate_right, constant, false)
        } else {
            (constant, evaluate_right, true)
        };
        self.backend
            .builder
            .build_conditional_branch(condition, true_block, false_block)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(constant);
        self.emit_constant(&Constant::Bool(constant_value), destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(evaluate_right);
        let right_continues = self.emit_expr(right, destination)?;
        if right_continues {
            self.backend.branch(merge)?;
        }
        self.backend.builder.position_at_end(merge);
        if right_continues {
            Ok(true)
        } else {
            // The constant branch always reaches the merge.
            Ok(true)
        }
    }

    fn emit_if(
        &self,
        condition: &Expr,
        then_branch: &Block,
        else_branch: &Block,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let value = self.alloc_value("if.condition");
        if !self.emit_expr(condition, value)? {
            return Ok(false);
        }
        let condition = self.bool_value(value)?;
        let then_block = self.append_block("if.then");
        let else_block = self.append_block("if.else");
        let merge = self.append_block("if.merge");
        self.backend
            .builder
            .build_conditional_branch(condition, then_block, else_block)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(then_block);
        let then_continues = self.emit_block(then_branch, destination)?;
        if then_continues {
            self.backend.branch(merge)?;
        }
        self.backend.builder.position_at_end(else_block);
        let else_continues = self.emit_block(else_branch, destination)?;
        if else_continues {
            self.backend.branch(merge)?;
        }
        self.backend.builder.position_at_end(merge);
        if then_continues || else_continues {
            Ok(true)
        } else {
            self.backend
                .builder
                .build_unreachable()
                .map_err(builder_error)?;
            Ok(false)
        }
    }

    fn emit_checked_integer(
        &self,
        operator: BinaryOp,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        if operator == BinaryOp::Divide {
            let zero = self
                .backend
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    right,
                    self.backend.i64_type.const_zero(),
                    "division.zero",
                )
                .map_err(builder_error)?;
            self.fail_if(zero, "IntegerDivisionByZero")?;
            let min = self
                .backend
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    left,
                    self.backend.signed_i64(i64::MIN),
                    "division.min",
                )
                .map_err(builder_error)?;
            let minus_one = self
                .backend
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    right,
                    self.backend.signed_i64(-1),
                    "division.minus_one",
                )
                .map_err(builder_error)?;
            let overflow = self
                .backend
                .builder
                .build_and(min, minus_one, "division.overflow")
                .map_err(builder_error)?;
            self.fail_if(overflow, "IntegerDivisionOverflow")?;
            return self
                .backend
                .builder
                .build_int_signed_div(left, right, "division")
                .map_err(builder_error);
        }

        let name = match operator {
            BinaryOp::Add => "llvm.sadd.with.overflow",
            BinaryOp::Subtract => "llvm.ssub.with.overflow",
            BinaryOp::Multiply => "llvm.smul.with.overflow",
            _ => unreachable!(),
        };
        let intrinsic = inkwell::intrinsics::Intrinsic::find(name)
            .and_then(|intrinsic| {
                intrinsic.get_declaration(&self.backend.module, &[self.backend.i64_type.into()])
            })
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing {name}")))?;
        let aggregate = self
            .backend
            .builder
            .build_call(intrinsic, &[left.into(), right.into()], "checked.integer")
            .map_err(builder_error)?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "overflow intrinsic returned void"))?
            .into_struct_value();
        let result = self
            .backend
            .builder
            .build_extract_value(aggregate, 0, "integer.result")
            .map_err(builder_error)?
            .into_int_value();
        let overflow = self
            .backend
            .builder
            .build_extract_value(aggregate, 1, "integer.overflow")
            .map_err(builder_error)?
            .into_int_value();
        self.fail_if(overflow, "IntegerOverflow")?;
        Ok(result)
    }

    fn fail_if(&self, condition: IntValue<'ctx>, code: &str) -> Result<(), CodegenError> {
        let fail = self.append_block("operation.fail");
        let pass = self.append_block("operation.pass");
        build_weighted_conditional_branch(
            self.backend.context,
            &self.backend.builder,
            condition,
            fail,
            pass,
            LikelyBranch::Else,
        )?;
        self.backend.builder.position_at_end(fail);
        self.record_or_print_fault(code, native_fault_message(code), code)?;
        self.emit_all_cleanups()?;
        self.emit_status_return(self.failure_status(), None)?;
        self.backend.builder.position_at_end(pass);
        Ok(())
    }

    fn record_or_print_fault(
        &self,
        code: &str,
        message: &str,
        synchronous_display: &str,
    ) -> Result<(), CodegenError> {
        let detail = serde_json::to_string(&serde_json::json!({
            "channel": "runtime",
            "fault": {
                "code": code,
                "message": message,
                "span": self.source.span,
            },
        }))
        .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
        self.record_or_print_fault_with_detail(code, message, synchronous_display, &detail)
    }

    fn record_or_print_fault_with_detail(
        &self,
        code: &str,
        message: &str,
        synchronous_display: &str,
        detail: &str,
    ) -> Result<(), CodegenError> {
        self.backend.raise_fault(
            self.runtime_context,
            code,
            message,
            synchronous_display,
            detail,
        )
    }

    fn failure_status(&self) -> IntValue<'ctx> {
        self.backend.context.i32_type().const_int(
            self.unwind_status.get().unwrap_or_else(|| {
                if self.task.is_some() {
                    TASK_STEP_FAULTED
                } else {
                    1
                }
            }),
            false,
        )
    }

    fn numeric_kind(&self, ty: &Type) -> Result<NumericKind, CodegenError> {
        match ty {
            Type::Int => Ok(NumericKind::Int),
            Type::Float => Ok(NumericKind::Float),
            Type::Nominal(id, _) => {
                let definition = self.backend.program.type_def(*id).ok_or_else(|| {
                    CodegenError::new(
                        "InvalidTypeReference",
                        format!("type #{} does not exist", id.0),
                    )
                })?;
                if let TypeDefKind::Refined { base, .. } = &definition.kind {
                    self.numeric_kind(base)
                } else {
                    Err(CodegenError::new(
                        "InvalidNumericType",
                        format!("{} is not numeric", definition.name),
                    ))
                }
            }
            _ => Err(CodegenError::new(
                "InvalidNumericType",
                "checked numeric expression has a non-numeric type",
            )),
        }
    }

    fn int_scalar(&self, value: PointerValue<'ctx>) -> Result<IntValue<'ctx>, CodegenError> {
        let value = self.unwrap(value)?;
        self.backend.load_i64_field(
            self.backend.value_type,
            value,
            VALUE_FIELD_SCALAR,
            "int.scalar",
        )
    }

    fn float_scalar(
        &self,
        value: PointerValue<'ctx>,
    ) -> Result<inkwell::values::FloatValue<'ctx>, CodegenError> {
        let bits = self.int_scalar(value)?;
        self.backend
            .builder
            .build_bit_cast(bits, self.backend.context.f64_type(), "float.scalar")
            .map_err(builder_error)
            .map(BasicValueEnum::into_float_value)
    }

    fn store_bool(
        &self,
        destination: PointerValue<'ctx>,
        value: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        self.initialize(destination, VALUE_TAG_BOOL)?;
        let value = self
            .backend
            .builder
            .build_int_z_extend(value, self.backend.i64_type, "bool.i64")
            .map_err(builder_error)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_SCALAR,
            value,
        )
    }

    fn store_float(
        &self,
        destination: PointerValue<'ctx>,
        value: inkwell::values::FloatValue<'ctx>,
    ) -> Result<(), CodegenError> {
        self.initialize(destination, VALUE_TAG_FLOAT)?;
        let bits = self
            .backend
            .builder
            .build_bit_cast(value, self.backend.i64_type, "float.bits")
            .map_err(builder_error)?
            .into_int_value();
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_SCALAR,
            bits,
        )
    }

    fn emit_record(
        &self,
        ty: TypeId,
        fields: &[Expr],
        construction: ConstructionMode,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        self.emit_record_with_nodes(ty, fields, construction, destination, None)
    }

    fn emit_stack_record_copy(
        &self,
        local: LocalId,
        destination: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let Type::Nominal(ty, arguments) = self.local_type(local).ok_or_else(|| {
            CodegenError::new(
                "InvalidLocalReference",
                format!("local #{} has no static type", local.0),
            )
        })?
        else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "stack record copy source is not nominal",
            ));
        };
        if !arguments.is_empty() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "stack record copy source has generic arguments",
            ));
        }
        let nodes = self.stack_record_nodes.get(&local).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "stack record copy source has no private nodes",
            )
        })?;
        let values = nodes
            .iter()
            .copied()
            .map(|node| {
                self.backend.struct_pointer(
                    self.backend.value_node_type,
                    node,
                    VALUE_NODE_FIELD_VALUE,
                    "record.copy.field",
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The stack-record proof restricts these nodes to primitive fields. Materialize an
        // independent managed chain directly instead of passing the source header through the
        // universal clone helper: that remains a deep value copy, while keeping the private
        // source addresses nonescaping so LLVM can promote hot mutable fields to SSA.
        self.initialize(destination, VALUE_TAG_RECORD)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_NOMINAL,
            self.backend.tag(u64::from(ty.0)),
        )?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(values.len() as u64),
        )?;
        let head = self.build_value_nodes(&values)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            head,
        )
    }

    fn emit_stack_record_initializer(
        &self,
        expression: &Expr,
        destination: PointerValue<'ctx>,
        nodes: &[PointerValue<'ctx>],
    ) -> Result<bool, CodegenError> {
        self.backend.set_debug_location(
            self.function,
            expression.span.file.0,
            expression.span.range.start,
        );
        match &expression.kind {
            ExprKind::Record {
                ty,
                fields,
                construction,
                ..
            } => self.emit_record_with_nodes(*ty, fields, *construction, destination, Some(nodes)),
            ExprKind::Block(block) => self.emit_block_with_record_nodes(block, destination, nodes),
            _ => Err(CodegenError::new(
                "LlvmAbiDefect",
                "stack record candidate does not have a record initializer",
            )),
        }
    }

    fn emit_block_with_record_nodes(
        &self,
        block: &Block,
        destination: PointerValue<'ctx>,
        nodes: &[PointerValue<'ctx>],
    ) -> Result<bool, CodegenError> {
        self.backend
            .set_debug_location(self.function, block.span.file.0, block.span.range.start);
        let cleanup_base = self.cleanups.borrow().len();
        for statement in &block.statements {
            if let StatementKind::Defer(cleanup) = &statement.kind {
                self.cleanups.borrow_mut().push(cleanup.clone());
                continue;
            }
            if !self.emit_statement(statement)? {
                self.cleanups.borrow_mut().truncate(cleanup_base);
                return Ok(false);
            }
        }
        let tail = block.tail.as_deref().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "stack record initializer block has no tail expression",
            )
        })?;
        let continues = self.emit_stack_record_initializer(tail, destination, nodes)?;
        if continues {
            self.emit_cleanups_from(cleanup_base)?;
        }
        self.cleanups.borrow_mut().truncate(cleanup_base);
        Ok(continues)
    }

    fn emit_record_with_nodes(
        &self,
        ty: TypeId,
        fields: &[Expr],
        construction: ConstructionMode,
        destination: PointerValue<'ctx>,
        nodes: Option<&[PointerValue<'ctx>]>,
    ) -> Result<bool, CodegenError> {
        let record = if construction == ConstructionMode::Runtime {
            self.alloc_value("record.candidate")
        } else {
            destination
        };
        let Some(values) = self.emit_values(fields)? else {
            return Ok(false);
        };
        self.initialize(record, VALUE_TAG_RECORD)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            record,
            VALUE_FIELD_NOMINAL,
            self.backend.tag(u64::from(ty.0)),
        )?;
        self.backend.store_i64_field(
            self.backend.value_type,
            record,
            VALUE_FIELD_AUX,
            self.backend.tag(values.len() as u64),
        )?;
        let head = if let Some(nodes) = nodes {
            self.populate_value_nodes(nodes, &values)?
        } else {
            self.build_value_nodes(&values)?
        };
        self.backend.store_pointer_field(
            self.backend.value_type,
            record,
            VALUE_FIELD_DATA,
            head,
        )?;
        if construction != ConstructionMode::Runtime {
            return Ok(true);
        }
        let definition = self.backend.program.type_def(ty).ok_or_else(|| {
            CodegenError::new(
                "InvalidTypeReference",
                format!("type #{} does not exist", ty.0),
            )
        })?;
        let TypeDefKind::Record { invariant, .. } = &definition.kind else {
            return Err(CodegenError::new(
                "InvalidRecordConstruction",
                format!("{} is not a record", definition.name),
            ));
        };
        let Some(invariant) = invariant else {
            self.shallow_copy(destination, record)?;
            return Ok(true);
        };
        let context = ContractContext {
            receiver: Some(TypedPointer {
                pointer: record,
                ty: Type::Nominal(ty, Vec::new()),
            }),
            result: None,
            arguments: Vec::new(),
            old_receiver: None,
            old_arguments: Vec::new(),
            bindings: Vec::new(),
        };
        self.emit_checked_construction(
            invariant,
            &context,
            record,
            record,
            ty,
            "InvariantViolation",
            destination,
        )
    }

    fn emit_tuple(
        &self,
        elements: &[Expr],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let Some(values) = self.emit_values(elements)? else {
            return Ok(false);
        };
        self.initialize(destination, VALUE_TAG_TUPLE)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(values.len() as u64),
        )?;
        let head = self.build_value_nodes(&values)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            head,
        )?;
        Ok(true)
    }

    fn emit_list(
        &self,
        elements: &[Expr],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let Some(values) = self.emit_values(elements)? else {
            return Ok(false);
        };
        self.initialize(destination, VALUE_TAG_LIST)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(values.len() as u64),
        )?;
        let head = self.build_value_nodes(&values)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            head,
        )?;
        Ok(true)
    }

    fn emit_refine(
        &self,
        ty: TypeId,
        expression: &Expr,
        construction: ConstructionMode,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let value = self.alloc_value("refine.value");
        if !self.emit_expr(expression, value)? {
            return Ok(false);
        }
        let definition = self.backend.program.type_def(ty).ok_or_else(|| {
            CodegenError::new(
                "InvalidTypeReference",
                format!("type #{} does not exist", ty.0),
            )
        })?;
        let TypeDefKind::Refined { predicate, .. } = &definition.kind else {
            return Err(CodegenError::new(
                "InvalidRefinement",
                format!("{} is not a refined type", definition.name),
            ));
        };
        let refined = if construction == ConstructionMode::Runtime {
            self.alloc_value("refined.candidate")
        } else {
            destination
        };
        self.initialize(refined, VALUE_TAG_REFINED)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            refined,
            VALUE_FIELD_NOMINAL,
            self.backend.tag(u64::from(ty.0)),
        )?;
        let inner = call_pointer(
            &self.backend.builder,
            self.backend.native_gc_alloc_value(),
            &[],
            "refined.inner",
        )?;
        self.shallow_copy(inner, value)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            refined,
            VALUE_FIELD_DATA,
            inner,
        )?;
        if construction != ConstructionMode::Runtime {
            return Ok(true);
        }
        let context = ContractContext {
            receiver: Some(TypedPointer {
                pointer: value,
                ty: expression.ty.clone(),
            }),
            result: None,
            arguments: Vec::new(),
            old_receiver: None,
            old_arguments: Vec::new(),
            bindings: Vec::new(),
        };
        self.emit_checked_construction(
            predicate,
            &context,
            refined,
            value,
            ty,
            "ConstraintViolation",
            destination,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn emit_checked_construction(
        &self,
        contract: &Contract,
        context: &ContractContext<'ctx>,
        accepted_value: PointerValue<'ctx>,
        summary_source: PointerValue<'ctx>,
        target_type: TypeId,
        violation_code: &str,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let condition = self.alloc_value("constraint");
        if !self.emit_contract_expr(&contract.expression, context, condition)? {
            return Ok(false);
        }
        let accepted = self.bool_value(condition)?;
        let ok = self.append_block("constraint.ok");
        let error = self.append_block("constraint.error");
        let merge = self.append_block("constraint.merge");
        self.backend
            .builder
            .build_conditional_branch(accepted, ok, error)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(ok);
        self.emit_result(true, accepted_value, destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(error);
        let constraint_error = self.alloc_value("constraint_error");
        self.initialize(constraint_error, VALUE_TAG_CONSTRAINT_ERROR)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            constraint_error,
            VALUE_FIELD_NOMINAL,
            self.backend.tag(u64::from(target_type.0)),
        )?;
        let definition = self.backend.program.type_def(target_type).ok_or_else(|| {
            CodegenError::new(
                "InvalidTypeReference",
                format!("type #{} does not exist", target_type.0),
            )
        })?;
        let target = self.alloc_value("constraint.target");
        self.emit_constant(&Constant::Text(definition.name.clone()), target)?;
        let code = self.alloc_value("constraint.code");
        self.emit_constant(&Constant::Text(violation_code.into()), code)?;
        let predicate = self.alloc_value("constraint.predicate");
        self.emit_constant(&Constant::Text(contract.code.clone()), predicate)?;
        let path = self.alloc_value("constraint.path");
        self.initialize(path, VALUE_TAG_LIST)?;
        let summary = self.alloc_value("constraint.summary");
        let summary_status = call_int(
            &self.backend.builder,
            self.backend.native_value_summary(),
            &[summary_source.into(), summary.into()],
            "constraint.summary.build",
        )?;
        let summary_failed = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                summary_status,
                self.backend.context.i32_type().const_zero(),
                "constraint.summary.failed",
            )
            .map_err(builder_error)?;
        self.fail_if(summary_failed, "ConstraintSummaryFault")?;
        let contract_span = self.alloc_value("constraint.span");
        let span_values = [
            i64::from(contract.span.file.0),
            i64::from(contract.span.range.start),
            i64::from(contract.span.range.end),
        ]
        .into_iter()
        .map(|value| {
            let slot = self.alloc_value("constraint.span.value");
            self.emit_constant(&Constant::Int(value), slot)
                .map(|()| slot)
        })
        .collect::<Result<Vec<_>, _>>()?;
        self.initialize(contract_span, VALUE_TAG_TUPLE)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            contract_span,
            VALUE_FIELD_AUX,
            self.backend.tag(span_values.len() as u64),
        )?;
        let span_head = self.build_value_nodes(&span_values)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            contract_span,
            VALUE_FIELD_DATA,
            span_head,
        )?;
        let fields = [target, code, predicate, path, summary, contract_span];
        self.backend.store_i64_field(
            self.backend.value_type,
            constraint_error,
            VALUE_FIELD_AUX,
            self.backend.tag(fields.len() as u64),
        )?;
        let data = self.build_value_nodes(&fields)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            constraint_error,
            VALUE_FIELD_DATA,
            data,
        )?;
        self.emit_result(false, constraint_error, destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(merge);
        Ok(true)
    }

    fn emit_result(
        &self,
        ok: bool,
        payload: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let result = self
            .backend
            .program
            .prelude
            .result
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "Result type is missing"))?;
        self.emit_variant_from_pointers(result, u32::from(!ok), &[payload], destination)
    }

    fn emit_variant(
        &self,
        ty: TypeId,
        variant: u32,
        payload: &[Expr],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let Some(values) = self.emit_values(payload)? else {
            return Ok(false);
        };
        self.emit_variant_from_pointers(ty, variant, &values, destination)?;
        Ok(true)
    }

    fn emit_variant_from_pointers(
        &self,
        ty: TypeId,
        variant: u32,
        payload: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        self.initialize(destination, VALUE_TAG_ENUM)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_NOMINAL,
            self.backend.tag(u64::from(ty.0)),
        )?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(u64::from(variant)),
        )?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_SCALAR,
            self.backend.tag(payload.len() as u64),
        )?;
        let head = self.build_value_nodes(payload)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            head,
        )
    }

    fn emit_values(
        &self,
        expressions: &[Expr],
    ) -> Result<Option<Vec<PointerValue<'ctx>>>, CodegenError> {
        let mut values = Vec::with_capacity(expressions.len());
        for expression in expressions {
            let value = self.alloc_value("aggregate.value");
            if !self.emit_expr(expression, value)? {
                return Ok(None);
            }
            values.push(value);
        }
        Ok(Some(values))
    }

    fn build_value_nodes(
        &self,
        values: &[PointerValue<'ctx>],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let mut head = self.backend.ptr_type.const_null();
        for value in values.iter().rev() {
            let node = call_pointer(
                &self.backend.builder,
                self.backend.native_gc_alloc_value_node(),
                &[],
                "aggregate.node",
            )?;
            let node_value = self.backend.struct_pointer(
                self.backend.value_node_type,
                node,
                VALUE_NODE_FIELD_VALUE,
                "node.value",
            )?;
            self.shallow_copy(node_value, *value)?;
            self.backend.store_pointer_field(
                self.backend.value_node_type,
                node,
                VALUE_NODE_FIELD_NEXT,
                head,
            )?;
            head = node;
        }
        Ok(head)
    }

    fn populate_value_nodes(
        &self,
        nodes: &[PointerValue<'ctx>],
        values: &[PointerValue<'ctx>],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        if nodes.len() != values.len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "stack record field storage does not match its value count",
            ));
        }
        for (index, (node, value)) in nodes.iter().zip(values).enumerate() {
            let node_value = self.backend.struct_pointer(
                self.backend.value_node_type,
                *node,
                VALUE_NODE_FIELD_VALUE,
                "record.field.value",
            )?;
            self.shallow_copy(node_value, *value)?;
            let next = nodes
                .get(index + 1)
                .copied()
                .unwrap_or_else(|| self.backend.ptr_type.const_null());
            self.backend.store_pointer_field(
                self.backend.value_node_type,
                *node,
                VALUE_NODE_FIELD_NEXT,
                next,
            )?;
        }
        Ok(nodes
            .first()
            .copied()
            .unwrap_or_else(|| self.backend.ptr_type.const_null()))
    }

    fn emit_match(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let value = self.alloc_value("match.value");
        if !self.emit_expr(scrutinee, value)? {
            return Ok(false);
        }
        let merge = self.append_block("match.merge");
        let mut test_block = self.backend.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "match has no insertion block")
        })?;
        let mut any_continues = false;
        for arm in arms {
            self.backend.builder.position_at_end(test_block);
            let selected = self.append_block("match.arm");
            let next = self.append_block("match.next");
            let mut bindings = Vec::new();
            self.emit_pattern_branch(&arm.pattern, value, selected, next, &mut bindings)?;
            self.backend.builder.position_at_end(selected);
            if bindings.len() != arm.bindings.len() {
                return Err(CodegenError::new(
                    "InvalidPattern",
                    "pattern binding count does not match MIR locals",
                ));
            }
            for (local, binding) in arm.bindings.iter().zip(bindings) {
                self.clone_value(self.local(*local)?, binding)?;
            }
            let continues = self.emit_expr(&arm.value, destination)?;
            if continues {
                any_continues = true;
                self.backend.branch(merge)?;
            }
            test_block = next;
        }
        self.backend.builder.position_at_end(test_block);
        self.backend.puts("NonExhaustiveMatch")?;
        self.emit_all_cleanups()?;
        self.emit_status_return(self.failure_status(), None)?;
        self.backend.builder.position_at_end(merge);
        if any_continues {
            Ok(true)
        } else {
            self.backend
                .builder
                .build_unreachable()
                .map_err(builder_error)?;
            Ok(false)
        }
    }

    #[allow(clippy::too_many_lines)]
    fn emit_pattern_branch(
        &self,
        pattern: &Pattern,
        value: PointerValue<'ctx>,
        success: inkwell::basic_block::BasicBlock<'ctx>,
        failure: inkwell::basic_block::BasicBlock<'ctx>,
        bindings: &mut Vec<PointerValue<'ctx>>,
    ) -> Result<(), CodegenError> {
        match pattern {
            Pattern::Wildcard => self.backend.branch(success),
            Pattern::Binding => {
                bindings.push(value);
                self.backend.branch(success)
            }
            Pattern::Constant(constant) => {
                let expected = self.alloc_value("pattern.constant");
                self.emit_constant(constant, expected)?;
                let equal = self
                    .backend
                    .module
                    .get_function("loom.runtime.equal")
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "equal helper is missing"))?;
                let matches = call_int(
                    &self.backend.builder,
                    equal,
                    &[value.into(), expected.into()],
                    "pattern.equal",
                )?;
                self.backend
                    .builder
                    .build_conditional_branch(matches, success, failure)
                    .map_err(builder_error)?;
                Ok(())
            }
            Pattern::Variant {
                ty,
                variant,
                payload,
            } => {
                let tag = self.backend.load_i64_field(
                    self.backend.value_type,
                    value,
                    VALUE_FIELD_TAG,
                    "pattern.tag",
                )?;
                let nominal = self.backend.load_i64_field(
                    self.backend.value_type,
                    value,
                    VALUE_FIELD_NOMINAL,
                    "pattern.type",
                )?;
                let actual_variant = self.backend.load_i64_field(
                    self.backend.value_type,
                    value,
                    VALUE_FIELD_AUX,
                    "pattern.variant",
                )?;
                let tag_ok = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        self.backend.tag(VALUE_TAG_ENUM),
                        "pattern.enum",
                    )
                    .map_err(builder_error)?;
                let type_ok = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        nominal,
                        self.backend.tag(u64::from(ty.0)),
                        "pattern.type.ok",
                    )
                    .map_err(builder_error)?;
                let variant_ok = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        actual_variant,
                        self.backend.tag(u64::from(variant.0)),
                        "pattern.variant.ok",
                    )
                    .map_err(builder_error)?;
                let header = self
                    .backend
                    .builder
                    .build_and(tag_ok, type_ok, "pattern.header")
                    .map_err(builder_error)?;
                let header = self
                    .backend
                    .builder
                    .build_and(header, variant_ok, "pattern.header.variant")
                    .map_err(builder_error)?;
                if payload.is_empty() {
                    self.backend
                        .builder
                        .build_conditional_branch(header, success, failure)
                        .map_err(builder_error)?;
                    return Ok(());
                }
                let details = self.append_block("pattern.payload");
                self.backend
                    .builder
                    .build_conditional_branch(header, details, failure)
                    .map_err(builder_error)?;
                self.backend.builder.position_at_end(details);
                let data = self.backend.load_pointer_field(
                    self.backend.value_type,
                    value,
                    VALUE_FIELD_DATA,
                    "pattern.data",
                )?;
                for (index, child_pattern) in payload.iter().enumerate() {
                    let node = self.value_node_at(
                        data,
                        u32::try_from(index).map_err(|_| {
                            CodegenError::new("ProgramTooLarge", "pattern payload is too large")
                        })?,
                    )?;
                    let child = self.backend.struct_pointer(
                        self.backend.value_node_type,
                        node,
                        VALUE_NODE_FIELD_VALUE,
                        "pattern.value",
                    )?;
                    let child_success = if index + 1 == payload.len() {
                        success
                    } else {
                        self.append_block("pattern.child")
                    };
                    self.emit_pattern_branch(
                        child_pattern,
                        child,
                        child_success,
                        failure,
                        bindings,
                    )?;
                    if index + 1 != payload.len() {
                        self.backend.builder.position_at_end(child_success);
                    }
                }
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn emit_call(
        &self,
        target: &CallTarget,
        arguments: &[CallArgument],
        witnesses: &[WitnessRef],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        if let CallTarget::Builtin(builtin) = target {
            return self.emit_builtin(*builtin, arguments, destination);
        }
        if let CallTarget::Direct(function) = target
            && self.backend.native_functions.contains_key(function)
        {
            return self.emit_native_call(*function, arguments, witnesses, destination);
        }

        let Some(mut values) = self.emit_call_arguments(arguments)? else {
            return Ok(false);
        };
        let mut dynamic_writeback = None;
        let (direct, indirect, witness_head) = match target {
            CallTarget::Direct(function) | CallTarget::Inherent(function) => {
                let target = self
                    .backend
                    .functions
                    .get(function)
                    .copied()
                    .ok_or_else(|| {
                        CodegenError::new(
                            "ReachabilityDefect",
                            format!("call target #{} was not emitted", function.0),
                        )
                    })?;
                (
                    Some(target),
                    None,
                    self.build_witness_nodes(witnesses, self.backend.ptr_type.const_null())?,
                )
            }
            CallTarget::StaticConcept {
                requirement,
                witness,
                ..
            } => {
                let runtime_witness = self.resolve_witness(witness)?;
                let proof_head = self.backend.load_pointer_field(
                    self.backend.witness_type,
                    runtime_witness,
                    0,
                    "witness.arguments",
                )?;
                let method_head =
                    self.build_witness_nodes(witnesses, self.backend.ptr_type.const_null())?;
                let witness_head = self.concat_witness_nodes(proof_head, method_head)?;
                if let Some(witness_id) = concrete_witness_id(witness) {
                    let function = self
                        .backend
                        .program
                        .witness(witness_id)
                        .and_then(|definition| definition.methods.get(requirement))
                        .and_then(|function| self.backend.functions.get(function))
                        .copied()
                        .ok_or_else(|| {
                            CodegenError::new(
                                "ReachabilityDefect",
                                format!(
                                    "witness #{} requirement #{} was not emitted",
                                    witness_id.0, requirement.0
                                ),
                            )
                        })?;
                    (Some(function), None, witness_head)
                } else {
                    let method = self.load_witness_method(runtime_witness, *requirement)?;
                    (None, Some(method), witness_head)
                }
            }
            CallTarget::Dynamic { requirement } => {
                let receiver = values.first().copied().ok_or_else(|| {
                    CodegenError::new("InvalidDynamicCall", "dynamic call has no receiver")
                })?;
                let owner = self.backend.load_pointer_field(
                    self.backend.value_type,
                    receiver,
                    VALUE_FIELD_DATA,
                    "dyn.data",
                )?;
                let runtime_witness = self.backend.load_pointer_field(
                    self.backend.value_type,
                    receiver,
                    VALUE_FIELD_WITNESS,
                    "dyn.witness",
                )?;
                values[0] = owner;
                if self
                    .backend
                    .program
                    .requirement(*requirement)
                    .is_some_and(|definition| {
                        definition.receiver == Some(loom_mir::Receiver::Mutable)
                    })
                {
                    let writeback = self.backend.load_i64_field(
                        self.backend.value_type,
                        receiver,
                        VALUE_FIELD_SCALAR,
                        "dyn.writeback",
                    )?;
                    dynamic_writeback = Some((owner, writeback));
                }
                let method = self.load_witness_method(runtime_witness, *requirement)?;
                let witness_head = self.backend.load_pointer_field(
                    self.backend.witness_type,
                    runtime_witness,
                    0,
                    "dyn.witness.arguments",
                )?;
                (None, Some(method), witness_head)
            }
            CallTarget::Builtin(_) => unreachable!(),
        };
        let argument_head = self.build_argument_nodes(&values)?;
        let status = if let Some(function) = direct {
            call_int(
                &self.backend.builder,
                function,
                &[
                    destination.into(),
                    argument_head.into(),
                    witness_head.into(),
                    self.runtime_context.into(),
                ],
                "call",
            )?
        } else {
            self.backend
                .builder
                .build_indirect_call(
                    self.backend.loom_function_type,
                    indirect.expect("one call target kind is present"),
                    &[
                        destination.into(),
                        argument_head.into(),
                        witness_head.into(),
                        self.runtime_context.into(),
                    ],
                    "dyn.call",
                )
                .map_err(builder_error)?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "dynamic call returned void"))?
                .into_int_value()
        };
        if let Some((source, writeback)) = dynamic_writeback {
            self.emit_dynamic_writeback(source, writeback)?;
        }
        self.propagate_status(status)?;
        Ok(true)
    }

    fn emit_native_call(
        &self,
        function: FunctionId,
        arguments: &[CallArgument],
        witnesses: &[WitnessRef],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        if !witnesses.is_empty() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "native ABI call unexpectedly carries witnesses",
            ));
        }
        let Some(values) = self.emit_call_arguments(arguments)? else {
            return Ok(false);
        };
        let declaration = &self.backend.native_functions[&function];
        let signature = &declaration.signature;
        let mut call_arguments = Vec::<BasicMetadataValueEnum<'ctx>>::with_capacity(
            values.len() + usize::from(signature.effect() == NativeEffectAbi::RuntimeStatus),
        );
        for value in values {
            call_arguments.push(
                self.backend
                    .load_i64_field(
                        self.backend.value_type,
                        value,
                        VALUE_FIELD_SCALAR,
                        "argument.scalar",
                    )?
                    .into(),
            );
        }
        let scalar = if signature.effect() == NativeEffectAbi::PureNoFault {
            call_int(
                &self.backend.builder,
                declaration.function,
                &call_arguments,
                "integer.call",
            )?
        } else {
            call_arguments.push(self.runtime_context.into());
            let (status, scalar) = call_native_status(
                &self.backend.builder,
                declaration.function,
                signature,
                &call_arguments,
                "integer.call",
            )?;
            self.propagate_status(status)?;
            scalar
        };
        self.initialize(destination, VALUE_TAG_INT)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_SCALAR,
            scalar,
        )?;
        Ok(true)
    }

    fn emit_dynamic_writeback(
        &self,
        source: PointerValue<'ctx>,
        writeback: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let copy = self.append_block("dyn.writeback.copy");
        let done = self.append_block("dyn.writeback.done");
        let present = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                writeback,
                self.backend.i64_type.const_zero(),
                "dyn.writeback.present",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(present, copy, done)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(copy);
        let target = self
            .backend
            .builder
            .build_int_to_ptr(writeback, self.backend.ptr_type, "dyn.writeback.target")
            .map_err(builder_error)?;
        self.clone_value(target, source)?;
        self.backend.branch(done)?;
        self.backend.builder.position_at_end(done);
        Ok(())
    }

    fn emit_call_arguments(
        &self,
        arguments: &[CallArgument],
    ) -> Result<Option<Vec<PointerValue<'ctx>>>, CodegenError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            match argument {
                CallArgument::Value(expression) => {
                    let value = self.alloc_value("argument.value");
                    if !self.emit_expr(expression, value)? {
                        return Ok(None);
                    }
                    values.push(value);
                }
                CallArgument::InOut(place) => values.push(self.place(place)?),
            }
        }
        Ok(Some(values))
    }

    fn build_argument_nodes(
        &self,
        arguments: &[PointerValue<'ctx>],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let mut head = self.backend.ptr_type.const_null();
        for argument in arguments.iter().rev() {
            let node = self.alloc_temporary(self.backend.arg_node_type, "argument.node")?;
            self.backend.store_pointer_field(
                self.backend.arg_node_type,
                node,
                ARG_NODE_FIELD_VALUE,
                *argument,
            )?;
            self.backend.store_pointer_field(
                self.backend.arg_node_type,
                node,
                ARG_NODE_FIELD_NEXT,
                head,
            )?;
            head = node;
        }
        Ok(head)
    }

    fn build_witness_nodes(
        &self,
        witnesses: &[WitnessRef],
        mut tail: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        for witness in witnesses.iter().rev() {
            let value = self.resolve_witness(witness)?;
            let node = self.alloc_temporary(self.backend.witness_node_type, "witness.node")?;
            self.backend.store_pointer_field(
                self.backend.witness_node_type,
                node,
                WITNESS_NODE_FIELD_VALUE,
                value,
            )?;
            self.backend.store_pointer_field(
                self.backend.witness_node_type,
                node,
                WITNESS_NODE_FIELD_NEXT,
                tail,
            )?;
            tail = node;
        }
        Ok(tail)
    }

    fn concat_witness_nodes(
        &self,
        prefix: PointerValue<'ctx>,
        suffix: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let concatenate = self
            .backend
            .module
            .get_function("loom.runtime.concat_witnesses")
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "witness concat helper is missing")
            })?;
        call_pointer(
            &self.backend.builder,
            concatenate,
            &[prefix.into(), suffix.into()],
            "witness.arguments",
        )
    }

    fn resolve_witness(&self, witness: &WitnessRef) -> Result<PointerValue<'ctx>, CodegenError> {
        match witness {
            WitnessRef::Concrete(id) => self
                .backend
                .witnesses
                .get(id)
                .map(|global| global.as_pointer_value())
                .ok_or_else(|| {
                    CodegenError::new(
                        "ReachabilityDefect",
                        format!("witness #{} was not emitted", id.0),
                    )
                }),
            WitnessRef::Parameter(index) => {
                self.witness_parameters.get(index).copied().ok_or_else(|| {
                    CodegenError::new(
                        "InvalidWitnessReference",
                        format!("witness parameter #{index} does not exist"),
                    )
                })
            }
            WitnessRef::Apply { witness, arguments } => {
                let base = self
                    .backend
                    .witnesses
                    .get(witness)
                    .map(|global| global.as_pointer_value())
                    .ok_or_else(|| {
                        CodegenError::new(
                            "ReachabilityDefect",
                            format!("witness #{} was not emitted", witness.0),
                        )
                    })?;
                let applied =
                    self.alloc_temporary(self.backend.witness_type, "witness.application")?;
                let value = self
                    .backend
                    .builder
                    .build_load(self.backend.witness_type, base, "witness.base")
                    .map_err(builder_error)?;
                self.backend
                    .builder
                    .build_store(applied, value)
                    .map_err(builder_error)?;
                let arguments =
                    self.build_witness_nodes(arguments, self.backend.ptr_type.const_null())?;
                self.backend.store_pointer_field(
                    self.backend.witness_type,
                    applied,
                    0,
                    arguments,
                )?;
                Ok(applied)
            }
        }
    }

    fn load_witness_method(
        &self,
        witness: PointerValue<'ctx>,
        requirement: RequirementId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let field = WITNESS_METHOD_FIELD_OFFSET
            .checked_add(requirement.0)
            .ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "requirement method index overflowed")
            })?;
        self.backend
            .load_pointer_field(self.backend.witness_type, witness, field, "witness.method")
    }

    fn propagate_status(&self, status: IntValue<'ctx>) -> Result<(), CodegenError> {
        let success = self.append_block("call.success");
        let failure = self.append_block("call.failure");
        let ok = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_zero(),
                "call.ok",
            )
            .map_err(builder_error)?;
        build_weighted_conditional_branch(
            self.backend.context,
            &self.backend.builder,
            ok,
            success,
            failure,
            LikelyBranch::Then,
        )?;
        self.backend.builder.position_at_end(failure);
        self.emit_all_cleanups()?;
        let status = if self.task.is_some() {
            self.failure_status()
        } else {
            status
        };
        self.emit_status_return(status, None)?;
        self.backend.builder.position_at_end(success);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn emit_builtin(
        &self,
        builtin: Builtin,
        arguments: &[CallArgument],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        if matches!(
            builtin,
            Builtin::ListAdd | Builtin::ListLength | Builtin::ListGet
        ) {
            return self.emit_list_builtin(builtin, arguments, destination);
        }
        let Some(values) = self.emit_call_arguments(arguments)? else {
            return Ok(false);
        };
        if matches!(
            builtin,
            Builtin::ProcessArguments | Builtin::ProcessEnvironment
        ) {
            return self.emit_process_builtin(builtin, &values, destination);
        }
        if matches!(builtin, Builtin::TaskFaultCode | Builtin::TaskFaultMessage) {
            return self.emit_task_fault_builtin(builtin, &values, destination);
        }
        if matches!(
            builtin,
            Builtin::TextLength
                | Builtin::TextGet
                | Builtin::TextConcat
                | Builtin::TextContains
                | Builtin::TextEncodeUtf8
                | Builtin::BytesLength
                | Builtin::BytesGet
                | Builtin::BytesAppend
                | Builtin::BytesDecodeUtf8
                | Builtin::PathFromText
                | Builtin::PathAsText
                | Builtin::PathJoin
        ) {
            return self.emit_standard_value_builtin(builtin, &values, destination);
        }
        if matches!(
            builtin,
            Builtin::TextMapNew
                | Builtin::TextMapLength
                | Builtin::TextMapContains
                | Builtin::TextMapGet
                | Builtin::TextMapInsert
                | Builtin::TextMapRemove
                | Builtin::JsonParse
                | Builtin::JsonFormat
                | Builtin::IoErrorKind
                | Builtin::IoErrorMessage
                | Builtin::LogDebug
                | Builtin::LogInfo
                | Builtin::LogWarn
                | Builtin::LogError
                | Builtin::LogWrite
        ) {
            return self.emit_structured_value_builtin(builtin, &values, destination);
        }
        if matches!(
            builtin,
            Builtin::DurationMilliseconds
                | Builtin::DurationAsMilliseconds
                | Builtin::FileOpenRead
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
                | Builtin::FileClose
                | Builtin::SocketConnect
                | Builtin::SocketTryConnect
                | Builtin::SocketReadText
                | Builtin::SocketWriteText
                | Builtin::SocketTryReadText
                | Builtin::SocketTryWriteText
                | Builtin::SocketClose
        ) {
            return self.emit_standard_io_builtin(builtin, &values, destination);
        }
        match (builtin, values.as_slice()) {
            (Builtin::IsFinite, [value]) => {
                let number = self.float_scalar(*value)?;
                let ordered = self
                    .backend
                    .builder
                    .build_float_compare(FloatPredicate::ORD, number, number, "finite.ordered")
                    .map_err(builder_error)?;
                let upper = self
                    .backend
                    .builder
                    .build_float_compare(
                        FloatPredicate::OLE,
                        number,
                        self.backend.context.f64_type().const_float(f64::MAX),
                        "finite.upper",
                    )
                    .map_err(builder_error)?;
                let lower = self
                    .backend
                    .builder
                    .build_float_compare(
                        FloatPredicate::OGE,
                        number,
                        self.backend.context.f64_type().const_float(-f64::MAX),
                        "finite.lower",
                    )
                    .map_err(builder_error)?;
                let bounded = self
                    .backend
                    .builder
                    .build_and(upper, lower, "finite.bounded")
                    .map_err(builder_error)?;
                let finite = self
                    .backend
                    .builder
                    .build_and(ordered, bounded, "finite")
                    .map_err(builder_error)?;
                self.store_bool(destination, finite)?;
                Ok(true)
            }
            (Builtin::ParseFloat, [value]) => self.emit_parse_float(*value, destination),
            (Builtin::ParseInt, [value]) => self.emit_parse_int(*value, destination),
            (Builtin::FormatFloat, [value]) => self.emit_format_float(*value, destination),
            _ => Err(CodegenError::new(
                "InvalidBuiltinCall",
                "builtin argument shape does not match checked MIR",
            )),
        }
    }

    fn emit_task_fault_builtin(
        &self,
        builtin: Builtin,
        values: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let [fault] = values else {
            return Err(CodegenError::new(
                "InvalidBuiltinCall",
                "TaskFault accessor expects one receiver",
            ));
        };
        let fault = self.unwrap(*fault)?;
        let data = self.backend.load_pointer_field(
            self.backend.value_type,
            fault,
            VALUE_FIELD_DATA,
            "task.fault.fields",
        )?;
        let index = u32::from(builtin == Builtin::TaskFaultMessage);
        let node = self.value_node_at(data, index)?;
        let field = self.backend.struct_pointer(
            self.backend.value_node_type,
            node,
            VALUE_NODE_FIELD_VALUE,
            "task.fault.field",
        )?;
        self.clone_value(destination, field)?;
        Ok(true)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_standard_value_builtin(
        &self,
        builtin: Builtin,
        values: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let option = self
            .backend
            .program
            .prelude
            .option
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "Option is missing"))?;
        let result = self
            .backend
            .program
            .prelude
            .result
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "Result is missing"))?;
        match (builtin, values) {
            (Builtin::TextLength, [text]) => {
                let text = self.unwrap(*text)?;
                let scalar = self.backend.text_scalar_length(text, "text.length")?;
                self.initialize(destination, VALUE_TAG_INT)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    scalar,
                )?;
                Ok(true)
            }
            (Builtin::TextGet, [text, index]) => {
                let (data, length) = self.text_parts(*text, "text.get")?;
                let index = self.int_scalar(*index)?;
                let scalar = self.alloc_value("text.scalar");
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_text_get(),
                    &[data.into(), length.into(), index.into(), scalar.into()],
                    "text.get.status",
                )?;
                self.emit_option_from_status(status, scalar, option, destination, "text.get")?;
                Ok(true)
            }
            (Builtin::TextConcat, [left, right]) => {
                let (left_data, left_length) = self.text_parts(*left, "text.concat.left")?;
                let (right_data, right_length) = self.text_parts(*right, "text.concat.right")?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_text_concat(),
                    &[
                        left_data.into(),
                        left_length.into(),
                        right_data.into(),
                        right_length.into(),
                        destination.into(),
                    ],
                    "text.concat.status",
                )?;
                self.fail_on_standard_status(status, "TextRuntimeFault")?;
                Ok(true)
            }
            (Builtin::TextContains, [text, needle]) => {
                let (data, length) = self.text_parts(*text, "text.contains.value")?;
                let (needle_data, needle_length) =
                    self.text_parts(*needle, "text.contains.needle")?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_text_contains(),
                    &[
                        data.into(),
                        length.into(),
                        needle_data.into(),
                        needle_length.into(),
                    ],
                    "text.contains.status",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::SLT,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "text.contains.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "TextRuntimeFault")?;
                let contained = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "text.contains.value",
                    )
                    .map_err(builder_error)?;
                self.store_bool(destination, contained)?;
                Ok(true)
            }
            (Builtin::TextEncodeUtf8, [text]) => {
                let bytes = self
                    .backend
                    .program
                    .prelude
                    .bytes
                    .ok_or_else(|| CodegenError::new("InvalidPrelude", "Bytes is missing"))?;
                self.emit_opaque_record(bytes, *text, destination)?;
                Ok(true)
            }
            (Builtin::BytesLength, [bytes]) => {
                let bytes = self.opaque_record_field(*bytes, "bytes.length.payload")?;
                let (_, length) = self.text_parts(bytes, "bytes.length")?;
                self.initialize(destination, VALUE_TAG_INT)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    length,
                )?;
                Ok(true)
            }
            (Builtin::BytesGet, [bytes, index]) => {
                let bytes = self.opaque_record_field(*bytes, "bytes.get.payload")?;
                let (data, length) = self.text_parts(bytes, "bytes.get")?;
                let index = self.int_scalar(*index)?;
                let byte = self.alloc_value("bytes.item");
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_bytes_get(),
                    &[data.into(), length.into(), index.into(), byte.into()],
                    "bytes.get.status",
                )?;
                self.emit_option_from_status(status, byte, option, destination, "bytes.get")?;
                Ok(true)
            }
            (Builtin::BytesAppend, [left, right]) => {
                let left = self.opaque_record_field(*left, "bytes.append.left.payload")?;
                let right = self.opaque_record_field(*right, "bytes.append.right.payload")?;
                let (left_data, left_length) = self.text_parts(left, "bytes.append.left")?;
                let (right_data, right_length) = self.text_parts(right, "bytes.append.right")?;
                let payload = self.alloc_value("bytes.append.payload");
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_bytes_append(),
                    &[
                        left_data.into(),
                        left_length.into(),
                        right_data.into(),
                        right_length.into(),
                        payload.into(),
                    ],
                    "bytes.append.status",
                )?;
                self.fail_on_standard_status(status, "BytesRuntimeFault")?;
                let bytes = self
                    .backend
                    .program
                    .prelude
                    .bytes
                    .ok_or_else(|| CodegenError::new("InvalidPrelude", "Bytes is missing"))?;
                self.emit_opaque_record(bytes, payload, destination)?;
                Ok(true)
            }
            (Builtin::BytesDecodeUtf8, [bytes]) => {
                let bytes = self.opaque_record_field(*bytes, "bytes.decode.payload")?;
                let (data, length) = self.text_parts(bytes, "bytes.decode")?;
                let decoded = self.alloc_value("bytes.decode.text");
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_bytes_decode_utf8(),
                    &[data.into(), length.into(), decoded.into()],
                    "bytes.decode.status",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::SLT,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "bytes.decode.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "BytesRuntimeFault")?;
                let valid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "bytes.decode.valid",
                    )
                    .map_err(builder_error)?;
                let ok = self.append_block("bytes.decode.ok");
                let error = self.append_block("bytes.decode.error");
                let merge = self.append_block("bytes.decode.merge");
                self.backend
                    .builder
                    .build_conditional_branch(valid, ok, error)
                    .map_err(builder_error)?;
                self.backend.builder.position_at_end(ok);
                self.emit_variant_from_pointers(result, 0, &[decoded], destination)?;
                self.backend.branch(merge)?;
                self.backend.builder.position_at_end(error);
                let error_value = self.alloc_value("decode.error");
                let error_ty = self
                    .backend
                    .program
                    .prelude
                    .decode_text_error
                    .ok_or_else(|| {
                        CodegenError::new("InvalidPrelude", "DecodeTextError is missing")
                    })?;
                self.emit_variant_from_pointers(error_ty, 0, &[], error_value)?;
                self.emit_variant_from_pointers(result, 1, &[error_value], destination)?;
                self.backend.branch(merge)?;
                self.backend.builder.position_at_end(merge);
                Ok(true)
            }
            (Builtin::PathFromText, [text]) => {
                let (data, length) = self.text_parts(*text, "path.from_text")?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_path_contains_nul(),
                    &[data.into(), length.into()],
                    "path.from_text.status",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::SLT,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "path.from_text.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "PathRuntimeFault")?;
                let contains_nul = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "path.contains_nul",
                    )
                    .map_err(builder_error)?;
                self.emit_path_result(
                    contains_nul,
                    *text,
                    0,
                    result,
                    destination,
                    "path.from_text",
                )?;
                Ok(true)
            }
            (Builtin::PathAsText, [path]) => {
                let path = self.opaque_record_field(*path, "path.as_text.payload")?;
                self.clone_value(destination, path)?;
                Ok(true)
            }
            (Builtin::PathJoin, [base, child]) => {
                let base = self.opaque_record_field(*base, "path.join.base.payload")?;
                let child = self.opaque_record_field(*child, "path.join.child.payload")?;
                let (base_data, base_length) = self.text_parts(base, "path.join.base")?;
                let (child_data, child_length) = self.text_parts(child, "path.join.child")?;
                let joined = self.alloc_value("path.join.payload");
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_path_join(),
                    &[
                        base_data.into(),
                        base_length.into(),
                        child_data.into(),
                        child_length.into(),
                        joined.into(),
                    ],
                    "path.join.status",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::SLT,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "path.join.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "PathRuntimeFault")?;
                let absolute = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "path.join.absolute",
                    )
                    .map_err(builder_error)?;
                self.emit_path_result(absolute, joined, 1, result, destination, "path.join")?;
                Ok(true)
            }
            _ => Err(CodegenError::new(
                "InvalidBuiltinCall",
                "standard value builtin argument shape does not match checked MIR",
            )),
        }
    }

    fn fail_on_standard_status(
        &self,
        status: IntValue<'ctx>,
        fault: &str,
    ) -> Result<(), CodegenError> {
        let invalid = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                self.backend.context.i32_type().const_zero(),
                "standard.invalid",
            )
            .map_err(builder_error)?;
        self.fail_if(invalid, fault)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_structured_value_builtin(
        &self,
        builtin: Builtin,
        values: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        match (builtin, values) {
            (Builtin::TextMapNew, []) => {
                let map = self
                    .backend
                    .program
                    .prelude
                    .text_map
                    .ok_or_else(|| CodegenError::new("InvalidPrelude", "TextMap is missing"))?;
                self.initialize(destination, VALUE_TAG_RECORD)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_NOMINAL,
                    self.backend.tag(u64::from(map.0)),
                )?;
                Ok(true)
            }
            (Builtin::TextMapLength, [map]) => {
                let map = self.unwrap(*map)?;
                let slots = self.backend.load_i64_field(
                    self.backend.value_type,
                    map,
                    VALUE_FIELD_AUX,
                    "text.map.slots",
                )?;
                let length = self
                    .backend
                    .builder
                    .build_int_unsigned_div(
                        slots,
                        self.backend.i64_type.const_int(2, false),
                        "text.map.length",
                    )
                    .map_err(builder_error)?;
                self.initialize(destination, VALUE_TAG_INT)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    length,
                )?;
                Ok(true)
            }
            (Builtin::TextMapContains | Builtin::TextMapGet, [map, key]) => {
                let (key_data, key_length) = self.text_parts(*key, "text.map.key")?;
                let value = call_pointer(
                    &self.backend.builder,
                    self.backend.native_text_map_get(),
                    &[(*map).into(), key_data.into(), key_length.into()],
                    "text.map.value",
                )?;
                let found = self
                    .backend
                    .builder
                    .build_is_not_null(value, "text.map.found")
                    .map_err(builder_error)?;
                if builtin == Builtin::TextMapContains {
                    self.store_bool(destination, found)?;
                } else {
                    let option =
                        self.backend.program.prelude.option.ok_or_else(|| {
                            CodegenError::new("InvalidPrelude", "Option is missing")
                        })?;
                    let some = self.append_block("text.map.get.some");
                    let none = self.append_block("text.map.get.none");
                    let merge = self.append_block("text.map.get.merge");
                    self.backend
                        .builder
                        .build_conditional_branch(found, some, none)
                        .map_err(builder_error)?;
                    self.backend.builder.position_at_end(some);
                    let owned = self.alloc_value("text.map.get.owned");
                    self.clone_value(owned, value)?;
                    self.emit_variant_from_pointers(option, 1, &[owned], destination)?;
                    self.backend.branch(merge)?;
                    self.backend.builder.position_at_end(none);
                    self.emit_variant_from_pointers(option, 0, &[], destination)?;
                    self.backend.branch(merge)?;
                    self.backend.builder.position_at_end(merge);
                }
                Ok(true)
            }
            (Builtin::TextMapInsert, [map, key, value]) => {
                let owned = self.alloc_value("text.map.insert.owned");
                self.clone_value(owned, *value)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_text_map_insert(),
                    &[
                        (*map).into(),
                        (*key).into(),
                        owned.into(),
                        destination.into(),
                    ],
                    "text.map.insert.status",
                )?;
                self.fail_on_standard_status(status, "TextMapRuntimeFault")?;
                Ok(true)
            }
            (Builtin::TextMapRemove, [map, key]) => {
                let (key_data, key_length) = self.text_parts(*key, "text.map.remove.key")?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_text_map_remove(),
                    &[
                        (*map).into(),
                        key_data.into(),
                        key_length.into(),
                        destination.into(),
                    ],
                    "text.map.remove.status",
                )?;
                self.fail_on_standard_status(status, "TextMapRuntimeFault")?;
                Ok(true)
            }
            (Builtin::JsonParse, [text]) => {
                let (data, length) = self.text_parts(*text, "json.parse")?;
                let (result, json, error, map) = self.json_type_ids()?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_json_parse(),
                    &[
                        data.into(),
                        length.into(),
                        self.backend.tag(u64::from(result.0)).into(),
                        self.backend.tag(u64::from(json.0)).into(),
                        self.backend.tag(u64::from(error.0)).into(),
                        self.backend.tag(u64::from(map.0)).into(),
                        destination.into(),
                    ],
                    "json.parse.status",
                )?;
                self.fail_on_standard_status(status, "JsonRuntimeFault")?;
                Ok(true)
            }
            (Builtin::JsonFormat, [json]) => {
                let (result, json_type, error, map) = self.json_type_ids()?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_json_format(),
                    &[
                        (*json).into(),
                        self.backend.tag(u64::from(result.0)).into(),
                        self.backend.tag(u64::from(json_type.0)).into(),
                        self.backend.tag(u64::from(error.0)).into(),
                        self.backend.tag(u64::from(map.0)).into(),
                        destination.into(),
                    ],
                    "json.format.status",
                )?;
                self.fail_on_standard_status(status, "JsonRuntimeFault")?;
                Ok(true)
            }
            (Builtin::IoErrorKind | Builtin::IoErrorMessage, [error]) => {
                let error = self.unwrap(*error)?;
                let data = self.backend.load_pointer_field(
                    self.backend.value_type,
                    error,
                    VALUE_FIELD_DATA,
                    "io.error.fields",
                )?;
                let index = u32::from(builtin == Builtin::IoErrorMessage);
                let node = self.value_node_at(data, index)?;
                let field = self.backend.struct_pointer(
                    self.backend.value_node_type,
                    node,
                    VALUE_NODE_FIELD_VALUE,
                    "io.error.field",
                )?;
                self.clone_value(destination, field)?;
                Ok(true)
            }
            (
                Builtin::LogDebug | Builtin::LogInfo | Builtin::LogWarn | Builtin::LogError,
                [message],
            ) => {
                let level = match builtin {
                    Builtin::LogDebug => 0,
                    Builtin::LogInfo => 1,
                    Builtin::LogWarn => 2,
                    Builtin::LogError => 3,
                    _ => unreachable!(),
                };
                self.emit_log_write(
                    level,
                    *message,
                    self.backend.ptr_type.const_null(),
                    destination,
                )
            }
            (Builtin::LogWrite, [level, message, fields]) => {
                let level = self.unwrap(*level)?;
                let level = self.backend.load_i64_field(
                    self.backend.value_type,
                    level,
                    VALUE_FIELD_AUX,
                    "log.level",
                )?;
                let level = self
                    .backend
                    .builder
                    .build_int_truncate(level, self.backend.context.i32_type(), "log.level.i32")
                    .map_err(builder_error)?;
                self.emit_log_write_value(level, *message, *fields, destination)
            }
            _ => Err(CodegenError::new(
                "InvalidBuiltinCall",
                "structured value builtin argument shape does not match checked MIR",
            )),
        }
    }

    fn json_type_ids(&self) -> Result<(TypeId, TypeId, TypeId, TypeId), CodegenError> {
        Ok((
            self.backend
                .program
                .prelude
                .result
                .ok_or_else(|| CodegenError::new("InvalidPrelude", "Result is missing"))?,
            self.backend
                .program
                .prelude
                .json
                .ok_or_else(|| CodegenError::new("InvalidPrelude", "Json is missing"))?,
            self.backend
                .program
                .prelude
                .json_error
                .ok_or_else(|| CodegenError::new("InvalidPrelude", "JsonError is missing"))?,
            self.backend
                .program
                .prelude
                .text_map
                .ok_or_else(|| CodegenError::new("InvalidPrelude", "TextMap is missing"))?,
        ))
    }

    fn emit_log_write(
        &self,
        level: u64,
        message: PointerValue<'ctx>,
        fields: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        self.emit_log_write_value(
            self.backend.context.i32_type().const_int(level, false),
            message,
            fields,
            destination,
        )
    }

    fn emit_log_write_value(
        &self,
        level: IntValue<'ctx>,
        message: PointerValue<'ctx>,
        fields: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let (data, length) = self.text_parts(message, "log.message")?;
        let status = call_int(
            &self.backend.builder,
            self.backend.native_log_write(),
            &[level.into(), data.into(), length.into(), fields.into()],
            "log.write.status",
        )?;
        let failed = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                self.backend.context.i32_type().const_zero(),
                "log.write.failed",
            )
            .map_err(builder_error)?;
        self.fail_if(failed, "LogWriteFault")?;
        self.emit_constant(&Constant::Unit, destination)?;
        Ok(true)
    }

    fn emit_option_from_status(
        &self,
        status: IntValue<'ctx>,
        payload: PointerValue<'ctx>,
        option: TypeId,
        destination: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let invalid = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                status,
                self.backend.context.i32_type().const_zero(),
                &format!("{name}.invalid"),
            )
            .map_err(builder_error)?;
        self.fail_if(invalid, "SequenceRuntimeFault")?;
        let found = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                self.backend.context.i32_type().const_zero(),
                &format!("{name}.found"),
            )
            .map_err(builder_error)?;
        let some = self.append_block(&format!("{name}.some"));
        let none = self.append_block(&format!("{name}.none"));
        let merge = self.append_block(&format!("{name}.merge"));
        self.backend
            .builder
            .build_conditional_branch(found, some, none)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(some);
        self.emit_variant_from_pointers(option, 1, &[payload], destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(none);
        self.emit_variant_from_pointers(option, 0, &[], destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(merge);
        Ok(())
    }

    fn emit_path_result(
        &self,
        is_error: IntValue<'ctx>,
        payload: PointerValue<'ctx>,
        error_variant: u32,
        result: TypeId,
        destination: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let ok = self.append_block(&format!("{name}.ok"));
        let error = self.append_block(&format!("{name}.error"));
        let merge = self.append_block(&format!("{name}.merge"));
        self.backend
            .builder
            .build_conditional_branch(is_error, error, ok)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(ok);
        let path = self.alloc_value(&format!("{name}.path"));
        let path_ty = self
            .backend
            .program
            .prelude
            .path
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "Path is missing"))?;
        self.emit_opaque_record(path_ty, payload, path)?;
        self.emit_variant_from_pointers(result, 0, &[path], destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(error);
        let error_value = self.alloc_value(&format!("{name}.error.value"));
        let error_ty = self
            .backend
            .program
            .prelude
            .path_error
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "PathError is missing"))?;
        self.emit_variant_from_pointers(error_ty, error_variant, &[], error_value)?;
        self.emit_variant_from_pointers(result, 1, &[error_value], destination)?;
        self.backend.branch(merge)?;
        self.backend.builder.position_at_end(merge);
        Ok(())
    }

    fn emit_list_builtin(
        &self,
        builtin: Builtin,
        arguments: &[CallArgument],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        if let Some(result) = self.emit_native_int_list_builtin(builtin, arguments, destination)? {
            return Ok(result);
        }
        let values = if matches!(builtin, Builtin::ListLength | Builtin::ListGet)
            && let Some((CallArgument::Value(receiver), remaining)) = arguments.split_first()
            && let ExprKind::Copy(place) = &receiver.kind
            && remaining.iter().all(|argument| match argument {
                CallArgument::Value(value) => !expression_contains_await(value),
                CallArgument::InOut(_) => true,
            }) {
            // Preserve left-to-right value evaluation without cloning the complete list.
            // The snapshot keeps the old logical length/head if a later argument mutates the
            // source. No suspension may intervene because native stack slots are not GC roots.
            let snapshot = self.alloc_value("list.readonly.snapshot");
            self.shallow_copy(snapshot, self.place(place)?)?;
            let mut values = Vec::with_capacity(arguments.len());
            values.push(snapshot);
            for argument in remaining {
                match argument {
                    CallArgument::Value(expression) => {
                        let value = self.alloc_value("argument.value");
                        if !self.emit_expr(expression, value)? {
                            return Ok(false);
                        }
                        values.push(value);
                    }
                    CallArgument::InOut(place) => values.push(self.place(place)?),
                }
            }
            values
        } else {
            let Some(values) = self.emit_call_arguments(arguments)? else {
                return Ok(false);
            };
            values
        };
        self.emit_list_builtin_values(builtin, &values, destination)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_native_int_list_builtin(
        &self,
        builtin: Builtin,
        arguments: &[CallArgument],
        destination: PointerValue<'ctx>,
    ) -> Result<Option<bool>, CodegenError> {
        match (builtin, arguments) {
            (Builtin::ListAdd, [CallArgument::InOut(receiver), CallArgument::Value(value)])
                if receiver.projection.is_empty()
                    && self.native_int_lists.contains_key(&receiver.local) =>
            {
                let storage = self.native_int_lists[&receiver.local];
                // Method receiver evaluation precedes the element expression,
                // while the private header remains inaccessible to that
                // expression by construction of the native-storage plan.
                let element = self.alloc_value("int.list.add.value");
                if !self.emit_expr(value, element)? {
                    return Ok(Some(false));
                }
                let scalar = self.int_scalar(element)?;
                let length = self.backend.load_i64_field(
                    self.backend.int_list_type,
                    storage,
                    INT_LIST_FIELD_LENGTH,
                    "int.list.add.length",
                )?;
                let capacity = self.backend.load_i64_field(
                    self.backend.int_list_type,
                    storage,
                    INT_LIST_FIELD_CAPACITY,
                    "int.list.add.capacity",
                )?;
                let full = self
                    .backend
                    .builder
                    .build_int_compare(IntPredicate::EQ, length, capacity, "int.list.add.full")
                    .map_err(builder_error)?;
                let grow = self.append_block("int.list.add.grow");
                let ready = self.append_block("int.list.add.ready");
                self.backend
                    .builder
                    .build_conditional_branch(full, grow, ready)
                    .map_err(builder_error)?;

                self.backend.builder.position_at_end(grow);
                let minimum = self
                    .backend
                    .builder
                    .build_int_add(
                        length,
                        self.backend.i64_type.const_int(1, false),
                        "int.list.add.minimum",
                    )
                    .map_err(builder_error)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_int_list_reserve(),
                    &[storage.into(), minimum.into()],
                    "int.list.reserve",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "int.list.reserve.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "ListRuntimeFault")?;
                self.backend.branch(ready)?;

                self.backend.builder.position_at_end(ready);
                let data = self.backend.load_pointer_field(
                    self.backend.int_list_type,
                    storage,
                    INT_LIST_FIELD_DATA,
                    "int.list.add.data",
                )?;
                let length = self.backend.load_i64_field(
                    self.backend.int_list_type,
                    storage,
                    INT_LIST_FIELD_LENGTH,
                    "int.list.add.index",
                )?;
                // The backend supports only 64-bit targets. Integer-addressed
                // indexing avoids introducing unsafe Rust into the compiler;
                // the private header invariant proves the resulting address
                // lies within the runtime-owned allocation.
                let slot =
                    self.native_int_list_element_pointer(data, length, "int.list.add.slot")?;
                self.backend
                    .builder
                    .build_store(slot, scalar)
                    .map_err(builder_error)?;
                let next = self
                    .backend
                    .builder
                    .build_int_add(
                        length,
                        self.backend.i64_type.const_int(1, false),
                        "int.list.add.next.length",
                    )
                    .map_err(builder_error)?;
                self.backend.store_i64_field(
                    self.backend.int_list_type,
                    storage,
                    INT_LIST_FIELD_LENGTH,
                    next,
                )?;
                self.emit_constant(&Constant::Unit, destination)?;
                Ok(Some(true))
            }
            (Builtin::ListLength, [CallArgument::Value(receiver)])
                if matches!(
                    &receiver.kind,
                    ExprKind::Copy(place)
                        if place.projection.is_empty()
                            && self.native_int_lists.contains_key(&place.local)
                ) =>
            {
                let ExprKind::Copy(place) = &receiver.kind else {
                    unreachable!("shape checked above")
                };
                let length = self.backend.load_i64_field(
                    self.backend.int_list_type,
                    self.native_int_lists[&place.local],
                    INT_LIST_FIELD_LENGTH,
                    "int.list.length",
                )?;
                self.initialize(destination, VALUE_TAG_INT)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    length,
                )?;
                Ok(Some(true))
            }
            _ => Ok(None),
        }
    }

    fn emit_native_int_list_get_match(
        &self,
        matched: NativeIntListGetMatch<'_>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let index_value = self.alloc_value("int.list.get.index.value");
        if !self.emit_expr(matched.index, index_value)? {
            return Ok(false);
        }
        let index = self.int_scalar(index_value)?;
        let storage = self.native_int_lists[&matched.local];
        if matched.index_proven_in_bounds {
            // The closed-world native-storage plan proved that this is the
            // induction variable of a zero-based scan whose immutable end is
            // exactly the completed append-loop length. The `None` arm is
            // therefore unreachable on every path which reaches this load.
            return self.emit_native_int_list_some_arm(&matched, storage, index, destination);
        }
        let length = self.backend.load_i64_field(
            self.backend.int_list_type,
            storage,
            INT_LIST_FIELD_LENGTH,
            "int.list.get.length",
        )?;
        let negative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                index,
                self.backend.i64_type.const_zero(),
                "int.list.get.negative",
            )
            .map_err(builder_error)?;
        let past_end = self
            .backend
            .builder
            .build_int_compare(IntPredicate::SGE, index, length, "int.list.get.past.end")
            .map_err(builder_error)?;
        let out_of_bounds = self
            .backend
            .builder
            .build_or(negative, past_end, "int.list.get.out.of.bounds")
            .map_err(builder_error)?;
        let some = self.append_block("int.list.get.some");
        let none = self.append_block("int.list.get.none");
        let merge = self.append_block("int.list.get.merge");
        self.backend
            .builder
            .build_conditional_branch(out_of_bounds, none, some)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(some);
        let some_continues =
            self.emit_native_int_list_some_arm(&matched, storage, index, destination)?;
        if some_continues {
            self.backend.branch(merge)?;
        }

        self.backend.builder.position_at_end(none);
        let none_continues = self.emit_expr(&matched.none.value, destination)?;
        if none_continues {
            self.backend.branch(merge)?;
        }

        self.backend.builder.position_at_end(merge);
        if some_continues || none_continues {
            Ok(true)
        } else {
            self.backend
                .builder
                .build_unreachable()
                .map_err(builder_error)?;
            Ok(false)
        }
    }

    fn emit_native_int_list_some_arm(
        &self,
        matched: &NativeIntListGetMatch<'_>,
        storage: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let data = self.backend.load_pointer_field(
            self.backend.int_list_type,
            storage,
            INT_LIST_FIELD_DATA,
            "int.list.get.data",
        )?;
        // The checked predecessor or the exact-range plan establishes
        // `0 <= index < length`; the private runtime ABI additionally
        // guarantees `length <= capacity` for this storage.
        let slot = self.native_int_list_element_pointer(data, index, "int.list.get.slot")?;
        let scalar = self
            .backend
            .builder
            .build_load(self.backend.i64_type, slot, "int.list.get.value")
            .map_err(builder_error)?
            .into_int_value();
        if let Some(binding) = matched.some_binding {
            let binding = self.local(binding)?;
            self.initialize(binding, VALUE_TAG_INT)?;
            self.backend.store_i64_field(
                self.backend.value_type,
                binding,
                VALUE_FIELD_SCALAR,
                scalar,
            )?;
        }
        self.emit_expr(&matched.some.value, destination)
    }

    fn native_int_list_element_pointer(
        &self,
        data: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let base = self
            .backend
            .builder
            .build_ptr_to_int(data, self.backend.i64_type, &format!("{name}.base"))
            .map_err(builder_error)?;
        let offset = self
            .backend
            .builder
            .build_int_mul(
                index,
                self.backend.i64_type.const_int(8, false),
                &format!("{name}.offset"),
            )
            .map_err(builder_error)?;
        let address = self
            .backend
            .builder
            .build_int_add(base, offset, &format!("{name}.address"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_int_to_ptr(address, self.backend.ptr_type, name)
            .map_err(builder_error)
    }

    fn emit_list_builtin_values(
        &self,
        builtin: Builtin,
        values: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        match (builtin, values) {
            (Builtin::ListAdd, [list, value]) => {
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_list_add(),
                    &[(*list).into(), (*value).into()],
                    "list.add",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "list.add.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "ListRuntimeFault")?;
                self.emit_constant(&Constant::Unit, destination)?;
                Ok(true)
            }
            (Builtin::ListLength, [list]) => {
                let list = self.unwrap(*list)?;
                let length = self.backend.load_i64_field(
                    self.backend.value_type,
                    list,
                    VALUE_FIELD_AUX,
                    "list.length",
                )?;
                self.initialize(destination, VALUE_TAG_INT)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    length,
                )?;
                Ok(true)
            }
            (Builtin::ListGet, [list, index]) => {
                let list = self.unwrap(*list)?;
                let index = self.int_scalar(*index)?;
                let element = call_pointer(
                    &self.backend.builder,
                    self.backend.native_list_get(),
                    &[list.into(), index.into()],
                    "list.get",
                )?;
                let found = self
                    .backend
                    .builder
                    .build_is_not_null(element, "list.get.found")
                    .map_err(builder_error)?;
                let some = self.append_block("list.get.some");
                let none = self.append_block("list.get.none");
                let merge = self.append_block("list.get.merge");
                self.backend
                    .builder
                    .build_conditional_branch(found, some, none)
                    .map_err(builder_error)?;

                self.backend.builder.position_at_end(some);
                let owned = self.alloc_value("list.get.owned");
                self.clone_value(owned, element)?;
                self.emit_variant_from_pointers(TypeId(0), 1, &[owned], destination)?;
                self.backend.branch(merge)?;

                self.backend.builder.position_at_end(none);
                self.emit_variant_from_pointers(TypeId(0), 0, &[], destination)?;
                self.backend.branch(merge)?;

                self.backend.builder.position_at_end(merge);
                Ok(true)
            }
            _ => Err(CodegenError::new(
                "InvalidBuiltinCall",
                "List builtin argument shape does not match checked MIR",
            )),
        }
    }

    fn emit_process_builtin(
        &self,
        builtin: Builtin,
        values: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        match (builtin, values) {
            (Builtin::ProcessArguments, []) => {
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_process_arguments(),
                    &[destination.into()],
                    "process.arguments",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "process.arguments.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "ProcessRuntimeFault")?;
                Ok(true)
            }
            (Builtin::ProcessEnvironment, [name]) => {
                let (data, length) = self.text_parts(*name, "environment.name")?;
                let value = call_pointer(
                    &self.backend.builder,
                    self.backend.native_process_environment(),
                    &[data.into(), length.into()],
                    "environment.value",
                )?;
                let found = self
                    .backend
                    .builder
                    .build_is_not_null(value, "environment.found")
                    .map_err(builder_error)?;
                let some = self.append_block("environment.some");
                let none = self.append_block("environment.none");
                let merge = self.append_block("environment.merge");
                self.backend
                    .builder
                    .build_conditional_branch(found, some, none)
                    .map_err(builder_error)?;

                self.backend.builder.position_at_end(some);
                let text = self.alloc_value("environment.text");
                self.initialize(text, VALUE_TAG_TEXT)?;
                self.backend.store_pointer_field(
                    self.backend.value_type,
                    text,
                    VALUE_FIELD_DATA,
                    value,
                )?;
                self.emit_variant_from_pointers(TypeId(0), 1, &[text], destination)?;
                self.backend.branch(merge)?;

                self.backend.builder.position_at_end(none);
                self.emit_variant_from_pointers(TypeId(0), 0, &[], destination)?;
                self.backend.branch(merge)?;

                self.backend.builder.position_at_end(merge);
                Ok(true)
            }
            _ => Err(CodegenError::new(
                "InvalidBuiltinCall",
                "process builtin argument shape does not match checked MIR",
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn emit_standard_io_builtin(
        &self,
        builtin: Builtin,
        values: &[PointerValue<'ctx>],
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        match (builtin, values) {
            (Builtin::DurationMilliseconds, [value]) => {
                let milliseconds = self.int_scalar(*value)?;
                let negative = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::SLT,
                        milliseconds,
                        self.backend.i64_type.const_zero(),
                        "duration.negative",
                    )
                    .map_err(builder_error)?;
                self.fail_if(negative, "InvalidDuration")?;
                let ty =
                    self.backend.program.prelude.duration.ok_or_else(|| {
                        CodegenError::new("InvalidPrelude", "Duration is missing")
                    })?;
                self.emit_opaque_record(ty, *value, destination)?;
                Ok(true)
            }
            (Builtin::DurationAsMilliseconds, [duration]) => {
                let value = self.opaque_record_field(*duration, "duration.value")?;
                self.clone_value(destination, value)?;
                Ok(true)
            }
            (
                Builtin::FileOpenRead
                | Builtin::FileCreate
                | Builtin::FileOpenReadPath
                | Builtin::FileCreatePath
                | Builtin::FileTryOpenRead
                | Builtin::FileTryCreate
                | Builtin::FileTryOpenReadPath
                | Builtin::FileTryCreatePath,
                [path],
            ) => {
                let path = if matches!(
                    builtin,
                    Builtin::FileOpenReadPath
                        | Builtin::FileCreatePath
                        | Builtin::FileTryOpenReadPath
                        | Builtin::FileTryCreatePath
                ) {
                    self.opaque_record_field(*path, "file.path.payload")?
                } else {
                    *path
                };
                let (data, length) = self.text_parts(path, "file.path")?;
                let function = match builtin {
                    Builtin::FileOpenRead | Builtin::FileOpenReadPath => {
                        self.backend.native_file_open_read()
                    }
                    Builtin::FileCreate | Builtin::FileCreatePath => {
                        self.backend.native_file_create()
                    }
                    Builtin::FileTryOpenRead | Builtin::FileTryOpenReadPath => {
                        self.backend.native_file_try_open_read()
                    }
                    Builtin::FileTryCreate | Builtin::FileTryCreatePath => {
                        self.backend.native_file_try_create()
                    }
                    _ => unreachable!("matched file open/create builtin"),
                };
                let task = call_pointer(
                    &self.backend.builder,
                    function,
                    &[self.runtime_context.into(), data.into(), length.into()],
                    "file.task",
                )?;
                self.store_io_task(task, destination, "FileTaskAllocationFault")?;
                Ok(true)
            }
            (
                Builtin::FileReadText
                | Builtin::SocketReadText
                | Builtin::FileTryReadText
                | Builtin::SocketTryReadText,
                [resource],
            ) => {
                let descriptor = self.resource_descriptor(*resource)?;
                let function = match builtin {
                    Builtin::FileReadText => self.backend.native_file_read_text(),
                    Builtin::SocketReadText => self.backend.native_socket_read_text(),
                    Builtin::FileTryReadText => self.backend.native_file_try_read_text(),
                    Builtin::SocketTryReadText => self.backend.native_socket_try_read_text(),
                    _ => unreachable!("matched I/O read builtin"),
                };
                let task = call_pointer(
                    &self.backend.builder,
                    function,
                    &[self.runtime_context.into(), descriptor.into()],
                    "io.read.task",
                )?;
                self.store_io_task(task, destination, "IoTaskAllocationFault")?;
                Ok(true)
            }
            (
                Builtin::FileWriteText
                | Builtin::SocketWriteText
                | Builtin::FileTryWriteText
                | Builtin::SocketTryWriteText,
                [resource, text],
            ) => {
                let descriptor = self.resource_descriptor(*resource)?;
                let (data, length) = self.text_parts(*text, "io.write.text")?;
                let function = match builtin {
                    Builtin::FileWriteText => self.backend.native_file_write_text(),
                    Builtin::SocketWriteText => self.backend.native_socket_write_text(),
                    Builtin::FileTryWriteText => self.backend.native_file_try_write_text(),
                    Builtin::SocketTryWriteText => self.backend.native_socket_try_write_text(),
                    _ => unreachable!("matched I/O write builtin"),
                };
                let task = call_pointer(
                    &self.backend.builder,
                    function,
                    &[
                        self.runtime_context.into(),
                        descriptor.into(),
                        data.into(),
                        length.into(),
                    ],
                    "io.write.task",
                )?;
                self.store_io_task(task, destination, "IoTaskAllocationFault")?;
                Ok(true)
            }
            (Builtin::SocketConnect | Builtin::SocketTryConnect, [host, port]) => {
                let (data, length) = self.text_parts(*host, "socket.host")?;
                let port = self.int_scalar(*port)?;
                if builtin == Builtin::SocketConnect {
                    let negative = self
                        .backend
                        .builder
                        .build_int_compare(
                            IntPredicate::SLT,
                            port,
                            self.backend.i64_type.const_zero(),
                            "socket.port.negative",
                        )
                        .map_err(builder_error)?;
                    self.fail_if(negative, "InvalidPort")?;
                    let too_large = self
                        .backend
                        .builder
                        .build_int_compare(
                            IntPredicate::SGT,
                            port,
                            self.backend.i64_type.const_int(u64::from(u16::MAX), false),
                            "socket.port.large",
                        )
                        .map_err(builder_error)?;
                    self.fail_if(too_large, "InvalidPort")?;
                }
                let function = if builtin == Builtin::SocketTryConnect {
                    self.backend.native_socket_try_connect()
                } else {
                    self.backend.native_socket_connect()
                };
                let task = call_pointer(
                    &self.backend.builder,
                    function,
                    &[
                        self.runtime_context.into(),
                        data.into(),
                        length.into(),
                        port.into(),
                    ],
                    "socket.connect.task",
                )?;
                self.store_io_task(task, destination, "SocketTaskAllocationFault")?;
                Ok(true)
            }
            (Builtin::FileClose | Builtin::SocketClose, [resource]) => {
                let status = call_int(
                    &self.backend.builder,
                    self.backend.native_io_close(),
                    &[self.runtime_context.into(), (*resource).into()],
                    "io.close",
                )?;
                let invalid = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::NE,
                        status,
                        self.backend.context.i32_type().const_zero(),
                        "io.close.invalid",
                    )
                    .map_err(builder_error)?;
                self.fail_if(invalid, "ResourceCloseFault")?;
                self.emit_constant(&Constant::Unit, destination)?;
                Ok(true)
            }
            _ => Err(CodegenError::new(
                "InvalidBuiltinCall",
                "standard I/O builtin argument shape does not match checked MIR",
            )),
        }
    }

    fn emit_opaque_record(
        &self,
        ty: TypeId,
        value: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        self.initialize(destination, VALUE_TAG_RECORD)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_NOMINAL,
            self.backend.tag(u64::from(ty.0)),
        )?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(1),
        )?;
        let data = self.build_value_nodes(&[value])?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            data,
        )
    }

    fn opaque_record_field(
        &self,
        record: PointerValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let record = self.unwrap(record)?;
        let data = self.backend.load_pointer_field(
            self.backend.value_type,
            record,
            VALUE_FIELD_DATA,
            name,
        )?;
        let node = self.value_node_at(data, 0)?;
        self.backend.struct_pointer(
            self.backend.value_node_type,
            node,
            VALUE_NODE_FIELD_VALUE,
            name,
        )
    }

    fn resource_descriptor(
        &self,
        resource: PointerValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let value = self.opaque_record_field(resource, "resource.raw")?;
        self.int_scalar(value)
    }

    fn duration_scalar(
        &self,
        expression: &Expr,
        value: PointerValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        if matches!(expression.ty, Type::Int) {
            return self.int_scalar(value);
        }
        if let Type::Nominal(id, arguments) = &expression.ty
            && arguments.is_empty()
            && self.backend.program.prelude.duration == Some(*id)
        {
            let value = self.opaque_record_field(value, "duration.raw")?;
            return self.int_scalar(value);
        }
        Err(CodegenError::new(
            "InvalidDuration",
            "Task.sleep received neither Int nor Duration",
        ))
    }

    fn text_parts(
        &self,
        text: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let text = self.unwrap(text)?;
        let (_, data, length) = self.backend.sequence_parts(text, name)?;
        Ok((data, length))
    }

    fn store_io_task(
        &self,
        task: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
        fault: &str,
    ) -> Result<(), CodegenError> {
        let missing = self
            .backend
            .builder
            .build_is_null(task, "io.task.missing")
            .map_err(builder_error)?;
        self.fail_if(missing, fault)?;
        self.initialize(destination, VALUE_TAG_TASK)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_AUX,
            self.backend.tag(TASK_VALUE_DIRECT),
        )?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            task,
        )
    }

    fn emit_parse_float(
        &self,
        value: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let (data, length) = self.text_parts(value, "parse.float")?;
        let parsed = self.alloc_temporary(self.backend.context.f64_type(), "parse.output")?;
        let status = call_int(
            &self.backend.builder,
            self.backend.native_parse_float(),
            &[data.into(), length.into(), parsed.into()],
            "parse.float",
        )?;
        let success = self.append_block("parse.success");
        let failure = self.append_block("parse.failure");
        let invalid = self.append_block("parse.invalid");
        let out_of_range = self.append_block("parse.out_of_range");
        let merge = self.append_block("parse.merge");
        let ok = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_zero(),
                "parse.ok",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(ok, success, failure)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(success);
        let number = self
            .backend
            .builder
            .build_load(self.backend.context.f64_type(), parsed, "parse.value")
            .map_err(builder_error)?
            .into_float_value();
        let payload = self.alloc_value("parse.payload");
        self.store_float(payload, number)?;
        self.emit_result(true, payload, destination)?;
        self.backend.branch(merge)?;

        self.backend.builder.position_at_end(failure);
        let range_error = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_int(2, false),
                "parse.range",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(range_error, out_of_range, invalid)
            .map_err(builder_error)?;

        let error_type = self
            .backend
            .program
            .prelude
            .parse_float_error
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "ParseFloatError is missing"))?;
        for (block, variant) in [(invalid, 0), (out_of_range, 1)] {
            self.backend.builder.position_at_end(block);
            let error = self.alloc_value("parse.error");
            self.emit_variant_from_pointers(error_type, variant, &[], error)?;
            self.emit_result(false, error, destination)?;
            self.backend.branch(merge)?;
        }
        self.backend.builder.position_at_end(merge);
        Ok(true)
    }

    fn emit_parse_int(
        &self,
        value: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let (data, length) = self.text_parts(value, "parse.int")?;
        let parsed = self.alloc_temporary(self.backend.i64_type, "parse.int.output")?;
        let status = call_int(
            &self.backend.builder,
            self.backend.native_parse_int(),
            &[data.into(), length.into(), parsed.into()],
            "parse.int",
        )?;
        let success = self.append_block("parse.int.success");
        let failure = self.append_block("parse.int.failure");
        let invalid = self.append_block("parse.int.invalid");
        let out_of_range = self.append_block("parse.int.out_of_range");
        let merge = self.append_block("parse.int.merge");
        let ok = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_zero(),
                "parse.int.ok",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(ok, success, failure)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(success);
        let number = self
            .backend
            .builder
            .build_load(self.backend.i64_type, parsed, "parse.int.value")
            .map_err(builder_error)?
            .into_int_value();
        let payload = self.alloc_value("parse.int.payload");
        self.initialize(payload, VALUE_TAG_INT)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            payload,
            VALUE_FIELD_SCALAR,
            number,
        )?;
        self.emit_result(true, payload, destination)?;
        self.backend.branch(merge)?;

        self.backend.builder.position_at_end(failure);
        let range_error = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_int(2, false),
                "parse.int.range",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(range_error, out_of_range, invalid)
            .map_err(builder_error)?;

        let error_type = self
            .backend
            .program
            .prelude
            .parse_int_error
            .ok_or_else(|| CodegenError::new("InvalidPrelude", "ParseIntError is missing"))?;
        for (block, variant) in [(invalid, 0), (out_of_range, 1)] {
            self.backend.builder.position_at_end(block);
            let error = self.alloc_value("parse.int.error");
            self.emit_variant_from_pointers(error_type, variant, &[], error)?;
            self.emit_result(false, error, destination)?;
            self.backend.branch(merge)?;
        }
        self.backend.builder.position_at_end(merge);
        Ok(true)
    }

    fn emit_format_float(
        &self,
        value: PointerValue<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let number = self.float_scalar(value)?;
        let data_slot = self.alloc_temporary(self.backend.ptr_type, "format.data")?;
        let status = call_int(
            &self.backend.builder,
            self.backend.native_format_float(),
            &[number.into(), data_slot.into()],
            "format.float",
        )?;
        let failed = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                status,
                self.backend.context.i32_type().const_zero(),
                "format.failed",
            )
            .map_err(builder_error)?;
        self.fail_if(failed, "NativeFloatFormatFailure")?;
        let data = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, data_slot, "format.data.value")
            .map_err(builder_error)?
            .into_pointer_value();
        self.initialize(destination, VALUE_TAG_TEXT)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            data,
        )?;
        Ok(true)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_contract_expr(
        &self,
        expression: &ContractExpr,
        context: &ContractContext<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        match &expression.kind {
            ContractExprKind::Constant(constant) => {
                self.emit_constant(constant, destination)?;
                Ok(true)
            }
            ContractExprKind::Value(value) => {
                let value = Self::contract_value(*value, context)?;
                self.clone_value(destination, value.pointer)?;
                Ok(true)
            }
            ContractExprKind::Binding(index) => {
                let value = context.bindings.get(*index as usize).ok_or_else(|| {
                    CodegenError::new(
                        "InvalidContract",
                        format!("contract binding #{index} does not exist"),
                    )
                })?;
                self.clone_value(destination, value.pointer)?;
                Ok(true)
            }
            ContractExprKind::Field(value, field) => {
                let temporary = self.alloc_value("contract.field.base");
                if !self.emit_contract_expr(value, context, temporary)? {
                    return Ok(false);
                }
                let base_type = self.contract_expr_type(value, context)?;
                let projected = self.project_runtime_field(temporary, *field)?;
                let _field_type = self.projected_type(&base_type, *field)?;
                self.clone_value(destination, projected)?;
                Ok(true)
            }
            ContractExprKind::Unary(operator, value) => {
                let temporary = self.alloc_value("contract.unary");
                if !self.emit_contract_expr(value, context, temporary)? {
                    return Ok(false);
                }
                match operator {
                    UnaryOp::Not => {
                        let value = self.bool_value(temporary)?;
                        let value = self
                            .backend
                            .builder
                            .build_not(value, "contract.not")
                            .map_err(builder_error)?;
                        self.store_bool(destination, value)?;
                    }
                    UnaryOp::Negate => {
                        match self.numeric_kind(&self.contract_expr_type(value, context)?)? {
                            NumericKind::Int => {
                                let value = self.int_scalar(temporary)?;
                                let overflow = self
                                    .backend
                                    .builder
                                    .build_int_compare(
                                        IntPredicate::EQ,
                                        value,
                                        self.backend.signed_i64(i64::MIN),
                                        "contract.negate.overflow",
                                    )
                                    .map_err(builder_error)?;
                                self.fail_if(overflow, "IntegerOverflow")?;
                                let value = self
                                    .backend
                                    .builder
                                    .build_int_sub(
                                        self.backend.i64_type.const_zero(),
                                        value,
                                        "contract.negate",
                                    )
                                    .map_err(builder_error)?;
                                self.initialize(destination, VALUE_TAG_INT)?;
                                self.backend.store_i64_field(
                                    self.backend.value_type,
                                    destination,
                                    VALUE_FIELD_SCALAR,
                                    value,
                                )?;
                            }
                            NumericKind::Float => {
                                let value = self.float_scalar(temporary)?;
                                let value = self
                                    .backend
                                    .builder
                                    .build_float_neg(value, "contract.negate")
                                    .map_err(builder_error)?;
                                self.store_float(destination, value)?;
                            }
                        }
                    }
                }
                Ok(true)
            }
            ContractExprKind::Binary(operator, left, right) => {
                self.emit_contract_binary(*operator, left, right, context, destination)
            }
            ContractExprKind::IsFinite(value) => {
                let temporary = self.alloc_value("contract.finite");
                if !self.emit_contract_expr(value, context, temporary)? {
                    return Ok(false);
                }
                let number = self.float_scalar(temporary)?;
                let ordered = self
                    .backend
                    .builder
                    .build_float_compare(FloatPredicate::ORD, number, number, "finite.ordered")
                    .map_err(builder_error)?;
                let upper = self
                    .backend
                    .builder
                    .build_float_compare(
                        FloatPredicate::OLE,
                        number,
                        self.backend.context.f64_type().const_float(f64::MAX),
                        "finite.upper",
                    )
                    .map_err(builder_error)?;
                let lower = self
                    .backend
                    .builder
                    .build_float_compare(
                        FloatPredicate::OGE,
                        number,
                        self.backend.context.f64_type().const_float(-f64::MAX),
                        "finite.lower",
                    )
                    .map_err(builder_error)?;
                let bounded = self
                    .backend
                    .builder
                    .build_and(upper, lower, "finite.bounded")
                    .map_err(builder_error)?;
                let finite = self
                    .backend
                    .builder
                    .build_and(ordered, bounded, "finite")
                    .map_err(builder_error)?;
                self.store_bool(destination, finite)?;
                Ok(true)
            }
            ContractExprKind::Match { scrutinee, arms } => {
                self.emit_contract_match(scrutinee, arms, context, destination)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn emit_contract_binary(
        &self,
        operator: BinaryOp,
        left: &ContractExpr,
        right: &ContractExpr,
        context: &ContractContext<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        if matches!(operator, BinaryOp::And | BinaryOp::Or) {
            let left_value = self.alloc_value("contract.logical.left");
            if !self.emit_contract_expr(left, context, left_value)? {
                return Ok(false);
            }
            let condition = self.bool_value(left_value)?;
            let evaluate_right = self.append_block("contract.logical.right");
            let constant = self.append_block("contract.logical.constant");
            let merge = self.append_block("contract.logical.merge");
            let (true_block, false_block, constant_value) = if operator == BinaryOp::And {
                (evaluate_right, constant, false)
            } else {
                (constant, evaluate_right, true)
            };
            self.backend
                .builder
                .build_conditional_branch(condition, true_block, false_block)
                .map_err(builder_error)?;
            self.backend.builder.position_at_end(constant);
            self.emit_constant(&Constant::Bool(constant_value), destination)?;
            self.backend.branch(merge)?;
            self.backend.builder.position_at_end(evaluate_right);
            let continues = self.emit_contract_expr(right, context, destination)?;
            if continues {
                self.backend.branch(merge)?;
            }
            self.backend.builder.position_at_end(merge);
            return Ok(true);
        }

        let left_value = self.alloc_value("contract.left");
        if !self.emit_contract_expr(left, context, left_value)? {
            return Ok(false);
        }
        let right_value = self.alloc_value("contract.right");
        if !self.emit_contract_expr(right, context, right_value)? {
            return Ok(false);
        }
        let left_type = self.contract_expr_type(left, context)?;
        match operator {
            BinaryOp::Equal | BinaryOp::NotEqual => {
                let equal = self
                    .backend
                    .module
                    .get_function("loom.runtime.equal")
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "equal helper is missing"))?;
                let mut result = call_int(
                    &self.backend.builder,
                    equal,
                    &[left_value.into(), right_value.into()],
                    "contract.equal",
                )?;
                if operator == BinaryOp::NotEqual {
                    result = self
                        .backend
                        .builder
                        .build_not(result, "contract.not_equal")
                        .map_err(builder_error)?;
                }
                self.store_bool(destination, result)?;
            }
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                match self.numeric_kind(&left_type)? {
                    NumericKind::Int => {
                        let left = self.int_scalar(left_value)?;
                        let right = self.int_scalar(right_value)?;
                        let value = self.emit_checked_integer(operator, left, right)?;
                        self.initialize(destination, VALUE_TAG_INT)?;
                        self.backend.store_i64_field(
                            self.backend.value_type,
                            destination,
                            VALUE_FIELD_SCALAR,
                            value,
                        )?;
                    }
                    NumericKind::Float => {
                        let left = self.float_scalar(left_value)?;
                        let right = self.float_scalar(right_value)?;
                        let value = match operator {
                            BinaryOp::Add => {
                                self.backend
                                    .builder
                                    .build_float_add(left, right, "contract.add")
                            }
                            BinaryOp::Subtract => {
                                self.backend
                                    .builder
                                    .build_float_sub(left, right, "contract.sub")
                            }
                            BinaryOp::Multiply => {
                                self.backend
                                    .builder
                                    .build_float_mul(left, right, "contract.mul")
                            }
                            BinaryOp::Divide => {
                                self.backend
                                    .builder
                                    .build_float_div(left, right, "contract.div")
                            }
                            _ => unreachable!(),
                        }
                        .map_err(builder_error)?;
                        self.store_float(destination, value)?;
                    }
                }
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual => {
                let result = match self.numeric_kind(&left_type)? {
                    NumericKind::Int => {
                        let left = self.int_scalar(left_value)?;
                        let right = self.int_scalar(right_value)?;
                        let predicate = match operator {
                            BinaryOp::Less => IntPredicate::SLT,
                            BinaryOp::LessEqual => IntPredicate::SLE,
                            BinaryOp::Greater => IntPredicate::SGT,
                            BinaryOp::GreaterEqual => IntPredicate::SGE,
                            _ => unreachable!(),
                        };
                        self.backend
                            .builder
                            .build_int_compare(predicate, left, right, "contract.compare")
                            .map_err(builder_error)?
                    }
                    NumericKind::Float => {
                        let left = self.float_scalar(left_value)?;
                        let right = self.float_scalar(right_value)?;
                        let predicate = match operator {
                            BinaryOp::Less => FloatPredicate::OLT,
                            BinaryOp::LessEqual => FloatPredicate::OLE,
                            BinaryOp::Greater => FloatPredicate::OGT,
                            BinaryOp::GreaterEqual => FloatPredicate::OGE,
                            _ => unreachable!(),
                        };
                        self.backend
                            .builder
                            .build_float_compare(predicate, left, right, "contract.compare")
                            .map_err(builder_error)?
                    }
                };
                self.store_bool(destination, result)?;
            }
            BinaryOp::And | BinaryOp::Or => unreachable!(),
        }
        Ok(true)
    }

    fn emit_contract_match(
        &self,
        scrutinee: &ContractExpr,
        arms: &[ContractArm],
        context: &ContractContext<'ctx>,
        destination: PointerValue<'ctx>,
    ) -> Result<bool, CodegenError> {
        let value = self.alloc_value("contract.match.value");
        if !self.emit_contract_expr(scrutinee, context, value)? {
            return Ok(false);
        }
        let merge = self.append_block("contract.match.merge");
        let mut test_block = self.backend.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "contract match has no insertion block")
        })?;
        let mut any_continues = false;
        for arm in arms {
            self.backend.builder.position_at_end(test_block);
            let selected = self.append_block("contract.match.arm");
            let next = self.append_block("contract.match.next");
            let mut binding_values = Vec::new();
            self.emit_pattern_branch(&arm.pattern, value, selected, next, &mut binding_values)?;
            self.backend.builder.position_at_end(selected);
            if binding_values.len() != arm.bindings.len() {
                return Err(CodegenError::new(
                    "InvalidContract",
                    "contract pattern binding count is inconsistent",
                ));
            }
            let mut nested = context.clone();
            nested
                .bindings
                .extend(
                    binding_values
                        .into_iter()
                        .zip(&arm.bindings)
                        .map(|(pointer, ty)| TypedPointer {
                            pointer,
                            ty: ty.clone(),
                        }),
                );
            let continues = self.emit_contract_expr(&arm.value, &nested, destination)?;
            if continues {
                any_continues = true;
                self.backend.branch(merge)?;
            }
            test_block = next;
        }
        self.backend.builder.position_at_end(test_block);
        self.backend.puts("InvalidContractMatch")?;
        self.emit_all_cleanups()?;
        self.emit_status_return(self.failure_status(), None)?;
        self.backend.builder.position_at_end(merge);
        if any_continues {
            Ok(true)
        } else {
            self.backend
                .builder
                .build_unreachable()
                .map_err(builder_error)?;
            Ok(false)
        }
    }

    fn contract_value(
        value: ContractValue,
        context: &ContractContext<'ctx>,
    ) -> Result<TypedPointer<'ctx>, CodegenError> {
        let selected = match value {
            ContractValue::SelfValue => context.receiver.as_ref(),
            ContractValue::Result => context.result.as_ref(),
            ContractValue::Argument(index) => context.arguments.get(index as usize),
            ContractValue::OldSelf => context.old_receiver.as_ref(),
            ContractValue::OldArgument(index) => context
                .old_arguments
                .get(index as usize)
                .and_then(Option::as_ref),
        };
        selected.cloned().ok_or_else(|| {
            CodegenError::new(
                "InvalidContract",
                format!("contract value {value:?} is unavailable"),
            )
        })
    }

    fn contract_expr_type(
        &self,
        expression: &ContractExpr,
        context: &ContractContext<'ctx>,
    ) -> Result<Type, CodegenError> {
        match &expression.kind {
            ContractExprKind::Constant(constant) => Ok(match constant {
                Constant::Unit => Type::Unit,
                Constant::Bool(_) => Type::Bool,
                Constant::Int(_) => Type::Int,
                Constant::Float(_) => Type::Float,
                Constant::Text(_) => Type::Text,
            }),
            ContractExprKind::Value(value) => Ok(Self::contract_value(*value, context)?.ty),
            ContractExprKind::Binding(index) => context
                .bindings
                .get(*index as usize)
                .map(|binding| binding.ty.clone())
                .ok_or_else(|| {
                    CodegenError::new("InvalidContract", "contract binding type is unavailable")
                }),
            ContractExprKind::Field(value, field) => {
                let base = self.contract_expr_type(value, context)?;
                self.projected_type(&base, *field)
            }
            ContractExprKind::Unary(UnaryOp::Not, _)
            | ContractExprKind::Binary(
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
                | BinaryOp::And
                | BinaryOp::Or,
                _,
                _,
            )
            | ContractExprKind::IsFinite(_)
            | ContractExprKind::Match { .. } => Ok(Type::Bool),
            ContractExprKind::Unary(UnaryOp::Negate, value)
            | ContractExprKind::Binary(
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide,
                value,
                _,
            ) => self.contract_expr_type(value, context),
        }
    }

    fn project_runtime_field(
        &self,
        value: PointerValue<'ctx>,
        field: u32,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let value = self.unwrap(value)?;
        let data = self.backend.load_pointer_field(
            self.backend.value_type,
            value,
            VALUE_FIELD_DATA,
            "contract.record.data",
        )?;
        let node = self.value_node_at(data, field)?;
        self.backend.struct_pointer(
            self.backend.value_node_type,
            node,
            VALUE_NODE_FIELD_VALUE,
            "contract.field",
        )
    }

    fn projected_type(&self, ty: &Type, field: u32) -> Result<Type, CodegenError> {
        match ty {
            Type::Nominal(id, arguments) => {
                let definition = self.backend.program.type_def(*id).ok_or_else(|| {
                    CodegenError::new(
                        "InvalidTypeReference",
                        format!("type #{} does not exist", id.0),
                    )
                })?;
                match &definition.kind {
                    TypeDefKind::Record { fields, .. } => fields
                        .get(field as usize)
                        .map(|field| substitute_type(&field.ty, arguments))
                        .ok_or_else(|| {
                            CodegenError::new("InvalidField", "record field is out of bounds")
                        }),
                    TypeDefKind::Refined { base, .. } => self.projected_type(base, field),
                    TypeDefKind::Enum { .. } => {
                        Err(CodegenError::new("InvalidField", "enum has no fields"))
                    }
                }
            }
            _ => Err(CodegenError::new(
                "InvalidField",
                "field projection targets a non-record",
            )),
        }
    }

    fn emit_constant(
        &self,
        constant: &Constant,
        destination: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        match constant {
            Constant::Unit => self.initialize(destination, VALUE_TAG_UNIT),
            Constant::Bool(value) => {
                self.initialize(destination, VALUE_TAG_BOOL)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    self.backend.tag(u64::from(*value)),
                )
            }
            Constant::Int(value) => {
                self.initialize(destination, VALUE_TAG_INT)?;
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    self.backend.signed_i64(*value),
                )
            }
            Constant::Float(value) => {
                self.initialize(destination, VALUE_TAG_FLOAT)?;
                let bits = self
                    .backend
                    .builder
                    .build_bit_cast(
                        self.backend.context.f64_type().const_float(*value),
                        self.backend.i64_type,
                        "float.bits",
                    )
                    .map_err(builder_error)?
                    .into_int_value();
                self.backend.store_i64_field(
                    self.backend.value_type,
                    destination,
                    VALUE_FIELD_SCALAR,
                    bits,
                )
            }
            Constant::Text(value) => self.emit_text_constant(value, destination),
        }
    }

    fn emit_text_constant(
        &self,
        value: &str,
        destination: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let byte_length = u64::try_from(value.len()).map_err(|_| {
            CodegenError::new("TextLiteralTooLarge", "Text literal length exceeds u64")
        })?;
        let array_length = u32::try_from(value.len()).map_err(|_| {
            CodegenError::new(
                "TextLiteralTooLarge",
                "Text literal exceeds LLVM's constant array limit",
            )
        })?;
        let allocation_size = TEXT_OBJECT_HEADER_SIZE
            .checked_add(byte_length)
            .ok_or_else(|| {
                CodegenError::new(
                    "TextLiteralTooLarge",
                    "Text literal allocation size overflowed",
                )
            })?;
        let scalar_length = u64::try_from(value.chars().count()).map_err(|_| {
            CodegenError::new("TextLiteralTooLarge", "Text scalar count exceeds u64")
        })?;
        let bytes_type = self.backend.context.i8_type().array_type(array_length);
        let literal_type = self.backend.context.struct_type(
            &[
                self.backend.ptr_type.into(),
                self.backend.i64_type.into(),
                self.backend.i64_type.into(),
                self.backend.i64_type.into(),
                bytes_type.into(),
            ],
            false,
        );
        let bytes = value
            .as_bytes()
            .iter()
            .map(|byte| {
                self.backend
                    .context
                    .i8_type()
                    .const_int(u64::from(*byte), false)
            })
            .collect::<Vec<_>>();
        let initializer = literal_type.const_named_struct(&[
            self.backend.text_layout.as_pointer_value().into(),
            self.backend
                .i64_type
                .const_int(allocation_size, false)
                .into(),
            self.backend.i64_type.const_int(byte_length, false).into(),
            self.backend.i64_type.const_int(scalar_length, false).into(),
            self.backend.context.i8_type().const_array(&bytes).into(),
        ]);
        let literal =
            self.backend
                .module
                .add_global(literal_type, None, &self.backend.unique("text.object"));
        literal.set_initializer(&initializer);
        literal.set_constant(true);
        literal.set_linkage(Linkage::Private);
        literal.set_unnamed_address(UnnamedAddress::Global);
        literal.set_alignment(u32::try_from(TEXT_OBJECT_ALIGNMENT).unwrap_or(8));
        self.initialize(destination, VALUE_TAG_TEXT)?;
        self.backend.store_pointer_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_DATA,
            literal.as_pointer_value(),
        )
    }

    fn initialize(&self, destination: PointerValue<'ctx>, tag: u64) -> Result<(), CodegenError> {
        self.backend
            .builder
            .build_store(destination, self.backend.value_type.const_zero())
            .map_err(builder_error)?;
        self.backend.store_i64_field(
            self.backend.value_type,
            destination,
            VALUE_FIELD_TAG,
            self.backend.tag(tag),
        )
    }

    fn clone_value(
        &self,
        destination: PointerValue<'ctx>,
        source: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let clone = self
            .backend
            .module
            .get_function("loom.runtime.clone")
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "clone helper is missing"))?;
        self.backend
            .builder
            .build_call(clone, &[destination.into(), source.into()], "clone")
            .map_err(builder_error)?;
        Ok(())
    }

    fn shallow_copy(
        &self,
        destination: PointerValue<'ctx>,
        source: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let value = self
            .backend
            .builder
            .build_load(self.backend.value_type, source, "move")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(destination, value)
            .map_err(builder_error)?;
        Ok(())
    }

    fn alloc_entry<T: BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let builder = self.backend.context.create_builder();
        let entry = self
            .function
            .get_first_basic_block()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "function has no entry block"))?;
        if let Some(instruction) = entry.get_first_instruction() {
            builder.position_before(&instruction);
        } else {
            builder.position_at_end(entry);
        }
        builder
            .build_alloca(ty, &self.backend.unique(name))
            .map_err(builder_error)
    }

    fn alloc_temporary<T: BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        if self.task.is_some() || self.loop_depth.get() > 0 {
            return self.alloc_entry(ty, name);
        }
        self.backend
            .builder
            .build_alloca(ty, &self.backend.unique(name))
            .map_err(builder_error)
    }

    fn alloc_value(&self, name: &str) -> PointerValue<'ctx> {
        self.alloc_temporary(self.backend.value_type, name)
            .expect("checked function accepts temporary allocation")
    }

    fn local(&self, local: LocalId) -> Result<PointerValue<'ctx>, CodegenError> {
        self.locals.get(&local).copied().ok_or_else(|| {
            CodegenError::new(
                "InvalidLocalReference",
                format!("local #{} does not exist", local.0),
            )
        })
    }

    fn place(&self, place: &Place) -> Result<PointerValue<'ctx>, CodegenError> {
        let mut current = self.local(place.local)?;
        let mut current_type = self.local_type(place.local);
        for field in &place.projection {
            if !current_type
                .as_ref()
                .is_some_and(|ty| self.is_static_record(ty))
            {
                current = self.unwrap(current)?;
            }
            let data = self.backend.load_pointer_field(
                self.backend.value_type,
                current,
                VALUE_FIELD_DATA,
                "record.data",
            )?;
            let node = self.value_node_at(data, *field)?;
            current = self.backend.struct_pointer(
                self.backend.value_node_type,
                node,
                VALUE_NODE_FIELD_VALUE,
                "field.value",
            )?;
            current_type = current_type
                .as_ref()
                .and_then(|ty| self.projected_type(ty, *field).ok());
        }
        Ok(current)
    }

    fn local_type(&self, local: LocalId) -> Option<Type> {
        self.source
            .params
            .iter()
            .chain(&self.source.locals)
            .find(|declaration| declaration.id == local)
            .map(|declaration| declaration.ty.clone())
    }

    fn static_place_type(&self, place: &Place) -> Option<Type> {
        let mut ty = self.local_type(place.local)?;
        for field in &place.projection {
            ty = self.projected_type(&ty, *field).ok()?;
        }
        Some(ty)
    }

    fn is_static_record(&self, ty: &Type) -> bool {
        let Type::Nominal(id, _) = ty else {
            return false;
        };
        self.backend
            .program
            .type_def(*id)
            .is_some_and(|definition| matches!(&definition.kind, TypeDefKind::Record { .. }))
    }

    fn value_node_at(
        &self,
        mut node: PointerValue<'ctx>,
        index: u32,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        for _ in 0..index {
            node = self.backend.load_pointer_field(
                self.backend.value_node_type,
                node,
                VALUE_NODE_FIELD_NEXT,
                "node.next",
            )?;
        }
        Ok(node)
    }

    fn unwrap(&self, value: PointerValue<'ctx>) -> Result<PointerValue<'ctx>, CodegenError> {
        let function = self
            .backend
            .module
            .get_function("loom.runtime.unwrap")
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "unwrap helper is missing"))?;
        call_pointer(&self.backend.builder, function, &[value.into()], "unwrap")
    }

    fn bool_value(&self, value: PointerValue<'ctx>) -> Result<IntValue<'ctx>, CodegenError> {
        let value = self.unwrap(value)?;
        let scalar = self.backend.load_i64_field(
            self.backend.value_type,
            value,
            VALUE_FIELD_SCALAR,
            "bool.scalar",
        )?;
        self.backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                scalar,
                self.backend.i64_type.const_zero(),
                "bool.value",
            )
            .map_err(builder_error)
    }

    fn append_block(&self, name: &str) -> inkwell::basic_block::BasicBlock<'ctx> {
        self.backend
            .context
            .append_basic_block(self.function, &self.backend.unique(name))
    }

    fn current_block_terminated(&self) -> bool {
        self.backend
            .builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_terminator)
            .is_some()
    }
}

#[derive(Clone)]
struct TypedPointer<'ctx> {
    pointer: PointerValue<'ctx>,
    ty: Type,
}

#[derive(Clone)]
struct ContractContext<'ctx> {
    receiver: Option<TypedPointer<'ctx>>,
    result: Option<TypedPointer<'ctx>>,
    arguments: Vec<TypedPointer<'ctx>>,
    old_receiver: Option<TypedPointer<'ctx>>,
    old_arguments: Vec<Option<TypedPointer<'ctx>>>,
    bindings: Vec<TypedPointer<'ctx>>,
}

#[derive(Clone, Copy)]
enum NumericKind {
    Int,
    Float,
}

fn build_weighted_conditional_branch<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    condition: IntValue<'ctx>,
    then_block: BasicBlock<'ctx>,
    else_block: BasicBlock<'ctx>,
    likely: LikelyBranch,
) -> Result<(), CodegenError> {
    let branch = builder
        .build_conditional_branch(condition, then_block, else_block)
        .map_err(builder_error)?;
    let (then_weight, else_weight) = match likely {
        LikelyBranch::Then => (LIKELY_BRANCH_WEIGHT, UNLIKELY_BRANCH_WEIGHT),
        LikelyBranch::Else => (UNLIKELY_BRANCH_WEIGHT, LIKELY_BRANCH_WEIGHT),
    };
    let weights = context.metadata_node(&[
        context.metadata_string("branch_weights").into(),
        context.i32_type().const_int(then_weight, false).into(),
        context.i32_type().const_int(else_weight, false).into(),
    ]);
    branch
        .set_metadata(weights, context.get_kind_id("prof"))
        .map_err(|error| CodegenError::new("LlvmMetadataFailed", error.to_string()))?;
    Ok(())
}

fn mark_cold_noinline(context: &Context, function: FunctionValue<'_>) -> Result<(), CodegenError> {
    for name in ["cold", "noinline"] {
        let kind = Attribute::get_named_enum_kind_id(name);
        if kind == 0 {
            return Err(CodegenError::new(
                "LlvmAttributeUnavailable",
                format!("LLVM does not provide the `{name}` function attribute"),
            ));
        }
        function.add_attribute(
            AttributeLoc::Function,
            context.create_enum_attribute(kind, 0),
        );
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)] // `Result::map_err` passes it by value.
fn builder_error(error: inkwell::builder::BuilderError) -> CodegenError {
    CodegenError::new("LlvmBuilderFailed", error.to_string())
}

fn parameter_pointer(
    function: FunctionValue<'_>,
    index: u32,
) -> Result<PointerValue<'_>, CodegenError> {
    function
        .get_nth_param(index)
        .map(BasicValueEnum::into_pointer_value)
        .ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("function is missing pointer parameter {index}"),
            )
        })
}

fn parameter_int(function: FunctionValue<'_>, index: u32) -> Result<IntValue<'_>, CodegenError> {
    function
        .get_nth_param(index)
        .map(BasicValueEnum::into_int_value)
        .ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("function is missing integer parameter {index}"),
            )
        })
}

fn call_pointer<'ctx>(
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<PointerValue<'ctx>, CodegenError> {
    builder
        .build_call(function, arguments, name)
        .map_err(builder_error)?
        .try_as_basic_value()
        .basic()
        .map(BasicValueEnum::into_pointer_value)
        .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "call did not return a pointer"))
}

fn call_int<'ctx>(
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<IntValue<'ctx>, CodegenError> {
    builder
        .build_call(function, arguments, name)
        .map_err(builder_error)?
        .try_as_basic_value()
        .basic()
        .map(BasicValueEnum::into_int_value)
        .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "call did not return an integer"))
}

fn call_native_status<'ctx>(
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    signature: &NativeSignature,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
    if signature.effect() != NativeEffectAbi::RuntimeStatus {
        return Err(CodegenError::new(
            "LlvmAbiDefect",
            "a native status call requires the runtime-status effect ABI",
        ));
    }
    let aggregate = builder
        .build_call(function, arguments, name)
        .map_err(builder_error)?
        .try_as_basic_value()
        .basic()
        .map(BasicValueEnum::into_struct_value)
        .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "native call returned no status pair"))?;
    let status = builder
        .build_extract_value(aggregate, 0, &format!("{name}.status"))
        .map_err(builder_error)?
        .into_int_value();
    let value = builder
        .build_extract_value(aggregate, 1, &format!("{name}.value"))
        .map_err(builder_error)?;
    let value = match signature.shape().result() {
        NativeLayout::Scalar(NativeScalar::Int) => value.into_int_value(),
    };
    Ok((status, value))
}

fn concrete_witness_id(reference: &WitnessRef) -> Option<WitnessId> {
    match reference {
        WitnessRef::Concrete(witness) | WitnessRef::Apply { witness, .. } => Some(*witness),
        WitnessRef::Parameter(_) => None,
    }
}

fn join_result_shape(mode: TaskJoinMode, task_ty: &Type) -> u64 {
    let output = match task_ty {
        Type::Task(output) => output.as_ref(),
        other => other,
    };
    match mode {
        TaskJoinMode::Race => JOIN_RESULT_OUTCOME,
        TaskJoinMode::Any => JOIN_RESULT_SCALAR,
        TaskJoinMode::Settled => match output {
            Type::Tuple(_) => JOIN_RESULT_OUTCOME_TUPLE,
            Type::List(_) => JOIN_RESULT_OUTCOME_LIST,
            _ => JOIN_RESULT_OUTCOME,
        },
        TaskJoinMode::All => match output {
            Type::Tuple(_) => JOIN_RESULT_TUPLE,
            Type::List(_) => JOIN_RESULT_LIST,
            _ => JOIN_RESULT_SCALAR,
        },
    }
}

fn substitute_type(ty: &Type, arguments: &[Type]) -> Type {
    match ty {
        Type::Parameter(index) => arguments
            .get(*index as usize)
            .cloned()
            .unwrap_or(Type::Error),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute_type(element, arguments))
                .collect(),
        ),
        Type::List(element) => Type::List(Box::new(substitute_type(element, arguments))),
        Type::Nominal(id, nested) => Type::Nominal(
            *id,
            nested
                .iter()
                .map(|nested| substitute_type(nested, arguments))
                .collect(),
        ),
        Type::Task(output) => Type::Task(Box::new(substitute_type(output, arguments))),
        Type::TaskOutcome(output) => {
            Type::TaskOutcome(Box::new(substitute_type(output, arguments)))
        }
        Type::View {
            mutable,
            concept,
            bindings,
        } => Type::View {
            mutable: *mutable,
            concept: *concept,
            bindings: bindings
                .iter()
                .map(|(name, ty)| (name.clone(), substitute_type(ty, arguments)))
                .collect(),
        },
        Type::Never => Type::Never,
        Type::Unit => Type::Unit,
        Type::Bool => Type::Bool,
        Type::Int => Type::Int,
        Type::Float => Type::Float,
        Type::Text => Type::Text,
        Type::AssociatedProjection {
            witness,
            associated,
        } => Type::AssociatedProjection {
            witness: *witness,
            associated: associated.clone(),
        },
        Type::Error => Type::Error,
    }
}

fn mangle(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}
