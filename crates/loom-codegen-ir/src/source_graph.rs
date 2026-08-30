use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
    rc::Rc,
};

use loom_mir::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, CheckedProgram, Expr, ExprId, ExprKind,
    FunctionId, LocalId, Program, RequirementId, StatementKind, Type, WitnessId, WitnessRef,
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
    /// Executable concrete-to-dynamic producer expressions by source function.
    #[serde(default)]
    pub dynamic_producers: BTreeMap<FunctionId, BTreeSet<ExprId>>,
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
    dynamic_producers: BTreeSet<ExprId>,
    dynamic: BTreeSet<RequirementId>,
    concrete_methods: BTreeSet<(WitnessId, RequirementId)>,
}

type WitnessTrie = Option<Rc<WitnessTrieNode>>;

#[derive(Debug)]
enum WitnessTrieNode {
    Branch { zero: WitnessTrie, one: WitnessTrie },
    Leaf(BTreeSet<WitnessId>),
}

/// Persistent sparse facts for witnesses proven to inhabit local values.
///
/// Missing locals and explicitly unknown witness facts are semantically
/// identical, so the trie stores only concrete non-empty facts. Branch copies
/// share their root and merges skip pointer-identical subtries. Updating one
/// local copies at most one 32-node radix path.
#[derive(Clone, Debug, Default)]
struct WitnessFlow {
    root: WitnessTrie,
}

struct WitnessLoopFlow {
    cleanup_base: usize,
    breaks: Vec<WitnessFlow>,
    continues: Vec<WitnessFlow>,
}

impl WitnessFlow {
    fn get(&self, local: LocalId) -> Option<&BTreeSet<WitnessId>> {
        witness_trie_get(&self.root, local.0)
    }

    fn set(&mut self, local: LocalId, witnesses: Option<BTreeSet<WitnessId>>) {
        self.root = match witnesses.filter(|witnesses| !witnesses.is_empty()) {
            Some(witnesses) => witness_trie_set(&self.root, local.0, Some(witnesses), 0),
            None => witness_trie_set(&self.root, local.0, None, 0),
        };
    }

    fn remove(&mut self, local: LocalId) {
        self.root = witness_trie_set(&self.root, local.0, None, 0);
    }

    fn merge(&self, other: &Self) -> Self {
        Self {
            root: merge_witness_tries(&self.root, &other.root, 0),
        }
    }

    fn same_root(&self, other: &Self) -> bool {
        witness_roots_equal(&self.root, &other.root)
    }
}

fn witness_trie_get(root: &WitnessTrie, key: u32) -> Option<&BTreeSet<WitnessId>> {
    let mut current = root.as_ref()?;
    for depth in 0..u32::BITS {
        let WitnessTrieNode::Branch { zero, one } = current.as_ref() else {
            return None;
        };
        let bit = u32::BITS - depth - 1;
        current = if key & (1_u32 << bit) == 0 {
            zero.as_ref()?
        } else {
            one.as_ref()?
        };
    }
    match current.as_ref() {
        WitnessTrieNode::Leaf(witnesses) => Some(witnesses),
        WitnessTrieNode::Branch { .. } => None,
    }
}

fn witness_trie_set(
    root: &WitnessTrie,
    key: u32,
    value: Option<BTreeSet<WitnessId>>,
    depth: u32,
) -> WitnessTrie {
    if depth == u32::BITS {
        return match value {
            None => None,
            Some(value) if matches!(root.as_deref(), Some(WitnessTrieNode::Leaf(current)) if current == &value) => {
                root.clone()
            }
            Some(value) => Some(witness_leaf(value)),
        };
    }

    let (zero, one) = witness_children(root);
    let bit = u32::BITS - depth - 1;
    let (next_zero, next_one) = if key & (1_u32 << bit) == 0 {
        (witness_trie_set(&zero, key, value, depth + 1), one)
    } else {
        (zero, witness_trie_set(&one, key, value, depth + 1))
    };
    if next_zero.is_none() && next_one.is_none() {
        return None;
    }
    if witness_children_equal(root, &next_zero, &next_one) {
        return root.clone();
    }
    Some(witness_branch(next_zero, next_one))
}

fn merge_witness_tries(left: &WitnessTrie, right: &WitnessTrie, depth: u32) -> WitnessTrie {
    if witness_roots_equal(left, right) {
        return left.clone();
    }
    let (Some(left_node), Some(right_node)) = (left.as_deref(), right.as_deref()) else {
        // A fact is known after a join only when every incoming path knows it.
        return None;
    };
    if depth == u32::BITS {
        let (WitnessTrieNode::Leaf(left_values), WitnessTrieNode::Leaf(right_values)) =
            (left_node, right_node)
        else {
            debug_assert!(false, "witness radix leaf depth contained a branch");
            return None;
        };
        if left_values == right_values {
            return left.clone();
        }
        let mut merged = left_values.clone();
        merged.extend(right_values.iter().copied());
        return if merged == *left_values {
            left.clone()
        } else if merged == *right_values {
            right.clone()
        } else {
            Some(witness_leaf(merged))
        };
    }

    let (left_zero, left_one) = witness_children(left);
    let (right_zero, right_one) = witness_children(right);
    let zero = merge_witness_tries(&left_zero, &right_zero, depth + 1);
    let one = merge_witness_tries(&left_one, &right_one, depth + 1);
    if zero.is_none() && one.is_none() {
        None
    } else if witness_children_equal(left, &zero, &one) {
        left.clone()
    } else if witness_children_equal(right, &zero, &one) {
        right.clone()
    } else {
        Some(witness_branch(zero, one))
    }
}

fn witness_children(root: &WitnessTrie) -> (WitnessTrie, WitnessTrie) {
    match root.as_deref() {
        Some(WitnessTrieNode::Branch { zero, one }) => (zero.clone(), one.clone()),
        Some(WitnessTrieNode::Leaf(_)) | None => (None, None),
    }
}

fn witness_children_equal(root: &WitnessTrie, zero: &WitnessTrie, one: &WitnessTrie) -> bool {
    let (current_zero, current_one) = witness_children(root);
    witness_roots_equal(&current_zero, zero) && witness_roots_equal(&current_one, one)
}

fn witness_roots_equal(left: &WitnessTrie, right: &WitnessTrie) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

fn witness_branch(zero: WitnessTrie, one: WitnessTrie) -> Rc<WitnessTrieNode> {
    debug_assert!(zero.is_some() || one.is_some());
    count_witness_node_allocation();
    Rc::new(WitnessTrieNode::Branch { zero, one })
}

fn witness_leaf(witnesses: BTreeSet<WitnessId>) -> Rc<WitnessTrieNode> {
    debug_assert!(!witnesses.is_empty());
    count_witness_node_allocation();
    Rc::new(WitnessTrieNode::Leaf(witnesses))
}

#[cfg(test)]
std::thread_local! {
    static WITNESS_NODE_ALLOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count_witness_node_allocation() {
    WITNESS_NODE_ALLOCATIONS.set(WITNESS_NODE_ALLOCATIONS.get() + 1);
}

#[cfg(test)]
fn witness_node_allocations() -> usize {
    WITNESS_NODE_ALLOCATIONS.get()
}

#[cfg(not(test))]
const fn count_witness_node_allocation() {}

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
            if !edges.dynamic_producers.is_empty() {
                result
                    .dynamic_producers
                    .entry(function_id)
                    .or_default()
                    .extend(edges.dynamic_producers.iter().copied());
            }
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
    let _ = scan_block_with_flow(
        block,
        edges,
        &mut WitnessFlow::default(),
        &mut Vec::new(),
        &mut Vec::new(),
    );
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
    loops: &mut Vec<WitnessLoopFlow>,
) -> bool {
    let cleanup_base = active_cleanups.len();
    let mut continues = true;
    for statement in &block.statements {
        continues = match &statement.kind {
            StatementKind::Let { local, value } => {
                let witnesses = expression_witnesses(value, flow);
                if scan_expr(value, edges, flow, active_cleanups, loops) {
                    flow.set(*local, witnesses);
                    true
                } else {
                    false
                }
            }
            StatementKind::Scoped {
                local,
                value,
                disposal,
            } => {
                let witnesses = expression_witnesses(value, flow);
                if scan_expr(value, edges, flow, active_cleanups, loops) {
                    flow.set(*local, witnesses);
                    let loom_mir::ScopedDisposal::StaticConcept {
                        requirement,
                        witness,
                        ..
                    } = disposal;
                    collect_witness(witness, &mut edges.witnesses);
                    if let Some(witness) = concrete_witness(witness) {
                        edges.concrete_methods.insert((witness, *requirement));
                    } else {
                        edges.dynamic.insert(*requirement);
                    }
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
                            if !scan_expr(element, edges, flow, active_cleanups, loops) {
                                continues = false;
                                break;
                            }
                        }
                        (continues && value.ty != Type::Never, Some(witnesses))
                    }
                    _ => (scan_expr(value, edges, flow, active_cleanups, loops), None),
                };
                if continues {
                    for (index, local) in locals.iter().enumerate() {
                        flow.set(
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
                if scan_expr(value, edges, flow, active_cleanups, loops) {
                    if place.projection.is_empty() {
                        flow.set(place.local, witnesses);
                    } else {
                        flow.remove(place.local);
                    }
                    true
                } else {
                    false
                }
            }
            StatementKind::Assert { condition: value } | StatementKind::Evaluate(value) => {
                scan_expr(value, edges, flow, active_cleanups, loops)
            }
            StatementKind::RestoreReceiverInvariant { .. } => true,
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                if !scan_expr(start, edges, flow, active_cleanups, loops)
                    || !scan_expr(end, edges, flow, active_cleanups, loops)
                {
                    false
                } else {
                    // The range may execute zero times, so the post-loop flow
                    // always includes the state after evaluating both bounds.
                    // A small monotone fixed point also exposes witness values
                    // that can reach calls and cleanups on later iterations.
                    let entry = flow.clone();
                    let mut loop_head = entry.clone();
                    let mut break_exit: Option<WitnessFlow> = None;
                    loop {
                        let mut body_flow = loop_head.clone();
                        body_flow.remove(*local);
                        loops.push(WitnessLoopFlow {
                            cleanup_base: active_cleanups.len(),
                            breaks: Vec::new(),
                            continues: Vec::new(),
                        });
                        let body_continues = scan_block_with_flow(
                            body,
                            edges,
                            &mut body_flow,
                            active_cleanups,
                            loops,
                        );
                        let loop_flow = loops.pop().expect("range loop flow");
                        for exit in loop_flow.breaks {
                            break_exit = Some(match break_exit {
                                Some(current) => current.merge(&exit),
                                None => exit,
                            });
                        }
                        let mut backedges = loop_flow.continues;
                        if body_continues {
                            backedges.push(body_flow);
                        }
                        let next = merge_witness_flows(
                            [&loop_head, &entry].into_iter().chain(backedges.iter()),
                        );
                        if next.same_root(&loop_head) {
                            break;
                        }
                        loop_head = next;
                    }
                    *flow = break_exit
                        .map_or(loop_head.clone(), |break_exit| loop_head.merge(&break_exit));
                    true
                }
            }
            StatementKind::While { condition, body } => {
                let entry = flow.clone();
                let mut loop_head = entry.clone();
                let mut natural_exit: Option<WitnessFlow> = None;
                let mut break_exit: Option<WitnessFlow> = None;
                loop {
                    let mut condition_flow = loop_head.clone();
                    if !scan_expr(
                        condition,
                        edges,
                        &mut condition_flow,
                        active_cleanups,
                        loops,
                    ) {
                        return false;
                    }
                    natural_exit = Some(match natural_exit {
                        Some(current) => current.merge(&condition_flow),
                        None => condition_flow.clone(),
                    });

                    let mut body_flow = condition_flow;
                    loops.push(WitnessLoopFlow {
                        cleanup_base: active_cleanups.len(),
                        breaks: Vec::new(),
                        continues: Vec::new(),
                    });
                    let body_continues =
                        scan_block_with_flow(body, edges, &mut body_flow, active_cleanups, loops);
                    let loop_flow = loops.pop().expect("while loop flow");
                    for exit in loop_flow.breaks {
                        break_exit = Some(match break_exit {
                            Some(current) => current.merge(&exit),
                            None => exit,
                        });
                    }
                    let mut backedges = loop_flow.continues;
                    if body_continues {
                        backedges.push(body_flow);
                    }
                    let next = merge_witness_flows(
                        [&loop_head, &entry].into_iter().chain(backedges.iter()),
                    );
                    if next.same_root(&loop_head) {
                        break;
                    }
                    loop_head = next;
                }
                let natural_exit = natural_exit.expect("while condition exit flow");
                *flow = break_exit.map_or(natural_exit.clone(), |break_exit| {
                    natural_exit.merge(&break_exit)
                });
                true
            }
            StatementKind::Break => {
                record_loop_flow(true, edges, flow, active_cleanups, loops);
                false
            }
            StatementKind::Continue => {
                record_loop_flow(false, edges, flow, active_cleanups, loops);
                false
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    let _ = scan_expr(value, edges, flow, active_cleanups, loops);
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
                let _ = scan_block_with_flow(
                    cleanup,
                    edges,
                    &mut cleanup_flow,
                    &mut Vec::new(),
                    &mut Vec::new(),
                );
                active_cleanups.push(cleanup);
                true
            }
        };
        if !continues {
            break;
        }
    }

    if continues && let Some(tail) = &block.tail {
        continues = scan_expr(tail, edges, flow, active_cleanups, loops);
    }
    if continues && active_cleanups.len() > cleanup_base {
        continues = scan_cleanup_sequence(&active_cleanups[cleanup_base..], edges, flow);
    }
    active_cleanups.truncate(cleanup_base);
    continues
}

fn record_loop_flow(
    is_break: bool,
    edges: &mut FunctionEdges,
    flow: &WitnessFlow,
    active_cleanups: &[&Block],
    loops: &mut [WitnessLoopFlow],
) {
    let Some(cleanup_base) = loops.last().map(|target| target.cleanup_base) else {
        return;
    };
    let mut exit_flow = flow.clone();
    if !scan_cleanup_sequence(&active_cleanups[cleanup_base..], edges, &mut exit_flow) {
        return;
    }
    let target = loops.last_mut().expect("checked loop target");
    if is_break {
        target.breaks.push(exit_flow);
    } else {
        target.continues.push(exit_flow);
    }
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
        continues &= scan_block_with_flow(cleanup, edges, flow, &mut Vec::new(), &mut Vec::new());
    }
    continues
}

#[allow(clippy::too_many_lines)]
fn scan_expr<'mir>(
    expression: &'mir Expr,
    edges: &mut FunctionEdges,
    flow: &mut WitnessFlow,
    active_cleanups: &mut Vec<&'mir Block>,
    loops: &mut Vec<WitnessLoopFlow>,
) -> bool {
    match &expression.kind {
        ExprKind::Tuple(elements) | ExprKind::List(elements) => {
            for element in elements {
                if !scan_expr(element, edges, flow, active_cleanups, loops) {
                    return false;
                }
            }
        }
        ExprKind::Unary(_, value) | ExprKind::Unrefine(value) | ExprKind::Refine { value, .. } => {
            if !scan_expr(value, edges, flow, active_cleanups, loops) {
                return false;
            }
        }
        ExprKind::Binary(operator, left, right) => {
            if !scan_expr(left, edges, flow, active_cleanups, loops) {
                return false;
            }
            if matches!(operator, BinaryOp::And | BinaryOp::Or) {
                // The short-circuit path retains the state after the left
                // operand even when evaluating the right operand diverges.
                let short_circuit_flow = flow.clone();
                let mut right_flow = short_circuit_flow.clone();
                if scan_expr(right, edges, &mut right_flow, active_cleanups, loops) {
                    *flow = merge_witness_flows([&short_circuit_flow, &right_flow]);
                }
            } else if !scan_expr(right, edges, flow, active_cleanups, loops) {
                return false;
            }
        }
        ExprKind::Block(block) => {
            let mut block_flow = flow.clone();
            if !scan_block_with_flow(block, edges, &mut block_flow, active_cleanups, loops) {
                return false;
            }
            *flow = block_flow;
        }
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            if !scan_expr(condition, edges, flow, active_cleanups, loops) {
                return false;
            }
            let mut then_flow = flow.clone();
            let mut else_flow = flow.clone();
            let then_continues =
                scan_block_with_flow(then_branch, edges, &mut then_flow, active_cleanups, loops);
            let else_continues =
                scan_block_with_flow(else_branch, edges, &mut else_flow, active_cleanups, loops);
            match (then_continues, else_continues) {
                (true, true) => *flow = merge_witness_flows([&then_flow, &else_flow]),
                (true, false) => *flow = then_flow,
                (false, true) => *flow = else_flow,
                (false, false) => return false,
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            if !scan_expr(scrutinee, edges, flow, active_cleanups, loops) {
                return false;
            }
            let entry = flow.clone();
            let mut continuing = Vec::new();
            for arm in arms {
                let mut arm_flow = entry.clone();
                if scan_expr(&arm.value, edges, &mut arm_flow, active_cleanups, loops) {
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
                if !scan_expr(field, edges, flow, active_cleanups, loops) {
                    return false;
                }
            }
        }
        ExprKind::Variant { payload, .. } => {
            for value in payload {
                if !scan_expr(value, edges, flow, active_cleanups, loops) {
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
                            flow.get(place.local).cloned()
                        }
                        CallArgument::InOut(_) => None,
                    })
                }
                _ => None,
            };
            for argument in arguments {
                match argument {
                    CallArgument::Value(value) => {
                        if !scan_expr(value, edges, flow, active_cleanups, loops) {
                            return false;
                        }
                    }
                    CallArgument::InOut(place) => {
                        flow.remove(place.local);
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
            if !scan_expr(value, edges, flow, active_cleanups, loops) {
                return false;
            }
            edges.dynamic_producers.insert(expression.id);
            collect_witness(witness, &mut edges.witnesses);
        }
        ExprKind::Await { task, .. } => {
            if !scan_expr(task, edges, flow, active_cleanups, loops) {
                return false;
            }
        }
        ExprKind::TaskJoin { arguments, .. } => {
            for argument in arguments {
                if !scan_expr(argument, edges, flow, active_cleanups, loops) {
                    return false;
                }
            }
        }
        ExprKind::Sleep { milliseconds } => {
            if !scan_expr(milliseconds, edges, flow, active_cleanups, loops) {
                return false;
            }
        }
        ExprKind::Constant(_) | ExprKind::Copy(_) | ExprKind::ReborrowView { .. } => {}
        ExprKind::Move(place) => {
            flow.remove(place.local);
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
            flow.get(place.local).cloned()
        }
        ExprKind::ReborrowView { owner, .. } if owner.projection.is_empty() => {
            flow.get(owner.local).cloned()
        }
        _ => None,
    }
}

fn merge_witness_flows<'a>(flows: impl IntoIterator<Item = &'a WitnessFlow>) -> WitnessFlow {
    let mut flows = flows.into_iter();
    let Some(first) = flows.next() else {
        return WitnessFlow::default();
    };
    flows.fold(first.clone(), |merged, flow| merged.merge(flow))
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
    fn loop_exit_and_backedge_witness_flows_reach_later_dynamic_calls() {
        let break_edges = scan(vec![
            initialize(),
            statement(StatementKind::While {
                condition: Box::new(boolean(true)),
                body: Box::new(block(vec![
                    statement(StatementKind::Defer(block(vec![assign_second()]))),
                    statement(StatementKind::Break),
                ])),
            }),
            statement(StatementKind::Evaluate(dynamic_call())),
        ]);
        assert!(
            break_edges
                .concrete_methods
                .contains(&(SECOND, REQUIREMENT)),
            "a deferred witness assignment on break must reach the loop exit"
        );

        let continue_edges = scan(vec![
            initialize(),
            statement(StatementKind::ForRange {
                local: LocalId(1),
                start: Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(loom_mir::Constant::Int(0)),
                    ty: Type::Int,
                    span: Default::default(),
                }),
                end: Box::new(Expr {
                    id: ExprId::UNASSIGNED,
                    kind: ExprKind::Constant(loom_mir::Constant::Int(2)),
                    ty: Type::Int,
                    span: Default::default(),
                }),
                body: Box::new(block(vec![
                    assign_second(),
                    statement(StatementKind::Continue),
                ])),
            }),
            statement(StatementKind::Evaluate(dynamic_call())),
        ]);
        assert!(
            continue_edges
                .concrete_methods
                .contains(&(SECOND, REQUIREMENT)),
            "a witness assignment on continue must reach later iterations and loop exit"
        );

        let condition = Expr {
            id: ExprId::UNASSIGNED,
            kind: ExprKind::Block(Block {
                statements: vec![statement(StatementKind::Evaluate(dynamic_call()))],
                tail: Some(Box::new(boolean(true))),
                span: Default::default(),
            }),
            ty: Type::Bool,
            span: Default::default(),
        };
        let condition_edges = scan(vec![
            initialize(),
            statement(StatementKind::While {
                condition: Box::new(condition),
                body: Box::new(block(vec![
                    assign_second(),
                    statement(StatementKind::Continue),
                ])),
            }),
        ]);
        assert!(
            condition_edges
                .concrete_methods
                .contains(&(SECOND, REQUIREMENT)),
            "a continue backedge must rescan the while condition with its witness flow"
        );
    }

    #[test]
    fn persistent_witness_flow_identity_joins_allocate_no_per_local_work() {
        const LOCAL_COUNT: u32 = 8_192;
        const IDENTITY_JOIN_COUNT: usize = 8_192;

        let allocations_before_population = witness_node_allocations();
        let mut flow = WitnessFlow::default();
        for raw in 0..LOCAL_COUNT {
            flow.set(LocalId(raw), Some(BTreeSet::from([FIRST])));
        }
        let allocations_after_population = witness_node_allocations();
        assert!(
            allocations_after_population - allocations_before_population
                <= usize::try_from(LOCAL_COUNT).expect("local count fits usize") * 33
        );

        for _ in 0..IDENTITY_JOIN_COUNT {
            let then_flow = flow.clone();
            let else_flow = flow.clone();
            let joined = merge_witness_flows([&then_flow, &else_flow]);
            assert!(joined.same_root(&flow));
            flow = joined;
        }
        assert_eq!(witness_node_allocations(), allocations_after_population);
    }

    #[test]
    fn scanner_identity_branches_share_all_known_local_witness_facts() {
        const LOCAL_COUNT: u32 = 8_192;
        const IDENTITY_BRANCH_COUNT: u32 = 8_192;

        let population = block(
            (0..LOCAL_COUNT)
                .map(|raw| {
                    statement(StatementKind::Let {
                        local: LocalId(raw),
                        value: make_view(FIRST),
                    })
                })
                .collect(),
        );
        let identity_branches = block(
            (0..IDENTITY_BRANCH_COUNT)
                .map(|index| {
                    let expression = if index & 1 == 0 {
                        Expr {
                            id: ExprId::UNASSIGNED,
                            kind: ExprKind::If {
                                condition: Box::new(boolean(true)),
                                then_branch: block(Vec::new()),
                                else_branch: block(Vec::new()),
                            },
                            ty: Type::Unit,
                            span: Default::default(),
                        }
                    } else {
                        Expr {
                            id: ExprId::UNASSIGNED,
                            kind: ExprKind::Binary(
                                BinaryOp::And,
                                Box::new(boolean(true)),
                                Box::new(boolean(true)),
                            ),
                            ty: Type::Bool,
                            span: Default::default(),
                        }
                    };
                    statement(StatementKind::Evaluate(expression))
                })
                .collect(),
        );

        let mut edges = FunctionEdges::default();
        let mut flow = WitnessFlow::default();
        assert!(scan_block_with_flow(
            &population,
            &mut edges,
            &mut flow,
            &mut Vec::new(),
            &mut Vec::new()
        ));
        let allocations_after_population = witness_node_allocations();
        assert!(scan_block_with_flow(
            &identity_branches,
            &mut edges,
            &mut flow,
            &mut Vec::new(),
            &mut Vec::new()
        ));

        assert_eq!(witness_node_allocations(), allocations_after_population);
        assert_eq!(flow.get(LocalId(0)), Some(&BTreeSet::from([FIRST])));
        assert_eq!(
            flow.get(LocalId(LOCAL_COUNT - 1)),
            Some(&BTreeSet::from([FIRST]))
        );
    }

    #[test]
    fn persistent_witness_flow_matches_a_small_map_reference() {
        fn update(
            flow: &mut WitnessFlow,
            reference: &mut BTreeMap<LocalId, BTreeSet<WitnessId>>,
            local: LocalId,
            witnesses: Option<BTreeSet<WitnessId>>,
        ) {
            flow.set(local, witnesses.clone());
            match witnesses.filter(|witnesses| !witnesses.is_empty()) {
                Some(witnesses) => {
                    reference.insert(local, witnesses);
                }
                None => {
                    reference.remove(&local);
                }
            }
        }

        fn assert_matches(
            flow: &WitnessFlow,
            reference: &BTreeMap<LocalId, BTreeSet<WitnessId>>,
            locals: &[LocalId],
        ) {
            for local in locals {
                assert_eq!(flow.get(*local), reference.get(local), "local #{}", local.0);
            }
        }

        let mut locals = (0..48).map(LocalId).collect::<Vec<_>>();
        locals.extend([LocalId(1_u32 << 31), LocalId(u32::MAX)]);
        let mut left = WitnessFlow::default();
        let mut right = WitnessFlow::default();
        let mut left_reference = BTreeMap::new();
        let mut right_reference = BTreeMap::new();
        let mut state = 0x6d2b_79f5_u32;
        for step in 0..512 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let local = locals[usize::try_from(state).expect("u32 fits usize") % locals.len()];
            let witnesses = match (state >> 8) & 3 {
                0 => None,
                1 => Some(BTreeSet::from([FIRST])),
                2 => Some(BTreeSet::from([SECOND])),
                _ => Some(BTreeSet::from([FIRST, SECOND])),
            };
            if state & 1 == 0 {
                update(&mut left, &mut left_reference, local, witnesses);
            } else {
                update(&mut right, &mut right_reference, local, witnesses);
            }
            if step % 17 == 0 {
                assert_matches(&left, &left_reference, &locals);
                assert_matches(&right, &right_reference, &locals);
            }
        }

        let merged = left.merge(&right);
        let merged_reference = left_reference
            .iter()
            .filter_map(|(local, left_witnesses)| {
                right_reference.get(local).map(|right_witnesses| {
                    let mut witnesses = left_witnesses.clone();
                    witnesses.extend(right_witnesses.iter().copied());
                    (*local, witnesses)
                })
            })
            .collect::<BTreeMap<_, _>>();
        assert_matches(&merged, &merged_reference, &locals);
        let fixed_point = merged.merge(&left);
        assert!(fixed_point.same_root(&merged));
        assert_matches(&fixed_point, &merged_reference, &locals);
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
