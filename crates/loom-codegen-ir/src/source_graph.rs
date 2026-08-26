use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
};

use loom_mir::{
    Block, Builtin, CallArgument, CallTarget, Expr, ExprKind, FunctionId, LocalId, Program,
    RequirementId, StatementKind, WitnessId, WitnessRef,
};
use serde::{Deserialize, Serialize};

/// Root functions selected by a command-line build mode.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceRoots {
    functions: BTreeSet<FunctionId>,
}

impl SourceRoots {
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
pub struct ReachableSourceGraph {
    pub functions: BTreeSet<FunctionId>,
    pub witnesses: BTreeSet<WitnessId>,
    pub builtins: BTreeSet<Builtin>,
    /// Only these witness method slots are emitted as live table edges.
    pub witness_methods: BTreeMap<WitnessId, BTreeSet<RequirementId>>,
}

/// Stable category for an invalid checked-MIR source graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphErrorCode {
    InvalidFunctionReference,
    InvalidWitnessReference,
    InvalidWitnessTable,
}

impl GraphErrorCode {
    /// Stable diagnostic code used at compiler boundaries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidFunctionReference => "InvalidFunctionReference",
            Self::InvalidWitnessReference => "InvalidWitnessReference",
            Self::InvalidWitnessTable => "InvalidWitnessTable",
        }
    }
}

impl fmt::Display for GraphErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A malformed edge discovered while closing the checked-MIR source graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphError {
    code: GraphErrorCode,
    message: String,
}

impl GraphError {
    fn new(code: GraphErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Structured error category with a stable textual spelling.
    #[must_use]
    pub const fn code(&self) -> GraphErrorCode {
        self.code
    }

    /// Stable human-readable detail for the invalid graph edge.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for GraphError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl Error for GraphError {}

#[derive(Default)]
struct FunctionEdges {
    direct: BTreeSet<FunctionId>,
    witnesses: BTreeSet<WitnessId>,
    builtins: BTreeSet<Builtin>,
    dynamic: BTreeSet<RequirementId>,
    concrete_methods: BTreeSet<(WitnessId, RequirementId)>,
}

#[derive(Clone, Debug, Default)]
struct WitnessFlow {
    /// `None` means a value may carry any runtime witness. A concrete set is
    /// retained only while straight-line MIR proves every reaching value.
    locals: BTreeMap<LocalId, Option<BTreeSet<WitnessId>>>,
}

/// Traverses calls from the selected roots and closes dynamic edges through
/// only witness values that reachable code actually constructs or passes.
///
/// This is deliberately separate from LLVM emission. It is the stable source
/// dependency graph used by native-object identity and future per-module cache
/// keys.
///
/// # Errors
///
/// Returns an error if checked MIR contains a missing function, witness, or
/// witness method reference. Such an error is a compiler-boundary defect.
pub fn analyze_source_reachability(
    program: &Program,
    roots: &SourceRoots,
) -> Result<ReachableSourceGraph, GraphError> {
    if roots.functions.is_empty() {
        // An empty test suite is a successful, empty native harness. Entry
        // builds cannot reach this case because root selection reports an
        // unknown export before graph construction.
        return Ok(ReachableSourceGraph::default());
    }

    let mut result = ReachableSourceGraph::default();
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
                GraphError::new(
                    GraphErrorCode::InvalidWitnessReference,
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
    result: &mut ReachableSourceGraph,
    witness_id: WitnessId,
    requirement: RequirementId,
    queue: &mut VecDeque<FunctionId>,
) -> Result<(), GraphError> {
    let witness = program.witness(witness_id).ok_or_else(|| {
        GraphError::new(
            GraphErrorCode::InvalidWitnessReference,
            format!("reachable witness #{} does not exist", witness_id.0),
        )
    })?;
    let function = witness.methods.get(&requirement).copied().ok_or_else(|| {
        GraphError::new(
            GraphErrorCode::InvalidWitnessTable,
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
) -> Result<&loom_mir::Function, GraphError> {
    program.function(function).ok_or_else(|| {
        GraphError::new(
            GraphErrorCode::InvalidFunctionReference,
            format!("reachable function #{} does not exist", function.0),
        )
    })
}

fn scan_block(block: &Block, edges: &mut FunctionEdges) {
    scan_block_with_flow(block, edges, &mut WitnessFlow::default());
}

fn scan_block_with_flow(block: &Block, edges: &mut FunctionEdges, flow: &mut WitnessFlow) {
    for statement in &block.statements {
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let witnesses = expression_witnesses(value, flow);
                scan_expr(value, edges, flow);
                flow.locals.insert(*local, witnesses);
            }
            StatementKind::LetTuple { locals, value } => {
                let elements = match &value.kind {
                    ExprKind::Tuple(elements) if elements.len() == locals.len() => Some(
                        elements
                            .iter()
                            .map(|element| expression_witnesses(element, flow))
                            .collect::<Vec<_>>(),
                    ),
                    _ => None,
                };
                scan_expr(value, edges, flow);
                for (index, local) in locals.iter().enumerate() {
                    flow.locals.insert(
                        *local,
                        elements
                            .as_ref()
                            .and_then(|elements| elements.get(index).cloned())
                            .flatten(),
                    );
                }
            }
            StatementKind::Assign { place, value } => {
                let witnesses = expression_witnesses(value, flow);
                scan_expr(value, edges, flow);
                if place.projection.is_empty() {
                    flow.locals.insert(place.local, witnesses);
                } else {
                    flow.locals.insert(place.local, None);
                }
            }
            StatementKind::Assert { condition: value } | StatementKind::Evaluate(value) => {
                scan_expr(value, edges, flow);
            }
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                scan_expr(start, edges, flow);
                scan_expr(end, edges, flow);
                let mut body_flow = flow.clone();
                scan_block_with_flow(body, edges, &mut body_flow);
                invalidate_flow(flow);
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    scan_expr(value, edges, flow);
                }
            }
            StatementKind::Defer(cleanup) => {
                let mut cleanup_flow = flow.clone();
                scan_block_with_flow(cleanup, edges, &mut cleanup_flow);
            }
        }
    }
    if let Some(tail) = &block.tail {
        scan_expr(tail, edges, flow);
    }
}

#[allow(clippy::too_many_lines)]
fn scan_expr(expression: &Expr, edges: &mut FunctionEdges, flow: &mut WitnessFlow) {
    match &expression.kind {
        ExprKind::Tuple(elements) | ExprKind::List(elements) => {
            for element in elements {
                scan_expr(element, edges, flow);
            }
        }
        ExprKind::Unary(_, value) | ExprKind::Unrefine(value) | ExprKind::Refine { value, .. } => {
            scan_expr(value, edges, flow);
        }
        ExprKind::Binary(_, left, right) => {
            scan_expr(left, edges, flow);
            scan_expr(right, edges, flow);
        }
        ExprKind::Block(block) => {
            let mut block_flow = flow.clone();
            scan_block_with_flow(block, edges, &mut block_flow);
            invalidate_flow(flow);
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            scan_expr(condition, edges, flow);
            let mut then_flow = flow.clone();
            let mut else_flow = flow.clone();
            scan_block_with_flow(then_branch, edges, &mut then_flow);
            scan_block_with_flow(else_branch, edges, &mut else_flow);
            invalidate_flow(flow);
        }
        ExprKind::Match { scrutinee, arms } => {
            scan_expr(scrutinee, edges, flow);
            for arm in arms {
                let mut arm_flow = flow.clone();
                scan_expr(&arm.value, edges, &mut arm_flow);
            }
            invalidate_flow(flow);
        }
        ExprKind::Record { fields, .. } => {
            for field in fields {
                scan_expr(field, edges, flow);
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                scan_expr(value, edges, flow);
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
                    let receiver_witnesses =
                        arguments.first().and_then(|argument| match argument {
                            CallArgument::Value(receiver) => expression_witnesses(receiver, flow),
                            CallArgument::InOut(place) if place.projection.is_empty() => {
                                flow.locals.get(&place.local).cloned().flatten()
                            }
                            CallArgument::InOut(_) => None,
                        });
                    if let Some(witnesses) = receiver_witnesses.filter(|set| !set.is_empty()) {
                        edges
                            .concrete_methods
                            .extend(witnesses.into_iter().map(|witness| (witness, *requirement)));
                    } else {
                        edges.dynamic.insert(*requirement);
                    }
                }
                CallTarget::Builtin(builtin) => {
                    edges.builtins.insert(*builtin);
                }
            }
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => scan_expr(value, edges, flow),
                    CallArgument::InOut(place) => {
                        flow.locals.insert(place.local, None);
                    }
                }
            }
            for witness in witnesses {
                collect_witness(witness, &mut edges.witnesses);
            }
        }
        ExprKind::MakeView { value, witness, .. } => {
            scan_expr(value, edges, flow);
            collect_witness(witness, &mut edges.witnesses);
        }
        ExprKind::Await { task, .. } => scan_expr(task, edges, flow),
        ExprKind::TaskJoin { arguments, .. } => {
            for argument in arguments {
                scan_expr(argument, edges, flow);
            }
        }
        ExprKind::Sleep { milliseconds } => scan_expr(milliseconds, edges, flow),
        ExprKind::WaitFd { descriptor, .. } => scan_expr(descriptor, edges, flow),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => {}
    }
}

fn expression_witnesses(expression: &Expr, flow: &WitnessFlow) -> Option<BTreeSet<WitnessId>> {
    match &expression.kind {
        ExprKind::MakeView { witness, .. } => {
            concrete_witness(witness).map(|witness| BTreeSet::from([witness]))
        }
        ExprKind::Copy(place) | ExprKind::Move(place) if place.projection.is_empty() => {
            flow.locals.get(&place.local).cloned().flatten()
        }
        ExprKind::ReborrowView { owner, .. } if owner.projection.is_empty() => {
            flow.locals.get(&owner.local).cloned().flatten()
        }
        _ => None,
    }
}

fn invalidate_flow(flow: &mut WitnessFlow) {
    for witnesses in flow.locals.values_mut() {
        *witnesses = None;
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
