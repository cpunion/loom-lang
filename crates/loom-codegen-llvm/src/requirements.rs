use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, Expr, ExprKind, Function, FunctionId,
    Program, StatementKind, Type, TypeDefKind, UnaryOp, WitnessRef,
};

use crate::native_layout::NativeLayout;
use crate::native_range::NativeIntRangePlan;
use crate::native_storage::{NativeIntListPlan, NativeStackRecordPlan};
use crate::{CodegenError, ReachableProgram};

/// Compiler-private runtime capabilities needed by one lowered callable.
///
/// This is deliberately not a source-language effect system. The bits describe
/// the currently selected native representation and may become smaller as
/// typed lowering replaces universal `Value` operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimeRequirements(u8);

impl RuntimeRequirements {
    const MAY_FAULT_BIT: u8 = 1;
    const MAY_COLLECT_BIT: u8 = 1 << 1;
    const NEEDS_EXECUTOR_BIT: u8 = 1 << 2;

    pub(crate) const NONE: Self = Self(0);
    pub(crate) const MAY_FAULT: Self = Self(Self::MAY_FAULT_BIT);
    /// The callable can enter a moving-GC collection boundary. This bit alone
    /// controls shadow-root frames and root-state publication; native
    /// allocations which cannot collect do not set it.
    pub(crate) const MAY_COLLECT: Self = Self(Self::MAY_COLLECT_BIT);
    pub(crate) const NEEDS_EXECUTOR: Self = Self(Self::NEEDS_EXECUTOR_BIT);
    /// Task construction/resumption can clone captured Values and materialize
    /// managed completion/outcome aggregates, so it remains a collection
    /// boundary in addition to needing an executor and fault propagation.
    pub(crate) const ASYNC: Self = Self::MAY_FAULT
        .union(Self::MAY_COLLECT)
        .union(Self::NEEDS_EXECUTOR);

    #[must_use]
    pub(crate) const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) fn include(&mut self, other: Self) {
        self.0 |= other.0;
    }

    #[must_use]
    pub(crate) const fn is_pure_no_fault(self) -> bool {
        self.0 == 0
    }

    #[cfg(test)]
    #[must_use]
    const fn may_fault(self) -> bool {
        self.0 & Self::MAY_FAULT_BIT != 0
    }

    #[must_use]
    pub(crate) const fn may_collect(self) -> bool {
        self.0 & Self::MAY_COLLECT_BIT != 0
    }

    #[must_use]
    pub(crate) const fn needs_executor(self) -> bool {
        self.0 & Self::NEEDS_EXECUTOR_BIT != 0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FunctionRequirements {
    /// Requirements observed when source code invokes this function. For an
    /// async function this is the constructor, not the deferred resume body.
    pub(crate) invocation: RuntimeRequirements,
    /// Requirements of the synchronous body or generated async resume body.
    pub(crate) body: RuntimeRequirements,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeRequirementGraph {
    functions: BTreeMap<FunctionId, FunctionRequirements>,
}

#[derive(Clone, Debug, Default)]
struct LocalRequirements {
    requirements: RuntimeRequirements,
    callees: BTreeSet<FunctionId>,
}

struct RequirementScanner<'a> {
    program: &'a Program,
    reachable: &'a ReachableProgram,
    function: &'a Function,
    int_ranges: &'a NativeIntRangePlan,
    native_int_lists: NativeIntListPlan,
    stack_records: &'a NativeStackRecordPlan,
}

impl RuntimeRequirementGraph {
    pub(crate) fn analyze(
        program: &Program,
        reachable: &ReachableProgram,
        int_ranges: &NativeIntRangePlan,
        stack_record_plans: &BTreeMap<FunctionId, NativeStackRecordPlan>,
    ) -> Result<Self, CodegenError> {
        let empty_stack_records = NativeStackRecordPlan::default();
        let mut local = BTreeMap::new();
        for id in &reachable.functions {
            let function = program.function(*id).ok_or_else(|| {
                CodegenError::new(
                    "InvalidFunctionReference",
                    format!("reachable function #{} does not exist", id.0),
                )
            })?;
            let mut requirements = LocalRequirements::default();
            if function.call_plan.receiver_invariant.is_some()
                || !function.call_plan.requires.is_empty()
                || !function.call_plan.ensures.is_empty()
            {
                requirements.requirements.include(
                    RuntimeRequirements::MAY_FAULT.union(RuntimeRequirements::MAY_COLLECT),
                );
            }
            let stack_records = stack_record_plans.get(id).unwrap_or(&empty_stack_records);
            RequirementScanner {
                program,
                reachable,
                function,
                int_ranges,
                native_int_lists: NativeIntListPlan::analyze(program, function),
                stack_records,
            }
            .scan_block(&function.body, &mut requirements)?;
            local.insert(*id, requirements);
        }

        let mut functions = local
            .iter()
            .map(|(id, local)| {
                let asynchronous = program
                    .function(*id)
                    .is_some_and(|function| function.is_async);
                let body = if asynchronous {
                    local.requirements.union(RuntimeRequirements::ASYNC)
                } else {
                    local.requirements
                };
                let invocation = if asynchronous {
                    RuntimeRequirements::ASYNC
                } else {
                    body
                };
                (*id, FunctionRequirements { invocation, body })
            })
            .collect::<BTreeMap<_, _>>();

        loop {
            let previous = functions.clone();
            let mut changed = false;
            for (id, local) in &local {
                let asynchronous = program
                    .function(*id)
                    .is_some_and(|function| function.is_async);
                let mut body = local.requirements;
                if asynchronous {
                    body.include(RuntimeRequirements::ASYNC);
                }
                for callee in &local.callees {
                    let callee = previous.get(callee).ok_or_else(|| {
                        CodegenError::new(
                            "ReachabilityDefect",
                            format!(
                                "runtime-requirement edge to function #{} is not live",
                                callee.0
                            ),
                        )
                    })?;
                    body.include(callee.invocation);
                }
                let invocation = if asynchronous {
                    RuntimeRequirements::ASYNC
                } else {
                    body
                };
                let next = FunctionRequirements { invocation, body };
                changed |= functions.insert(*id, next) != Some(next);
            }
            if !changed {
                break;
            }
        }

        Ok(Self { functions })
    }

    pub(crate) fn function(&self, id: FunctionId) -> Result<FunctionRequirements, CodegenError> {
        self.functions.get(&id).copied().ok_or_else(|| {
            CodegenError::new(
                "ReachabilityDefect",
                format!("function #{} has no runtime-requirement summary", id.0),
            )
        })
    }
}

impl RequirementScanner<'_> {
    fn scan_block(
        &self,
        block: &Block,
        output: &mut LocalRequirements,
    ) -> Result<(), CodegenError> {
        self.scan_block_statements(block, output)?;
        if let Some(tail) = &block.tail {
            self.scan_expr(tail, output)?;
        }
        Ok(())
    }

    fn scan_block_statements(
        &self,
        block: &Block,
        output: &mut LocalRequirements,
    ) -> Result<(), CodegenError> {
        for statement in &block.statements {
            match &statement.kind {
                StatementKind::Let { local, value } => {
                    if is_native_int_list_initializer(&self.native_int_lists, *local, value) {
                        continue;
                    }
                    if self.stack_records.is_initializer(*local, value) {
                        self.scan_planned_stack_record_initializer(value, output)?;
                    } else {
                        self.scan_expr(value, output)?;
                    }
                }
                StatementKind::Assign { value, .. } | StatementKind::Evaluate(value) => {
                    self.scan_expr(value, output)?;
                }
                StatementKind::LetTuple { value, .. } => {
                    output
                        .requirements
                        .include(RuntimeRequirements::MAY_COLLECT);
                    self.scan_expr(value, output)?;
                }
                StatementKind::ForRange {
                    start, end, body, ..
                } => {
                    self.scan_expr(start, output)?;
                    self.scan_expr(end, output)?;
                    self.scan_block(body, output)?;
                }
                StatementKind::Assert { condition } => {
                    output.requirements.include(RuntimeRequirements::MAY_FAULT);
                    self.scan_expr(condition, output)?;
                }
                StatementKind::Defer(cleanup) => {
                    self.scan_block(cleanup, output)?;
                }
                StatementKind::Return(value) => {
                    if let Some(value) = value {
                        self.scan_expr(value, output)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn scan_planned_stack_record_initializer(
        &self,
        expression: &Expr,
        output: &mut LocalRequirements,
    ) -> Result<(), CodegenError> {
        match &expression.kind {
            ExprKind::Record { fields, .. } => {
                // Only the POD container is compiler-private. Field expressions
                // keep their full fault/call/collection requirements.
                for field in fields {
                    self.scan_expr(field, output)?;
                }
                Ok(())
            }
            ExprKind::Block(block) => {
                self.scan_block_statements(block, output)?;
                let tail = block.tail.as_deref().ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        "stack-record initializer block has no tail",
                    )
                })?;
                self.scan_planned_stack_record_initializer(tail, output)
            }
            _ => Err(CodegenError::new(
                "LlvmAbiDefect",
                "stack-record requirement plan does not match its initializer",
            )),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn scan_expr(
        &self,
        expression: &Expr,
        output: &mut LocalRequirements,
    ) -> Result<(), CodegenError> {
        match &expression.kind {
            ExprKind::Constant(_) | ExprKind::Move(_) | ExprKind::ReborrowView { .. } => {}
            ExprKind::Copy(_) => {
                // Text is immutable and its runtime clone is a slot copy. Emit it
                // directly just like a native scalar; aggregate/refined/dyn copies
                // still require the managed deep-clone helper.
                if !matches!(expression.ty, Type::Text)
                    && !matches!(
                        NativeLayout::classify(self.program, &expression.ty),
                        Some(NativeLayout::Scalar(_))
                    )
                {
                    output
                        .requirements
                        .include(RuntimeRequirements::MAY_COLLECT);
                }
            }
            ExprKind::Tuple(values) | ExprKind::List(values) => {
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_COLLECT);
                for value in values {
                    self.scan_expr(value, output)?;
                }
            }
            ExprKind::Unary(operator, value) => {
                self.scan_expr(value, output)?;
                if *operator == UnaryOp::Negate
                    && is_int_like(self.program, &value.ty)
                    && !self.int_ranges.proves(self.function.id, expression)
                {
                    output.requirements.include(RuntimeRequirements::MAY_FAULT);
                }
            }
            ExprKind::Binary(operator, left, right) => {
                self.scan_expr(left, output)?;
                self.scan_expr(right, output)?;
                if matches!(
                    operator,
                    BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                ) && is_int_like(self.program, &left.ty)
                    && !self.int_ranges.proves(self.function.id, expression)
                {
                    output.requirements.include(RuntimeRequirements::MAY_FAULT);
                }
            }
            ExprKind::Block(block) => {
                self.scan_block(block, output)?;
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.scan_expr(condition, output)?;
                self.scan_block(then_branch, output)?;
                self.scan_block(else_branch, output)?;
            }
            ExprKind::Match { scrutinee, arms } => {
                if let Some(matched) = self
                    .native_int_lists
                    .direct_get_match(None, scrutinee, arms)
                {
                    // This exact exhaustive Option[Int] match reads compiler-private
                    // contiguous storage. It builds neither an Option aggregate nor
                    // a copied binding in the universal Value representation.
                    self.scan_expr(matched.index, output)?;
                } else {
                    // Pattern bindings are logical copies in the universal Value ABI.
                    output
                        .requirements
                        .include(RuntimeRequirements::MAY_COLLECT);
                    self.scan_expr(scrutinee, output)?;
                }
                for arm in arms {
                    self.scan_expr(&arm.value, output)?;
                }
            }
            ExprKind::Record { fields, .. } => {
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_COLLECT);
                for field in fields {
                    self.scan_expr(field, output)?;
                }
            }
            ExprKind::Variant { payload, .. } => {
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_COLLECT);
                for value in payload {
                    self.scan_expr(value, output)?;
                }
            }
            ExprKind::Refine { value, .. }
            | ExprKind::Unrefine(value)
            | ExprKind::MakeView { value, .. } => {
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_COLLECT);
                self.scan_expr(value, output)?;
            }
            ExprKind::Call {
                target, arguments, ..
            } => {
                for (index, argument) in arguments.iter().enumerate() {
                    if let CallArgument::Value(value) = argument {
                        if !matches!(target, CallTarget::Builtin(builtin)
                        if builtin_borrows_copy_argument(*builtin, index)
                            && matches!(value.kind, ExprKind::Copy(_)))
                        {
                            self.scan_expr(value, output)?;
                        }
                    }
                }
                match target {
                    CallTarget::Direct(callee) | CallTarget::Inherent(callee) => {
                        self.program.function(*callee).ok_or_else(|| {
                            CodegenError::new(
                                "InvalidFunctionReference",
                                format!("call target #{} does not exist", callee.0),
                            )
                        })?;
                        output.callees.insert(*callee);
                    }
                    CallTarget::StaticConcept {
                        requirement,
                        witness,
                        ..
                    } => {
                        if let Some(witness) = concrete_witness(witness) {
                            let method = self
                                .program
                                .witness(witness)
                                .and_then(|witness| witness.methods.get(requirement))
                                .copied()
                                .ok_or_else(|| {
                                    CodegenError::new(
                                        "InvalidWitnessTable",
                                        format!(
                                            "witness #{} has no requirement #{}",
                                            witness.0, requirement.0
                                        ),
                                    )
                                })?;
                            output.callees.insert(method);
                        } else {
                            add_dynamic_callees(
                                self.program,
                                self.reachable,
                                *requirement,
                                output,
                            )?;
                        }
                    }
                    CallTarget::Dynamic { requirement } => {
                        add_dynamic_callees(self.program, self.reachable, *requirement, output)?;
                    }
                    CallTarget::Builtin(builtin) => {
                        if is_native_int_list_add(&self.native_int_lists, *builtin, arguments) {
                            // The private append can reserve native bytes and fail,
                            // but it never allocates a managed Value or collects.
                            output.requirements.include(RuntimeRequirements::MAY_FAULT);
                        } else {
                            output.requirements.include(builtin_requirements(*builtin));
                        }
                    }
                }
            }
            ExprKind::Await { task, .. } => {
                output.requirements.include(RuntimeRequirements::ASYNC);
                self.scan_expr(task, output)?;
            }
            ExprKind::Sleep { milliseconds } => {
                output.requirements.include(RuntimeRequirements::ASYNC);
                self.scan_expr(milliseconds, output)?;
            }
            ExprKind::WaitFd { descriptor, .. } => {
                output.requirements.include(RuntimeRequirements::ASYNC);
                self.scan_expr(descriptor, output)?;
            }
            ExprKind::TaskJoin { arguments, .. } => {
                output.requirements.include(RuntimeRequirements::ASYNC);
                for argument in arguments {
                    self.scan_expr(argument, output)?;
                }
            }
        }
        Ok(())
    }
}

fn add_dynamic_callees(
    program: &Program,
    reachable: &ReachableProgram,
    requirement: loom_mir::RequirementId,
    output: &mut LocalRequirements,
) -> Result<(), CodegenError> {
    for (witness, methods) in &reachable.witness_methods {
        if !methods.contains(&requirement) {
            continue;
        }
        let method = program
            .witness(*witness)
            .and_then(|witness| witness.methods.get(&requirement))
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "InvalidWitnessTable",
                    format!(
                        "witness #{} has no live requirement #{}",
                        witness.0, requirement.0
                    ),
                )
            })?;
        output.callees.insert(method);
    }
    Ok(())
}

fn concrete_witness(reference: &WitnessRef) -> Option<loom_mir::WitnessId> {
    match reference {
        WitnessRef::Concrete(witness) | WitnessRef::Apply { witness, .. } => Some(*witness),
        WitnessRef::Parameter(_) => None,
    }
}

fn is_int_like(program: &Program, ty: &Type) -> bool {
    match ty {
        Type::Int => true,
        Type::Nominal(id, _) => program.type_def(*id).is_some_and(|definition| {
            matches!(
                &definition.kind,
                TypeDefKind::Refined { base, .. } if is_int_like(program, base)
            )
        }),
        _ => false,
    }
}

fn is_native_int_list_initializer(
    plan: &NativeIntListPlan,
    local: loom_mir::LocalId,
    expression: &Expr,
) -> bool {
    plan.contains(local)
        && expression.ty == Type::List(Box::new(Type::Int))
        && matches!(&expression.kind, ExprKind::List(values) if values.is_empty())
}

fn is_native_int_list_add(
    plan: &NativeIntListPlan,
    builtin: Builtin,
    arguments: &[CallArgument],
) -> bool {
    builtin == Builtin::ListAdd
        && matches!(
            arguments,
            [CallArgument::InOut(receiver), CallArgument::Value(_)]
                if receiver.projection.is_empty() && plan.contains(receiver.local)
        )
}

/// Whether a synchronous builtin consumes a copied argument only for the
/// duration of the call. The emitter mirrors this table by taking a rooted
/// shallow snapshot instead of materializing a deep source-language copy.
/// Any non-`Copy` argument expression is still evaluated normally.
pub(crate) const fn builtin_borrows_copy_argument(builtin: Builtin, index: usize) -> bool {
    match builtin {
        Builtin::TextLength
        | Builtin::BytesLength
        | Builtin::PathAsText
        | Builtin::ListLength
        | Builtin::TaskFaultCode
        | Builtin::TaskFaultMessage
        | Builtin::DurationAsMilliseconds
        | Builtin::TextMapLength
        | Builtin::IoErrorKind
        | Builtin::IoErrorMessage
        | Builtin::LogDebug
        | Builtin::LogInfo
        | Builtin::LogWarn
        | Builtin::LogError => index == 0,
        Builtin::TextContains | Builtin::TextMapContains => index < 2,
        Builtin::LogWrite => index < 3,
        _ => false,
    }
}

const fn builtin_requirements(builtin: Builtin) -> RuntimeRequirements {
    match builtin {
        Builtin::IsFinite
        | Builtin::TextLength
        | Builtin::BytesLength
        | Builtin::PathAsText
        | Builtin::ListLength
        | Builtin::TaskFaultCode
        | Builtin::TaskFaultMessage
        | Builtin::DurationAsMilliseconds
        | Builtin::TextMapNew
        | Builtin::TextMapLength
        | Builtin::IoErrorKind
        | Builtin::IoErrorMessage => RuntimeRequirements::NONE,
        Builtin::FileOpenRead
        | Builtin::FileCreate
        | Builtin::FileOpenReadPath
        | Builtin::FileCreatePath
        | Builtin::FileReadText
        | Builtin::FileWriteText
        | Builtin::SocketConnect
        | Builtin::SocketReadText
        | Builtin::SocketWriteText
        | Builtin::FileTryOpenRead
        | Builtin::FileTryCreate
        | Builtin::FileTryOpenReadPath
        | Builtin::FileTryCreatePath
        | Builtin::FileTryReadText
        | Builtin::FileTryWriteText
        | Builtin::SocketTryConnect
        | Builtin::SocketTryReadText
        | Builtin::SocketTryWriteText => RuntimeRequirements::ASYNC,
        Builtin::FileClose | Builtin::SocketClose => {
            RuntimeRequirements::MAY_FAULT.union(RuntimeRequirements::NEEDS_EXECUTOR)
        }
        Builtin::TextContains
        | Builtin::TextMapContains
        | Builtin::LogDebug
        | Builtin::LogInfo
        | Builtin::LogWarn
        | Builtin::LogError
        | Builtin::LogWrite => RuntimeRequirements::MAY_FAULT,
        Builtin::ParseFloat | Builtin::ParseInt | Builtin::TextEncodeUtf8 => {
            RuntimeRequirements::MAY_COLLECT
        }
        Builtin::FormatFloat
        | Builtin::TextGet
        | Builtin::TextConcat
        | Builtin::BytesGet
        | Builtin::BytesAppend
        | Builtin::BytesDecodeUtf8
        | Builtin::PathFromText
        | Builtin::PathJoin
        | Builtin::ListAdd
        | Builtin::ListGet
        | Builtin::ProcessArguments
        | Builtin::ProcessEnvironment
        | Builtin::DurationMilliseconds
        | Builtin::TextMapGet
        | Builtin::TextMapInsert
        | Builtin::TextMapRemove
        | Builtin::JsonParse
        | Builtin::JsonFormat => {
            RuntimeRequirements::MAY_FAULT.union(RuntimeRequirements::MAY_COLLECT)
        }
    }
}

#[cfg(test)]
#[allow(clippy::default_trait_access)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use loom_mir::{
        BinaryOp, Block, CallArgument, CallPlan, CallTarget, Constant, Expr, ExprKind, Function,
        FunctionId, LocalDecl, LocalId, Program, Type,
    };

    use super::{RuntimeRequirementGraph, builtin_requirements};

    fn int_function(id: u32, body: Expr) -> Function {
        Function {
            id: FunctionId(id),
            name: format!("function_{id}"),
            span: Default::default(),
            type_parameters: 0,
            is_async: false,
            suspension_points: Vec::new(),
            params: vec![LocalDecl {
                id: LocalId(0),
                name: "value".into(),
                ty: Type::Int,
                mutable: false,
                span: Default::default(),
            }],
            witness_params: Vec::new(),
            witness_prefix_count: 0,
            locals: Vec::new(),
            return_ty: Type::Int,
            receiver: None,
            body: Block {
                statements: Vec::new(),
                tail: Some(Box::new(body)),
                span: Default::default(),
            },
            call_plan: CallPlan::default(),
        }
    }

    fn expression(kind: ExprKind, ty: Type) -> Expr {
        Expr::new(kind, ty, Default::default())
    }

    #[test]
    fn closed_world_requirements_propagate_through_direct_calls() {
        let copy = expression(
            ExprKind::Copy(loom_mir::Place::local(LocalId(0))),
            Type::Int,
        );
        let pure_call = expression(
            ExprKind::Call {
                target: CallTarget::Direct(FunctionId(0)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(expression(
                    ExprKind::Copy(loom_mir::Place::local(LocalId(0))),
                    Type::Int,
                ))],
                witnesses: Vec::new(),
            },
            Type::Int,
        );
        let checked = expression(
            ExprKind::Binary(
                BinaryOp::Add,
                Box::new(expression(
                    ExprKind::Copy(loom_mir::Place::local(LocalId(0))),
                    Type::Int,
                )),
                Box::new(expression(ExprKind::Constant(Constant::Int(1)), Type::Int)),
            ),
            Type::Int,
        );
        let checked_call = expression(
            ExprKind::Call {
                target: CallTarget::Direct(FunctionId(2)),
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(expression(
                    ExprKind::Copy(loom_mir::Place::local(LocalId(0))),
                    Type::Int,
                ))],
                witnesses: Vec::new(),
            },
            Type::Int,
        );
        let program = Program {
            functions: vec![
                int_function(0, copy),
                int_function(1, pure_call),
                int_function(2, checked),
                int_function(3, checked_call),
            ],
            ..Program::default()
        };
        let reachable = crate::ReachableProgram {
            functions: BTreeSet::from([FunctionId(0), FunctionId(1), FunctionId(2), FunctionId(3)]),
            witnesses: BTreeSet::new(),
            builtins: BTreeSet::new(),
            witness_methods: BTreeMap::new(),
        };
        let graph = RuntimeRequirementGraph::analyze(
            &program,
            &reachable,
            &crate::native_range::NativeIntRangePlan::default(),
            &BTreeMap::new(),
        )
        .expect("requirements");
        assert!(
            crate::native_layout::NativeSignatureShape::for_supported_function(
                &program.functions[0]
            )
            .is_some()
        );
        assert!(
            graph
                .function(FunctionId(0))
                .unwrap()
                .body
                .is_pure_no_fault()
        );
        assert!(
            graph
                .function(FunctionId(1))
                .unwrap()
                .body
                .is_pure_no_fault()
        );
        assert!(graph.function(FunctionId(2)).unwrap().body.may_fault());
        assert!(graph.function(FunctionId(3)).unwrap().body.may_fault());
        assert!(!graph.function(FunctionId(3)).unwrap().body.may_collect());
        assert!(!graph.function(FunctionId(3)).unwrap().body.needs_executor());
    }

    #[test]
    fn builtins_distinguish_collection_fault_and_executor_requirements() {
        use loom_mir::Builtin;

        for builtin in [
            Builtin::TextLength,
            Builtin::TextContains,
            Builtin::BytesLength,
            Builtin::PathAsText,
            Builtin::ListLength,
            Builtin::TaskFaultCode,
            Builtin::TaskFaultMessage,
            Builtin::DurationAsMilliseconds,
            Builtin::TextMapNew,
            Builtin::TextMapLength,
            Builtin::TextMapContains,
            Builtin::IoErrorKind,
            Builtin::IoErrorMessage,
            Builtin::LogDebug,
            Builtin::LogInfo,
            Builtin::LogWarn,
            Builtin::LogError,
            Builtin::LogWrite,
            Builtin::FileClose,
            Builtin::SocketClose,
        ] {
            assert!(
                !builtin_requirements(builtin).may_collect(),
                "{builtin:?} unexpectedly enters moving GC"
            );
        }
        for builtin in [
            Builtin::TextGet,
            Builtin::FormatFloat,
            Builtin::ListGet,
            Builtin::TextMapGet,
            Builtin::JsonParse,
        ] {
            assert!(
                builtin_requirements(builtin).may_collect(),
                "{builtin:?} lost its managed builder boundary"
            );
        }

        let contains = builtin_requirements(Builtin::TextMapContains);
        assert!(contains.may_fault());
        assert!(!contains.needs_executor());
        let get = builtin_requirements(Builtin::TextMapGet);
        assert!(get.may_fault());
        assert!(get.may_collect());

        for builtin in [Builtin::FileClose, Builtin::SocketClose] {
            let requirements = builtin_requirements(builtin);
            assert!(requirements.may_fault());
            assert!(requirements.needs_executor());
            assert!(!requirements.may_collect());
        }
        let async_constructor = builtin_requirements(Builtin::FileOpenRead);
        assert!(async_constructor.may_fault());
        assert!(async_constructor.needs_executor());
        assert!(async_constructor.may_collect());
    }
}
