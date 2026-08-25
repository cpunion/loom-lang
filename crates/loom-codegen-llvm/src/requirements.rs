use std::collections::{BTreeMap, BTreeSet};

use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, Expr, ExprKind, Function, FunctionId,
    Program, StatementKind, Type, TypeDefKind, UnaryOp, WitnessRef,
};

use crate::native_layout::{NativeLayout, NativeSignatureShape};
use crate::native_range::NativeIntRangePlan;
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
    const MAY_ALLOCATE_BIT: u8 = 1 << 1;
    const NEEDS_EXECUTOR_BIT: u8 = 1 << 2;

    pub(crate) const NONE: Self = Self(0);
    pub(crate) const MAY_FAULT: Self = Self(Self::MAY_FAULT_BIT);
    pub(crate) const MAY_ALLOCATE: Self = Self(Self::MAY_ALLOCATE_BIT);
    pub(crate) const NEEDS_EXECUTOR: Self = Self(Self::NEEDS_EXECUTOR_BIT);
    /// The current Task constructor/resume ABI requires all three facilities.
    pub(crate) const ASYNC: Self = Self::MAY_FAULT
        .union(Self::MAY_ALLOCATE)
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
    pub(crate) const fn may_allocate(self) -> bool {
        self.0 & Self::MAY_ALLOCATE_BIT != 0
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

impl RuntimeRequirementGraph {
    pub(crate) fn analyze(
        program: &Program,
        reachable: &ReachableProgram,
        int_ranges: &NativeIntRangePlan,
    ) -> Result<Self, CodegenError> {
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
                    RuntimeRequirements::MAY_FAULT.union(RuntimeRequirements::MAY_ALLOCATE),
                );
            }
            scan_block(
                program,
                reachable,
                function,
                &function.body,
                &mut requirements,
                int_ranges,
            )?;
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

fn scan_block(
    program: &Program,
    reachable: &ReachableProgram,
    function: &Function,
    block: &Block,
    output: &mut LocalRequirements,
    int_ranges: &NativeIntRangePlan,
) -> Result<(), CodegenError> {
    for statement in &block.statements {
        match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => {
                scan_expr(program, reachable, function, value, output, int_ranges)?;
            }
            StatementKind::LetTuple { value, .. } => {
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_ALLOCATE);
                scan_expr(program, reachable, function, value, output, int_ranges)?;
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                scan_expr(program, reachable, function, start, output, int_ranges)?;
                scan_expr(program, reachable, function, end, output, int_ranges)?;
                scan_block(program, reachable, function, body, output, int_ranges)?;
            }
            StatementKind::Assert { condition } => {
                output.requirements.include(RuntimeRequirements::MAY_FAULT);
                scan_expr(program, reachable, function, condition, output, int_ranges)?;
            }
            StatementKind::Defer(cleanup) => {
                scan_block(program, reachable, function, cleanup, output, int_ranges)?;
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    scan_expr(program, reachable, function, value, output, int_ranges)?;
                }
            }
        }
    }
    if let Some(tail) = &block.tail {
        scan_expr(program, reachable, function, tail, output, int_ranges)?;
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn scan_expr(
    program: &Program,
    reachable: &ReachableProgram,
    function: &Function,
    expression: &Expr,
    output: &mut LocalRequirements,
    int_ranges: &NativeIntRangePlan,
) -> Result<(), CodegenError> {
    match &expression.kind {
        ExprKind::Constant(_) | ExprKind::Move(_) | ExprKind::ReborrowView { .. } => {}
        ExprKind::Copy(_) => {
            if !matches!(
                NativeLayout::classify(program, &expression.ty),
                Some(NativeLayout::Scalar(_))
            ) {
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_ALLOCATE);
            }
        }
        ExprKind::Tuple(values) | ExprKind::List(values) => {
            output
                .requirements
                .include(RuntimeRequirements::MAY_ALLOCATE);
            for value in values {
                scan_expr(program, reachable, function, value, output, int_ranges)?;
            }
        }
        ExprKind::Unary(operator, value) => {
            scan_expr(program, reachable, function, value, output, int_ranges)?;
            if *operator == UnaryOp::Negate
                && is_int_like(program, &value.ty)
                && !int_ranges.proves(function.id, expression)
            {
                output.requirements.include(RuntimeRequirements::MAY_FAULT);
            }
        }
        ExprKind::Binary(operator, left, right) => {
            scan_expr(program, reachable, function, left, output, int_ranges)?;
            scan_expr(program, reachable, function, right, output, int_ranges)?;
            if matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
            ) && is_int_like(program, &left.ty)
                && !int_ranges.proves(function.id, expression)
            {
                output.requirements.include(RuntimeRequirements::MAY_FAULT);
            } else if matches!(operator, BinaryOp::Equal | BinaryOp::NotEqual) {
                // Equality still goes through the universal Value helper.
                output
                    .requirements
                    .include(RuntimeRequirements::MAY_ALLOCATE);
            }
        }
        ExprKind::Block(block) => {
            scan_block(program, reachable, function, block, output, int_ranges)?;
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_expr(program, reachable, function, condition, output, int_ranges)?;
            scan_block(
                program,
                reachable,
                function,
                then_branch,
                output,
                int_ranges,
            )?;
            scan_block(
                program,
                reachable,
                function,
                else_branch,
                output,
                int_ranges,
            )?;
        }
        ExprKind::Match { scrutinee, arms } => {
            // Pattern bindings are logical copies in the universal Value ABI.
            output
                .requirements
                .include(RuntimeRequirements::MAY_ALLOCATE);
            scan_expr(program, reachable, function, scrutinee, output, int_ranges)?;
            for arm in arms {
                scan_expr(program, reachable, function, &arm.value, output, int_ranges)?;
            }
        }
        ExprKind::Record { fields, .. } => {
            output
                .requirements
                .include(RuntimeRequirements::MAY_ALLOCATE);
            for field in fields {
                scan_expr(program, reachable, function, field, output, int_ranges)?;
            }
        }
        ExprKind::Variant { payload, .. } => {
            output
                .requirements
                .include(RuntimeRequirements::MAY_ALLOCATE);
            for value in payload {
                scan_expr(program, reachable, function, value, output, int_ranges)?;
            }
        }
        ExprKind::Refine { value, .. }
        | ExprKind::Unrefine(value)
        | ExprKind::MakeView { value, .. } => {
            output
                .requirements
                .include(RuntimeRequirements::MAY_ALLOCATE);
            scan_expr(program, reachable, function, value, output, int_ranges)?;
        }
        ExprKind::Call {
            target, arguments, ..
        } => {
            for argument in arguments {
                if let CallArgument::Value(value) = argument {
                    scan_expr(program, reachable, function, value, output, int_ranges)?;
                }
            }
            match target {
                CallTarget::Direct(callee) | CallTarget::Inherent(callee) => {
                    let target = program.function(*callee).ok_or_else(|| {
                        CodegenError::new(
                            "InvalidFunctionReference",
                            format!("call target #{} does not exist", callee.0),
                        )
                    })?;
                    output.callees.insert(*callee);
                    if NativeSignatureShape::for_supported_function(target).is_none() {
                        // Aggregate and managed calls retain the universal Value ABI boundary
                        // until their complete layout/materialization plan exists.
                        output
                            .requirements
                            .include(RuntimeRequirements::MAY_ALLOCATE);
                    }
                }
                CallTarget::StaticConcept {
                    requirement,
                    witness,
                    ..
                } => {
                    output
                        .requirements
                        .include(RuntimeRequirements::MAY_ALLOCATE);
                    if let Some(witness) = concrete_witness(witness) {
                        let method = program
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
                        add_dynamic_callees(program, reachable, *requirement, output)?;
                    }
                }
                CallTarget::Dynamic { requirement } => {
                    output
                        .requirements
                        .include(RuntimeRequirements::MAY_ALLOCATE);
                    add_dynamic_callees(program, reachable, *requirement, output)?;
                }
                CallTarget::Builtin(builtin) => {
                    output.requirements.include(builtin_requirements(*builtin));
                }
            }
        }
        ExprKind::Await { task, .. } => {
            output.requirements.include(RuntimeRequirements::ASYNC);
            scan_expr(program, reachable, function, task, output, int_ranges)?;
        }
        ExprKind::Sleep { milliseconds } => {
            output.requirements.include(RuntimeRequirements::ASYNC);
            scan_expr(
                program,
                reachable,
                function,
                milliseconds,
                output,
                int_ranges,
            )?;
        }
        ExprKind::WaitFd { descriptor, .. } => {
            output.requirements.include(RuntimeRequirements::ASYNC);
            scan_expr(program, reachable, function, descriptor, output, int_ranges)?;
        }
        ExprKind::TaskJoin { arguments, .. } => {
            output.requirements.include(RuntimeRequirements::ASYNC);
            for argument in arguments {
                scan_expr(program, reachable, function, argument, output, int_ranges)?;
            }
        }
    }
    Ok(())
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

const fn builtin_requirements(builtin: Builtin) -> RuntimeRequirements {
    match builtin {
        Builtin::IsFinite => RuntimeRequirements::NONE,
        Builtin::FileOpenRead
        | Builtin::FileCreate
        | Builtin::FileOpenReadPath
        | Builtin::FileCreatePath
        | Builtin::FileReadText
        | Builtin::FileWriteText
        | Builtin::FileClose
        | Builtin::SocketConnect
        | Builtin::SocketReadText
        | Builtin::SocketWriteText
        | Builtin::SocketClose
        | Builtin::FileTryOpenRead
        | Builtin::FileTryCreate
        | Builtin::FileTryOpenReadPath
        | Builtin::FileTryCreatePath
        | Builtin::FileTryReadText
        | Builtin::FileTryWriteText
        | Builtin::SocketTryConnect
        | Builtin::SocketTryReadText
        | Builtin::SocketTryWriteText => RuntimeRequirements::ASYNC,
        Builtin::ParseFloat
        | Builtin::FormatFloat
        | Builtin::TextLength
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
        | Builtin::ListAdd
        | Builtin::ListLength
        | Builtin::ListGet
        | Builtin::ProcessArguments
        | Builtin::ProcessEnvironment
        | Builtin::ParseInt
        | Builtin::TaskFaultCode
        | Builtin::TaskFaultMessage
        | Builtin::DurationMilliseconds
        | Builtin::DurationAsMilliseconds
        | Builtin::TextMapNew
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
        | Builtin::LogWrite => {
            RuntimeRequirements::MAY_FAULT.union(RuntimeRequirements::MAY_ALLOCATE)
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

    use super::RuntimeRequirementGraph;

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
        assert!(!graph.function(FunctionId(3)).unwrap().body.may_allocate());
        assert!(!graph.function(FunctionId(3)).unwrap().body.needs_executor());
    }
}
