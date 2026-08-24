use std::collections::{BTreeMap, BTreeSet, VecDeque};

use loom_mir::{
    Block, Builtin, CallArgument, CallTarget, Expr, ExprKind, FunctionId, Program, RequirementId,
    StatementKind, WitnessId, WitnessRef,
};
use serde::{Deserialize, Serialize};

use crate::CodegenError;

/// Root functions selected by a command-line build mode.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Roots {
    functions: BTreeSet<FunctionId>,
}

impl Roots {
    #[must_use]
    pub fn for_entry(program: &Program, entry: &str) -> Option<Self> {
        program.exports.get(entry).copied().map(Self::one)
    }

    #[must_use]
    pub fn for_tests(program: &Program) -> Self {
        Self {
            functions: program.tests.iter().copied().collect(),
        }
    }

    #[must_use]
    pub fn one(function: FunctionId) -> Self {
        Self {
            functions: BTreeSet::from([function]),
        }
    }

    #[must_use]
    pub fn functions(&self) -> &BTreeSet<FunctionId> {
        &self.functions
    }
}

/// The closed-world subset that must be materialized in one native artifact.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReachableProgram {
    pub functions: BTreeSet<FunctionId>,
    pub witnesses: BTreeSet<WitnessId>,
    pub builtins: BTreeSet<Builtin>,
    /// Only these witness method slots are emitted as live table edges.
    pub witness_methods: BTreeMap<WitnessId, BTreeSet<RequirementId>>,
}

#[derive(Default)]
struct FunctionEdges {
    direct: BTreeSet<FunctionId>,
    witnesses: BTreeSet<WitnessId>,
    builtins: BTreeSet<Builtin>,
    dynamic: BTreeSet<RequirementId>,
    concrete_methods: BTreeSet<(WitnessId, RequirementId)>,
}

/// Traverses calls from the selected roots and closes dynamic edges through
/// only witness values that reachable code actually constructs or passes.
///
/// This is deliberately separate from LLVM emission: it becomes the stable
/// dependency graph used by future per-module compilation cache keys.
///
/// # Errors
///
/// Returns an error if checked MIR contains a missing function, witness, or
/// witness method reference. Such an error is a compiler-boundary defect.
pub fn analyze_reachability(
    program: &Program,
    roots: &Roots,
) -> Result<ReachableProgram, CodegenError> {
    if roots.functions.is_empty() {
        // An empty test suite is a successful, empty native harness. Entry
        // builds cannot reach this case because root selection reports an
        // unknown export before graph construction.
        return Ok(ReachableProgram::default());
    }

    let mut result = ReachableProgram::default();
    let mut queue = VecDeque::new();
    for root in &roots.functions {
        require_function(program, *root)?;
        if result.functions.insert(*root) {
            queue.push_back(*root);
        }
    }

    let mut dynamic_requirements = BTreeSet::new();
    let mut explicit_methods = BTreeSet::new();
    loop {
        while let Some(function_id) = queue.pop_front() {
            let function = require_function(program, function_id)?;
            let mut edges = FunctionEdges::default();
            scan_block(&function.body, &mut edges);
            for target in edges.direct {
                require_function(program, target)?;
                if result.functions.insert(target) {
                    queue.push_back(target);
                }
            }
            result.witnesses.extend(edges.witnesses);
            result.builtins.extend(edges.builtins);
            dynamic_requirements.extend(edges.dynamic);
            explicit_methods.extend(edges.concrete_methods);
        }

        let before_functions = result.functions.len();
        let before_witnesses = result.witnesses.len();

        for (witness_id, requirement) in explicit_methods.iter().copied() {
            result.witnesses.insert(witness_id);
            retain_witness_method(program, &mut result, witness_id, requirement, &mut queue)?;
        }

        // A dynamic receiver can only carry a witness made live by a reachable
        // erasure/proof edge. Unreferenced conformances remain dead.
        let live_witnesses = result.witnesses.iter().copied().collect::<Vec<_>>();
        for witness_id in live_witnesses {
            let witness = program.witness(witness_id).ok_or_else(|| {
                CodegenError::new(
                    "InvalidWitnessReference",
                    format!("reachable witness #{} does not exist", witness_id.0),
                )
            })?;
            for requirement in &dynamic_requirements {
                if program
                    .requirement(*requirement)
                    .is_some_and(|definition| definition.concept == witness.concept)
                {
                    retain_witness_method(
                        program,
                        &mut result,
                        witness_id,
                        *requirement,
                        &mut queue,
                    )?;
                }
            }
        }

        if queue.is_empty()
            && result.functions.len() == before_functions
            && result.witnesses.len() == before_witnesses
        {
            break;
        }
    }

    Ok(result)
}

fn retain_witness_method(
    program: &Program,
    result: &mut ReachableProgram,
    witness_id: WitnessId,
    requirement: RequirementId,
    queue: &mut VecDeque<FunctionId>,
) -> Result<(), CodegenError> {
    let witness = program.witness(witness_id).ok_or_else(|| {
        CodegenError::new(
            "InvalidWitnessReference",
            format!("reachable witness #{} does not exist", witness_id.0),
        )
    })?;
    let function = witness.methods.get(&requirement).copied().ok_or_else(|| {
        CodegenError::new(
            "InvalidWitnessTable",
            format!(
                "witness #{} has no slot for requirement #{}",
                witness_id.0, requirement.0
            ),
        )
    })?;
    result
        .witness_methods
        .entry(witness_id)
        .or_default()
        .insert(requirement);
    require_function(program, function)?;
    if result.functions.insert(function) {
        queue.push_back(function);
    }
    Ok(())
}

fn require_function(
    program: &Program,
    function: FunctionId,
) -> Result<&loom_mir::Function, CodegenError> {
    program.function(function).ok_or_else(|| {
        CodegenError::new(
            "InvalidFunctionReference",
            format!("reachable function #{} does not exist", function.0),
        )
    })
}

fn scan_block(block: &Block, edges: &mut FunctionEdges) {
    for statement in &block.statements {
        match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Assert { condition: value }
            | StatementKind::Evaluate(value) => scan_expr(value, edges),
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    scan_expr(value, edges);
                }
            }
            StatementKind::Defer(cleanup) => scan_block(cleanup, edges),
        }
    }
    if let Some(tail) = &block.tail {
        scan_expr(tail, edges);
    }
}

fn scan_expr(expression: &Expr, edges: &mut FunctionEdges) {
    match &expression.kind {
        ExprKind::Tuple(elements) | ExprKind::List(elements) => {
            for element in elements {
                scan_expr(element, edges);
            }
        }
        ExprKind::Unary(_, value) | ExprKind::Unrefine(value) | ExprKind::Refine { value, .. } => {
            scan_expr(value, edges);
        }
        ExprKind::Binary(_, left, right) => {
            scan_expr(left, edges);
            scan_expr(right, edges);
        }
        ExprKind::Block(block) => scan_block(block, edges),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_expr(condition, edges);
            scan_block(then_branch, edges);
            scan_block(else_branch, edges);
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_expr(scrutinee, edges);
            for arm in arms {
                scan_expr(&arm.value, edges);
            }
        }
        ExprKind::Record { fields, .. } => {
            for field in fields {
                scan_expr(field, edges);
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                scan_expr(value, edges);
            }
        }
        ExprKind::Call {
            target,
            arguments,
            witnesses,
            ..
        } => {
            match target {
                CallTarget::Direct(function) | CallTarget::Inherent(function) => {
                    edges.direct.insert(*function);
                }
                CallTarget::StaticConcept {
                    requirement,
                    witness,
                    ..
                } => {
                    collect_witness(witness, &mut edges.witnesses);
                    if let Some(witness) = concrete_witness(witness) {
                        edges.concrete_methods.insert((witness, *requirement));
                    } else {
                        edges.dynamic.insert(*requirement);
                    }
                }
                CallTarget::Dynamic { requirement } => {
                    edges.dynamic.insert(*requirement);
                }
                CallTarget::Builtin(builtin) => {
                    edges.builtins.insert(*builtin);
                }
            }
            for argument in arguments {
                if let CallArgument::Value(value) = argument {
                    scan_expr(value, edges);
                }
            }
            for witness in witnesses {
                collect_witness(witness, &mut edges.witnesses);
            }
        }
        ExprKind::MakeView { witness, .. } => collect_witness(witness, &mut edges.witnesses),
        ExprKind::Await { task, .. } => scan_expr(task, edges),
        ExprKind::TaskJoin { arguments, .. } => {
            for argument in arguments {
                scan_expr(argument, edges);
            }
        }
        ExprKind::Sleep { milliseconds } => scan_expr(milliseconds, edges),
        ExprKind::WaitFd { descriptor, .. } => scan_expr(descriptor, edges),
        ExprKind::Constant(_) | ExprKind::Copy(_) | ExprKind::Move(_) => {}
    }
}

fn collect_witness(reference: &WitnessRef, output: &mut BTreeSet<WitnessId>) {
    match reference {
        WitnessRef::Concrete(witness) => {
            output.insert(*witness);
        }
        WitnessRef::Parameter(_) => {}
        WitnessRef::Apply { witness, arguments } => {
            output.insert(*witness);
            for argument in arguments {
                collect_witness(argument, output);
            }
        }
    }
}

fn concrete_witness(reference: &WitnessRef) -> Option<WitnessId> {
    match reference {
        WitnessRef::Concrete(witness) | WitnessRef::Apply { witness, .. } => Some(*witness),
        WitnessRef::Parameter(_) => None,
    }
}
