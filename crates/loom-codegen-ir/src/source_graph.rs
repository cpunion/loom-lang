use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
};

use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, CheckedProgram, Expr, ExprKind, FunctionId,
    LocalId, Program, RequirementId, StatementKind, Type, WitnessId, WitnessRef,
};
use serde::{Deserialize, Serialize};

/// Root functions selected by a command-line build mode.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceRoots {
    functions: BTreeSet<FunctionId>,
}

impl SourceRoots {
    #[must_use]
    pub fn for_entry(program: &CheckedProgram, entry: &str) -> Option<Self> {
        program.exports.get(entry).copied().map(Self::one)
    }

    #[must_use]
    pub fn for_tests(program: &CheckedProgram) -> Self {
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

#[derive(Clone, Debug, Default, Eq, PartialEq)]
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
    program: &CheckedProgram,
    roots: &SourceRoots,
) -> Result<ReachableSourceGraph, GraphError> {
    let program = program.as_program();
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
    let _ = scan_block_with_flow(block, edges, &mut WitnessFlow::default(), &mut Vec::new());
}

/// Scans the executable portion of a block and returns whether control can
/// reach its normal successor. The checked-MIR source graph must agree with
/// the control flow a backend can actually materialize; otherwise a callee
/// mentioned only after divergence becomes an unreachable artifact member.
#[allow(clippy::too_many_lines)]
fn scan_block_with_flow<'mir>(
    block: &'mir Block,
    edges: &mut FunctionEdges,
    flow: &mut WitnessFlow,
    active_cleanups: &mut Vec<&'mir Block>,
) -> bool {
    let cleanup_base = active_cleanups.len();
    let mut continues = true;
    for statement in &block.statements {
        continues = match &statement.kind {
            StatementKind::Let { local, value } => {
                let witnesses = expression_witnesses(value, flow);
                if scan_expr(value, edges, flow, active_cleanups) {
                    flow.locals.insert(*local, witnesses);
                    true
                } else {
                    false
                }
            }
            StatementKind::LetTuple { locals, value } => {
                let (continues, elements) = match &value.kind {
                    ExprKind::Tuple(elements) if elements.len() == locals.len() => {
                        // Tuple elements evaluate left-to-right. Snapshot each
                        // element's witness at its own evaluation point, then
                        // let that element update the flow seen by the next.
                        // Destination locals are bound only after the entire
                        // tuple value completes.
                        let mut witnesses = Vec::with_capacity(elements.len());
                        let mut continues = true;
                        for element in elements {
                            witnesses.push(expression_witnesses(element, flow));
                            if !scan_expr(element, edges, flow, active_cleanups) {
                                continues = false;
                                break;
                            }
                        }
                        (continues && value.ty != Type::Never, Some(witnesses))
                    }
                    _ => (scan_expr(value, edges, flow, active_cleanups), None),
                };
                if continues {
                    for (index, local) in locals.iter().enumerate() {
                        flow.locals.insert(
                            *local,
                            elements
                                .as_ref()
                                .and_then(|elements| elements.get(index).cloned())
                                .flatten(),
                        );
                    }
                    true
                } else {
                    false
                }
            }
            StatementKind::Assign { place, value } => {
                let witnesses = expression_witnesses(value, flow);
                if scan_expr(value, edges, flow, active_cleanups) {
                    if place.projection.is_empty() {
                        flow.locals.insert(place.local, witnesses);
                    } else {
                        flow.locals.insert(place.local, None);
                    }
                    true
                } else {
                    false
                }
            }
            StatementKind::Assert { condition: value } | StatementKind::Evaluate(value) => {
                scan_expr(value, edges, flow, active_cleanups)
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                if !scan_expr(start, edges, flow, active_cleanups)
                    || !scan_expr(end, edges, flow, active_cleanups)
                {
                    false
                } else {
                    // The range may execute zero times, so the post-loop flow
                    // always includes the state after evaluating both bounds.
                    // A small monotone fixed point also exposes witness values
                    // that can reach calls and cleanups on later iterations.
                    let entry = flow.clone();
                    let mut loop_head = entry.clone();
                    loop {
                        let mut body_flow = loop_head.clone();
                        body_flow.locals.insert(*local, None);
                        if !scan_block_with_flow(body, edges, &mut body_flow, active_cleanups) {
                            break;
                        }
                        let next = merge_witness_flows([&entry, &body_flow]);
                        if next == loop_head {
                            break;
                        }
                        loop_head = next;
                    }
                    *flow = loop_head;
                    true
                }
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    let _ = scan_expr(value, edges, flow, active_cleanups);
                }
                // Every registered cleanup was already scanned with unknown
                // witness state. A Return has no continuation that could use
                // its exact post-cleanup flow, so replaying the whole active
                // stack for every returning branch only duplicates edges.
                false
            }
            StatementKind::Defer(cleanup) => {
                // A failure after registration can run the cleanup before any
                // particular normal exit state is reached. Scan that path
                // conservatively with unknown witness state, then keep
                // the cleanup active for exact continuing normal-exit scans.
                // Return needs no post-cleanup flow and uses this conservative
                // registration coverage instead.
                // A missing local and an explicitly unknown witness fact are
                // equivalent to `expression_witnesses`. Starting empty is
                // therefore conservative without cloning every live local at
                // each registration (which would make N defers over L locals
                // cost O(N * L)).
                let mut cleanup_flow = WitnessFlow::default();
                // Older cleanups were each scanned with unknown witness state
                // when registered. They do not need to be recursively replayed
                // for every newer registration.
                let _ = scan_block_with_flow(cleanup, edges, &mut cleanup_flow, &mut Vec::new());
                active_cleanups.push(cleanup);
                true
            }
        };
        if !continues {
            break;
        }
    }

    if continues && let Some(tail) = &block.tail {
        continues = scan_expr(tail, edges, flow, active_cleanups);
    }
    if continues && active_cleanups.len() > cleanup_base {
        continues = scan_cleanup_sequence(&active_cleanups[cleanup_base..], edges, flow);
    }
    active_cleanups.truncate(cleanup_base);
    continues
}

fn scan_cleanup_sequence(
    cleanups: &[&Block],
    edges: &mut FunctionEdges,
    flow: &mut WitnessFlow,
) -> bool {
    let mut continues = true;
    for cleanup in cleanups.iter().rev() {
        // The driver itself executes the complete LIFO sequence. Scanning a
        // cleanup with an inherited prefix would make each body recursively
        // replay every older body and turn a flat stack exponential.
        continues &= scan_block_with_flow(cleanup, edges, flow, &mut Vec::new());
    }
    continues
}

#[allow(clippy::too_many_lines)]
fn scan_expr<'mir>(
    expression: &'mir Expr,
    edges: &mut FunctionEdges,
    flow: &mut WitnessFlow,
    active_cleanups: &mut Vec<&'mir Block>,
) -> bool {
    match &expression.kind {
        ExprKind::Tuple(elements) | ExprKind::List(elements) => {
            for element in elements {
                if !scan_expr(element, edges, flow, active_cleanups) {
                    return false;
                }
            }
        }
        ExprKind::Unary(_, value) | ExprKind::Unrefine(value) | ExprKind::Refine { value, .. } => {
            if !scan_expr(value, edges, flow, active_cleanups) {
                return false;
            }
        }
        ExprKind::Binary(operator, left, right) => {
            if !scan_expr(left, edges, flow, active_cleanups) {
                return false;
            }
            if matches!(operator, BinaryOp::And | BinaryOp::Or) {
                // The short-circuit path retains the state after the left
                // operand even when evaluating the right operand diverges.
                let short_circuit_flow = flow.clone();
                let mut right_flow = short_circuit_flow.clone();
                if scan_expr(right, edges, &mut right_flow, active_cleanups) {
                    *flow = merge_witness_flows([&short_circuit_flow, &right_flow]);
                }
            } else if !scan_expr(right, edges, flow, active_cleanups) {
                return false;
            }
        }
        ExprKind::Block(block) => {
            let mut block_flow = flow.clone();
            if !scan_block_with_flow(block, edges, &mut block_flow, active_cleanups) {
                return false;
            }
            *flow = block_flow;
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if !scan_expr(condition, edges, flow, active_cleanups) {
                return false;
            }
            let mut then_flow = flow.clone();
            let mut else_flow = flow.clone();
            let then_continues =
                scan_block_with_flow(then_branch, edges, &mut then_flow, active_cleanups);
            let else_continues =
                scan_block_with_flow(else_branch, edges, &mut else_flow, active_cleanups);
            match (then_continues, else_continues) {
                (true, true) => *flow = merge_witness_flows([&then_flow, &else_flow]),
                (true, false) => *flow = then_flow,
                (false, true) => *flow = else_flow,
                (false, false) => return false,
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            if !scan_expr(scrutinee, edges, flow, active_cleanups) {
                return false;
            }
            let entry = flow.clone();
            let mut continuing = Vec::new();
            for arm in arms {
                let mut arm_flow = entry.clone();
                if scan_expr(&arm.value, edges, &mut arm_flow, active_cleanups) {
                    continuing.push(arm_flow);
                }
            }
            if continuing.is_empty() {
                return false;
            }
            *flow = merge_witness_flows(continuing.iter());
        }
        ExprKind::Record { fields, .. } => {
            for field in fields {
                if !scan_expr(field, edges, flow, active_cleanups) {
                    return false;
                }
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                if !scan_expr(value, edges, flow, active_cleanups) {
                    return false;
                }
            }
        }
        ExprKind::Call {
            target,
            arguments,
            witnesses,
            ..
        } => {
            let receiver_witnesses = match target {
                CallTarget::Dynamic { .. } => {
                    arguments.first().and_then(|argument| match argument {
                        CallArgument::Value(receiver) => expression_witnesses(receiver, flow),
                        CallArgument::InOut(place) if place.projection.is_empty() => {
                            flow.locals.get(&place.local).cloned().flatten()
                        }
                        CallArgument::InOut(_) => None,
                    })
                }
                _ => None,
            };
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => {
                        if !scan_expr(value, edges, flow, active_cleanups) {
                            return false;
                        }
                    }
                    CallArgument::InOut(place) => {
                        flow.locals.insert(place.local, None);
                    }
                }
            }
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
            for witness in witnesses {
                collect_witness(witness, &mut edges.witnesses);
            }
        }
        ExprKind::MakeView { value, witness, .. } => {
            if !scan_expr(value, edges, flow, active_cleanups) {
                return false;
            }
            collect_witness(witness, &mut edges.witnesses);
        }
        ExprKind::Await { task, .. } => {
            if !scan_expr(task, edges, flow, active_cleanups) {
                return false;
            }
        }
        ExprKind::TaskJoin { arguments, .. } => {
            for argument in arguments {
                if !scan_expr(argument, edges, flow, active_cleanups) {
                    return false;
                }
            }
        }
        ExprKind::Sleep { milliseconds } => {
            if !scan_expr(milliseconds, edges, flow, active_cleanups) {
                return false;
            }
        }
        ExprKind::WaitFd { descriptor, .. } => {
            if !scan_expr(descriptor, edges, flow, active_cleanups) {
                return false;
            }
        }
        ExprKind::Constant(_) | ExprKind::Copy(_) | ExprKind::ReborrowView { .. } => {}
        ExprKind::Move(place) => {
            flow.locals.remove(&place.local);
        }
    }
    expression.ty != Type::Never
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

fn merge_witness_flows<'a>(flows: impl IntoIterator<Item = &'a WitnessFlow>) -> WitnessFlow {
    let flows = flows.into_iter().collect::<Vec<_>>();
    let mut locals = BTreeMap::new();
    let keys = flows
        .iter()
        .flat_map(|flow| flow.locals.keys().copied())
        .collect::<BTreeSet<_>>();
    for local in keys {
        let mut witnesses = BTreeSet::new();
        let mut known = true;
        for flow in &flows {
            if let Some(Some(values)) = flow.locals.get(&local) {
                witnesses.extend(values.iter().copied());
            } else {
                known = false;
                break;
            }
        }
        locals.insert(local, known.then_some(witnesses));
    }
    WitnessFlow { locals }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::default_trait_access)]

    use super::*;
    use loom_mir::{ConceptId, ExprId, Place};

    const LOCAL: LocalId = LocalId(0);
    const FIRST: WitnessId = WitnessId(0);
    const SECOND: WitnessId = WitnessId(1);
    const REQUIREMENT: RequirementId = RequirementId(0);

    fn view_type() -> Type {
        Type::View {
            mutable: false,
            concept: ConceptId(0),
            bindings: BTreeMap::new(),
        }
    }

    fn unit() -> Expr {
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Constant(loom_mir::Constant::Unit),
            ty: Type::Unit,
            span: Default::default(),
        }
    }

    fn boolean(value: bool) -> Expr {
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Constant(loom_mir::Constant::Bool(value)),
            ty: Type::Bool,
            span: Default::default(),
        }
    }

    fn make_view(witness: WitnessId) -> Expr {
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::MakeView {
                value: Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(loom_mir::Constant::Int(1)),
                    ty: Type::Int,
                    span: Default::default(),
                }),
                writeback: None,
                witness: WitnessRef::Concrete(witness),
                mutable: false,
                token: witness.0,
            },
            ty: view_type(),
            span: Default::default(),
        }
    }

    fn dynamic_call_for(local: LocalId) -> Expr {
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Call {
                target: CallTarget::Dynamic {
                    requirement: REQUIREMENT,
                },
                type_arguments: Vec::new(),
                arguments: vec![CallArgument::Value(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Copy(Place::local(local)),
                    ty: view_type(),
                    span: Default::default(),
                })],
                witnesses: Vec::new(),
            },
            ty: Type::Unit,
            span: Default::default(),
        }
    }

    fn dynamic_call() -> Expr {
        dynamic_call_for(LOCAL)
    }

    fn statement(kind: StatementKind) -> loom_mir::Statement {
        loom_mir::Statement {
            kind,
            span: Default::default(),
        }
    }

    fn block(statements: Vec<loom_mir::Statement>) -> Block {
        Block {
            statements,
            tail: Some(Box::new(unit())),
            span: Default::default(),
        }
    }

    fn scan(statements: Vec<loom_mir::Statement>) -> FunctionEdges {
        let mut edges = FunctionEdges::default();
        scan_block(&block(statements), &mut edges);
        edges
    }

    fn initialize() -> loom_mir::Statement {
        statement(StatementKind::Let {
            local: LOCAL,
            value: make_view(FIRST),
        })
    }

    fn assign_second() -> loom_mir::Statement {
        statement(StatementKind::Assign {
            place: Place::local(LOCAL),
            value: make_view(SECOND),
        })
    }

    fn dynamic_cleanup() -> Block {
        block(vec![statement(StatementKind::Evaluate(dynamic_call()))])
    }

    fn direct_call(target: u32, ty: Type) -> Expr {
        Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Call {
                target: CallTarget::Direct(FunctionId(target)),
                type_arguments: Vec::new(),
                arguments: Vec::new(),
                witnesses: Vec::new(),
            },
            ty,
            span: Default::default(),
        }
    }

    fn cleanup_with_direct_and_dynamic_edges(target: u32) -> Block {
        block(vec![
            statement(StatementKind::Evaluate(direct_call(target, Type::Unit))),
            statement(StatementKind::Evaluate(dynamic_call())),
        ])
    }

    #[test]
    fn deferred_dispatch_uses_the_normal_exit_witness() {
        let edges = scan(vec![
            initialize(),
            statement(StatementKind::Defer(dynamic_cleanup())),
            assign_second(),
        ]);

        assert!(edges.concrete_methods.contains(&(SECOND, REQUIREMENT)));
    }

    #[test]
    fn tuple_binding_snapshots_witnesses_in_element_evaluation_order() {
        let first_binding = LocalId(1);
        let second_binding = LocalId(2);
        let mutating_element = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Block(block(vec![assign_second()])),
            ty: Type::Unit,
            span: Default::default(),
        };
        let copied_after_mutation = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Copy(Place::local(LOCAL)),
            ty: view_type(),
            span: Default::default(),
        };
        let tuple = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Tuple(vec![mutating_element, copied_after_mutation]),
            ty: Type::Tuple(vec![Type::Unit, view_type()]),
            span: Default::default(),
        };
        let edges = scan(vec![
            initialize(),
            statement(StatementKind::LetTuple {
                locals: vec![first_binding, second_binding],
                value: tuple,
            }),
            statement(StatementKind::Evaluate(dynamic_call_for(second_binding))),
        ]);

        assert_eq!(
            edges.concrete_methods,
            BTreeSet::from([(SECOND, REQUIREMENT)])
        );
    }

    #[test]
    fn deferred_cleanups_feed_newer_mutations_to_older_cleanups_lifo() {
        let edges = scan(vec![
            initialize(),
            statement(StatementKind::Defer(dynamic_cleanup())),
            statement(StatementKind::Defer(block(vec![assign_second()]))),
        ]);

        assert!(edges.concrete_methods.contains(&(SECOND, REQUIREMENT)));
    }

    #[test]
    fn flat_cleanup_stack_is_scanned_once_per_body() {
        // A flat cleanup stack is not recursive syntax. Prefix cloning and
        // inherited replay used to rescan every older cleanup for every newer
        // one (and then recursively rescan those prefixes at normal exit).
        let edges = scan(
            (0..1_024)
                .map(|_| statement(StatementKind::Defer(block(Vec::new()))))
                .collect(),
        );

        assert!(edges.direct.is_empty());
        assert!(edges.dynamic.is_empty());
    }

    #[test]
    fn registration_scan_covers_cleanup_edges_on_absorbed_direct_never_paths() {
        const CLEANUP_TARGET: u32 = 7;
        const DIVERGING_TARGET: u32 = 8;
        const FINAL_TARGET: u32 = 9;
        let short_circuit = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Binary(
                BinaryOp::And,
                Box::new(boolean(true)),
                Box::new(direct_call(DIVERGING_TARGET, Type::Never)),
            ),
            ty: Type::Bool,
            span: Default::default(),
        };
        let matched = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Match {
                scrutinee: Box::new(boolean(true)),
                arms: vec![
                    loom_mir::MatchArm {
                        pattern: loom_mir::Pattern::Constant(loom_mir::Constant::Bool(true)),
                        bindings: Vec::new(),
                        value: direct_call(DIVERGING_TARGET, Type::Never),
                    },
                    loom_mir::MatchArm {
                        pattern: loom_mir::Pattern::Constant(loom_mir::Constant::Bool(false)),
                        bindings: Vec::new(),
                        value: unit(),
                    },
                ],
            },
            ty: Type::Unit,
            span: Default::default(),
        };
        let edges = scan(vec![
            initialize(),
            statement(StatementKind::Defer(cleanup_with_direct_and_dynamic_edges(
                CLEANUP_TARGET,
            ))),
            statement(StatementKind::Evaluate(short_circuit)),
            statement(StatementKind::Evaluate(matched)),
            // Prevent the scanner's conservative continuing alternatives from
            // reaching a normal block exit. Cleanup edges therefore come from
            // the registration scan, including absorbed direct-Never paths.
            statement(StatementKind::Evaluate(direct_call(
                FINAL_TARGET,
                Type::Never,
            ))),
        ]);

        assert!(edges.direct.contains(&FunctionId(CLEANUP_TARGET)));
        assert!(edges.dynamic.contains(&REQUIREMENT));
        assert!(edges.witnesses.contains(&FIRST));
    }

    #[test]
    fn many_live_locals_do_not_multiply_flat_registration_cost() {
        const COUNT: u32 = 2_048;
        let mut statements = (0..COUNT)
            .map(|id| {
                statement(StatementKind::Let {
                    local: LocalId(id),
                    value: make_view(FIRST),
                })
            })
            .collect::<Vec<_>>();
        statements.extend((0..COUNT).map(|_| statement(StatementKind::Defer(block(Vec::new())))));

        let edges = scan(statements);

        assert_eq!(edges.witnesses, BTreeSet::from([FIRST]));
    }

    #[test]
    fn many_returning_arms_do_not_replay_the_active_cleanup_stack() {
        const COUNT: usize = 2_048;
        let mut statements = (0..COUNT)
            .map(|_| statement(StatementKind::Defer(block(Vec::new()))))
            .collect::<Vec<_>>();
        let arms = (0..COUNT)
            .map(|_| loom_mir::MatchArm {
                pattern: loom_mir::Pattern::Wildcard,
                bindings: Vec::new(),
                value: Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Block(Block {
                        statements: vec![statement(StatementKind::Return(Some(unit())))],
                        tail: None,
                        span: Default::default(),
                    }),
                    ty: Type::Never,
                    span: Default::default(),
                },
            })
            .collect();
        statements.push(statement(StatementKind::Evaluate(Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Match {
                scrutinee: Box::new(unit()),
                arms,
            },
            ty: Type::Never,
            span: Default::default(),
        })));

        let edges = scan(statements);

        assert!(edges.direct.is_empty());
        assert!(edges.dynamic.is_empty());
    }

    #[test]
    fn nested_normal_cleanup_updates_following_witness_flow() {
        let nested = block(vec![statement(StatementKind::Defer(block(vec![
            assign_second(),
        ])))]);
        let edges = scan(vec![
            initialize(),
            statement(StatementKind::Evaluate(Expr {
                id: ExprId::UNASSIGNED,
                kind: ExprKind::Block(nested),
                ty: Type::Unit,
                span: Default::default(),
            })),
            statement(StatementKind::Evaluate(dynamic_call())),
        ]);

        assert!(edges.concrete_methods.contains(&(SECOND, REQUIREMENT)));
    }

    #[test]
    fn returning_branches_keep_cleanup_dispatch_through_live_witnesses() {
        let returning = |assign: bool| Block {
            statements: assign
                .then(assign_second)
                .into_iter()
                .chain([statement(StatementKind::Return(Some(unit())))])
                .collect(),
            tail: None,
            span: Default::default(),
        };
        let branch = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::If {
                condition: Box::new(boolean(true)),
                then_branch: returning(true),
                else_branch: returning(false),
            },
            ty: Type::Never,
            span: Default::default(),
        };
        let edges = scan(vec![
            initialize(),
            statement(StatementKind::Defer(dynamic_cleanup())),
            statement(StatementKind::Evaluate(branch)),
        ]);

        // Return has no continuation that needs an exact post-cleanup flow.
        // Registration conservatively records the dynamic requirement, while
        // scanning the executable branches retains both witnesses that can
        // reach the cleanup at runtime.
        assert!(edges.dynamic.contains(&REQUIREMENT));
        assert!(edges.witnesses.contains(&FIRST));
        assert!(edges.witnesses.contains(&SECOND));
    }
}
