use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::{
    BinaryOp, Block, CallArgument, CallPlan, ConstructionMode, ContractExpr, ContractExprKind,
    ContractValue, Expr, ExprKind, LocalDecl, LocalId, MatchArm, Place, Receiver, Statement,
    StatementKind, Type, UnaryOp,
};

type NodeId = usize;
type CleanupId = usize;
type LiveTrieId = u32;
type LiveSet = u32;

const EMPTY_LIVE_SET: LiveSet = 0;

#[derive(Default)]
struct Node {
    uses: BTreeSet<LocalId>,
    defs: BTreeSet<LocalId>,
    successors: Vec<NodeId>,
    suspension: Option<u32>,
}

#[derive(Clone, Copy)]
struct LiveTrieNode {
    zero: LiveSet,
    one: LiveSet,
    count: usize,
}

/// Immutable sparse sets backed by one function-local radix-trie arena.
///
/// A dataflow node stores only a root id. A one-successor edge reuses that id,
/// while insertion/removal copies at most the 32 nodes on one `LocalId` path.
/// Pointer-identical subtries and memoized unions keep long cleanup suffixes
/// shared instead of materializing every growing prefix as an owned set.
struct LiveSetArena {
    nodes: Vec<LiveTrieNode>,
    leaf: LiveTrieId,
    union_cache: HashMap<(LiveTrieId, LiveTrieId), LiveTrieId>,
}

impl LiveSetArena {
    fn new() -> Self {
        let nodes = vec![LiveTrieNode {
            zero: EMPTY_LIVE_SET,
            one: EMPTY_LIVE_SET,
            count: 1,
        }];
        Self {
            nodes,
            // Trie ids are one-based so zero is the compact empty sentinel.
            leaf: 1,
            union_cache: HashMap::new(),
        }
    }

    fn count(&self, root: LiveSet) -> usize {
        if root == EMPTY_LIVE_SET {
            0
        } else {
            self.node(root).count
        }
    }

    fn node(&self, id: LiveTrieId) -> LiveTrieNode {
        debug_assert_ne!(id, EMPTY_LIVE_SET);
        let index = usize::try_from(id - 1).expect("u32 trie id fits usize");
        self.nodes[index]
    }

    fn insert(&mut self, root: LiveSet, local: LocalId) -> LiveSet {
        self.insert_at(root, local.0, 0)
    }

    fn insert_at(&mut self, root: LiveSet, key: u32, depth: u32) -> LiveSet {
        if depth == u32::BITS {
            return self.leaf;
        }
        let original = (root != EMPTY_LIVE_SET).then(|| self.node(root));
        let bit = u32::BITS - depth - 1;
        let goes_one = key & (1_u32 << bit) != 0;
        let child = original.map_or(
            EMPTY_LIVE_SET,
            |node| {
                if goes_one { node.one } else { node.zero }
            },
        );
        let updated = self.insert_at(child, key, depth + 1);
        if updated == child {
            return root;
        }
        let (zero, one) = original.map_or((EMPTY_LIVE_SET, EMPTY_LIVE_SET), |node| {
            (node.zero, node.one)
        });
        self.changed_branch(
            root,
            if goes_one { zero } else { updated },
            if goes_one { updated } else { one },
        )
    }

    fn remove(&mut self, root: LiveSet, local: LocalId) -> LiveSet {
        self.remove_at(root, local.0, 0)
    }

    fn remove_at(&mut self, root: LiveSet, key: u32, depth: u32) -> LiveSet {
        if root == EMPTY_LIVE_SET {
            return EMPTY_LIVE_SET;
        }
        if depth == u32::BITS {
            return EMPTY_LIVE_SET;
        }
        let original = self.node(root);
        let bit = u32::BITS - depth - 1;
        let goes_one = key & (1_u32 << bit) != 0;
        let child = if goes_one {
            original.one
        } else {
            original.zero
        };
        let updated = self.remove_at(child, key, depth + 1);
        if updated == child {
            return root;
        }
        self.changed_branch(
            root,
            if goes_one { original.zero } else { updated },
            if goes_one { updated } else { original.one },
        )
    }

    fn union(&mut self, left: LiveSet, right: LiveSet) -> LiveSet {
        if left == EMPTY_LIVE_SET {
            right
        } else if right == EMPTY_LIVE_SET {
            left
        } else {
            self.union_at(left, right)
        }
    }

    fn union_at(&mut self, mut left: LiveTrieId, mut right: LiveTrieId) -> LiveTrieId {
        if left == right {
            return left;
        }
        if left > right {
            std::mem::swap(&mut left, &mut right);
        }
        if let Some(cached) = self.union_cache.get(&(left, right)).copied() {
            return cached;
        }
        let left_node = self.node(left);
        let right_node = self.node(right);
        let zero = self.union(left_node.zero, right_node.zero);
        let one = self.union(left_node.one, right_node.one);
        let result = if zero == left_node.zero && one == left_node.one {
            left
        } else if zero == right_node.zero && one == right_node.one {
            right
        } else {
            let result = self.push_branch(zero, one);
            debug_assert_ne!(result, EMPTY_LIVE_SET);
            result
        };
        self.union_cache.insert((left, right), result);
        result
    }

    fn changed_branch(&mut self, original: LiveSet, zero: LiveSet, one: LiveSet) -> LiveSet {
        if original != EMPTY_LIVE_SET {
            let node = self.node(original);
            if node.zero == zero && node.one == one {
                return original;
            }
        }
        self.push_branch(zero, one)
    }

    fn push_branch(&mut self, zero: LiveSet, one: LiveSet) -> LiveSet {
        if zero == EMPTY_LIVE_SET && one == EMPTY_LIVE_SET {
            return EMPTY_LIVE_SET;
        }
        let count = self.count(zero) + self.count(one);
        let id = u32::try_from(self.nodes.len() + 1)
            .expect("liveness trie exhausted its u32 node-id domain");
        self.nodes.push(LiveTrieNode { zero, one, count });
        id
    }

    fn collect_sorted(&self, root: LiveSet) -> Vec<LocalId> {
        let mut locals = Vec::with_capacity(self.count(root));
        self.collect_at(root, 0, 0, &mut locals);
        locals
    }

    fn collect_at(&self, root: LiveSet, depth: u32, prefix: u32, locals: &mut Vec<LocalId>) {
        if root == EMPTY_LIVE_SET {
            return;
        }
        if depth == u32::BITS {
            locals.push(LocalId(prefix));
            return;
        }
        let node = self.node(root);
        self.collect_at(node.zero, depth + 1, prefix, locals);
        let bit = u32::BITS - depth - 1;
        self.collect_at(node.one, depth + 1, prefix | (1_u32 << bit), locals);
    }
}

#[derive(Clone, Copy)]
struct Cleanup<'mir> {
    action: CleanupAction<'mir>,
    older: Option<CleanupId>,
}

#[derive(Clone, Copy)]
enum CleanupAction<'mir> {
    Block(&'mir Block),
    Scoped(LocalId),
}

struct CfgBuilder<'mir> {
    nodes: Vec<Node>,
    exit: NodeId,
    cleanups: Vec<Cleanup<'mir>>,
    unwind_entries: Vec<Option<NodeId>>,
}

impl<'mir> CfgBuilder<'mir> {
    fn new() -> Self {
        Self::with_exit_uses([])
    }

    fn with_exit_uses(uses: impl IntoIterator<Item = LocalId>) -> Self {
        Self {
            nodes: vec![Node {
                uses: uses.into_iter().collect(),
                ..Node::default()
            }],
            exit: 0,
            cleanups: Vec::new(),
            unwind_entries: Vec::new(),
        }
    }

    fn node(
        &mut self,
        uses: impl IntoIterator<Item = LocalId>,
        defs: impl IntoIterator<Item = LocalId>,
        successors: impl IntoIterator<Item = NodeId>,
    ) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(Node {
            uses: uses.into_iter().collect(),
            defs: defs.into_iter().collect(),
            successors: successors.into_iter().collect(),
            suspension: None,
        });
        id
    }

    fn suspension_node(
        &mut self,
        state: u32,
        continuation: NodeId,
        active_cleanup: Option<CleanupId>,
    ) -> NodeId {
        // A parked coroutine may either resume normally or leave immediately
        // through cancellation/task failure. The latter path must preserve the
        // values read by every cleanup registered at this exact source point.
        let cancellation = self.build_unwind(active_cleanup);
        let mut successors = vec![continuation];
        if cancellation != continuation {
            successors.push(cancellation);
        }
        let id = self.node([], [], successors);
        self.nodes[id].suspension = Some(state);
        id
    }

    /// Insert an operation point that may either continue normally or unwind
    /// the cleanup suffix active after all of its operands were evaluated.
    /// With no active cleanup, the fault edge cannot make any local live and
    /// the normal continuation is already sufficient for suspension analysis.
    fn fault_continuation(
        &mut self,
        continuation: NodeId,
        active_cleanup: Option<CleanupId>,
    ) -> NodeId {
        let Some(active_cleanup) = active_cleanup else {
            return continuation;
        };
        let unwind = self.build_unwind(Some(active_cleanup));
        self.node([], [], [continuation, unwind])
    }

    /// Build one shared cleanup DAG for cancellation, Return, and Never exits.
    /// Cleanup ids always point to an older, smaller id. Missing suffixes can
    /// therefore be collected newest-to-oldest and materialized iteratively in
    /// the reverse order without recursive Rust calls or prefix cloning.
    fn build_unwind(&mut self, active_cleanup: Option<CleanupId>) -> NodeId {
        let mut missing = Vec::new();
        let mut cursor = active_cleanup;
        let mut entry = self.exit;
        while let Some(cleanup_id) = cursor {
            if let Some(cached) = self.unwind_entries.get(cleanup_id).copied().flatten() {
                entry = cached;
                break;
            }
            let Some(cleanup) = self.cleanups.get(cleanup_id).copied() else {
                debug_assert!(false, "cleanup id must address the builder arena");
                break;
            };
            debug_assert!(cleanup.older.is_none_or(|older| older < cleanup_id));
            missing.push(cleanup_id);
            cursor = cleanup.older;
        }
        for cleanup_id in missing.into_iter().rev() {
            let cleanup = self.cleanups[cleanup_id];
            entry = self.build_cleanup(cleanup, entry);
            self.unwind_entries[cleanup_id] = Some(entry);
        }
        entry
    }

    fn build_block(
        &mut self,
        block: &'mir Block,
        continuation: NodeId,
        outer_cleanup: Option<CleanupId>,
    ) -> NodeId {
        let mut active_cleanup = outer_cleanup;
        let mut block_cleanups = Vec::new();
        for statement in &block.statements {
            let action = match &statement.kind {
                StatementKind::Defer(cleanup) => Some(CleanupAction::Block(cleanup)),
                StatementKind::Scoped { local, .. } => Some(CleanupAction::Scoped(*local)),
                _ => None,
            };
            if let Some(action) = action {
                let cleanup_id = self.cleanups.len();
                debug_assert!(active_cleanup.is_none_or(|older| older < cleanup_id));
                self.cleanups.push(Cleanup {
                    action,
                    older: active_cleanup,
                });
                self.unwind_entries.push(None);
                active_cleanup = Some(cleanup_id);
                block_cleanups.push(cleanup_id);
            }
        }

        let normal_exit = self.build_normal_cleanups(&block_cleanups, continuation);

        let mut entry = block.tail.as_deref().map_or(normal_exit, |tail| {
            self.build_expr(tail, normal_exit, active_cleanup)
        });

        for statement in block.statements.iter().rev() {
            if matches!(
                &statement.kind,
                StatementKind::Defer(_) | StatementKind::Scoped { .. }
            ) {
                let Some(cleanup_id) = active_cleanup else {
                    continue;
                };
                let registered = self.cleanups[cleanup_id];
                match (&statement.kind, registered.action) {
                    (StatementKind::Defer(cleanup), CleanupAction::Block(registered)) => {
                        debug_assert!(std::ptr::eq(registered, cleanup));
                    }
                    (StatementKind::Scoped { local, .. }, CleanupAction::Scoped(registered)) => {
                        debug_assert_eq!(*local, registered);
                    }
                    _ => debug_assert!(false, "registered cleanup kind changed"),
                }
                active_cleanup = registered.older;
                if matches!(&statement.kind, StatementKind::Defer(_)) {
                    continue;
                }
            }
            entry = self.build_statement(statement, entry, active_cleanup);
        }
        entry
    }

    fn build_normal_cleanups(&mut self, cleanups: &[CleanupId], continuation: NodeId) -> NodeId {
        let mut entry = continuation;
        for cleanup_id in cleanups {
            // Cleanups execute in LIFO order. Building oldest-to-newest in CPS
            // makes the newest cleanup the resulting entry. A malformed return
            // in a cleanup sees only older registrations, avoiding a self-edge;
            // MIR validation rejects such control flow independently.
            let cleanup = self.cleanups[*cleanup_id];
            entry = self.build_cleanup(cleanup, entry);
        }
        entry
    }

    fn build_cleanup(&mut self, cleanup: Cleanup<'mir>, continuation: NodeId) -> NodeId {
        match cleanup.action {
            CleanupAction::Block(block) => self.build_block(block, continuation, cleanup.older),
            // Dispose is an inout use. Both its normal and fault exits continue
            // with the same older cleanup suffix, so one liveness successor is
            // sufficient while still keeping the resource live.
            CleanupAction::Scoped(local) => self.node([local], [local], [continuation]),
        }
    }

    fn build_statement(
        &mut self,
        statement: &'mir Statement,
        continuation: NodeId,
        active_cleanup: Option<CleanupId>,
    ) -> NodeId {
        match &statement.kind {
            StatementKind::Let { local, value } | StatementKind::Scoped { local, value, .. } => {
                let store = self.node([], [*local], [continuation]);
                self.build_expr(value, store, active_cleanup)
            }
            StatementKind::LetTuple { locals, value } => {
                let store = self.node([], locals.iter().copied(), [continuation]);
                self.build_expr(value, store, active_cleanup)
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let decision = self.node([], [], [continuation]);
                let body_entry = self.build_block(body, decision, active_cleanup);
                let iteration = self.node([], [*local], [body_entry]);
                self.nodes[decision].successors.push(iteration);
                let end_entry = self.build_expr(end, decision, active_cleanup);
                self.build_expr(start, end_entry, active_cleanup)
            }
            StatementKind::Assign { place, value } => {
                let store = if place.projection.is_empty() {
                    self.node([], [place.local], [continuation])
                } else {
                    self.node([place.local], [], [continuation])
                };
                self.build_expr(value, store, active_cleanup)
            }
            StatementKind::Assert { condition } => {
                let assertion = self.fault_continuation(continuation, active_cleanup);
                self.build_expr(condition, assertion, active_cleanup)
            }
            StatementKind::Evaluate(expression) => {
                self.build_expr(expression, continuation, active_cleanup)
            }
            StatementKind::Defer(_) => continuation,
            StatementKind::Return(value) => {
                let return_exit = self.build_unwind(active_cleanup);
                value.as_ref().map_or(return_exit, |value| {
                    self.build_expr(value, return_exit, active_cleanup)
                })
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn build_expr(
        &mut self,
        expression: &'mir Expr,
        continuation: NodeId,
        active_cleanup: Option<CleanupId>,
    ) -> NodeId {
        let continuation = if expression.ty == Type::Never {
            self.build_unwind(active_cleanup)
        } else {
            continuation
        };
        match &expression.kind {
            ExprKind::Constant(_) => continuation,
            ExprKind::Tuple(elements) | ExprKind::List(elements) => {
                self.build_exprs(elements, continuation, active_cleanup)
            }
            ExprKind::Copy(place) => self.place_read(place, continuation),
            ExprKind::Move(place) => self.node([place.local], [place.local], [continuation]),
            ExprKind::Unary(operator, operand) => {
                let operation = if *operator == UnaryOp::Negate && expression.ty == Type::Int {
                    self.fault_continuation(continuation, active_cleanup)
                } else {
                    continuation
                };
                self.build_expr(operand, operation, active_cleanup)
            }
            ExprKind::Unrefine(operand) => self.build_expr(operand, continuation, active_cleanup),
            ExprKind::Binary(operator, left, right) => {
                if matches!(operator, BinaryOp::And | BinaryOp::Or) {
                    let right = self.build_expr(right, continuation, active_cleanup);
                    // A short-circuiting left operand can reach the
                    // continuation without executing RHS definitions. That
                    // edge is essential when computing values that must be
                    // preserved across a suspension in the left operand.
                    let decision = self.node([], [], [continuation, right]);
                    self.build_expr(left, decision, active_cleanup)
                } else {
                    let operation = if expression.ty == Type::Int
                        && matches!(
                            operator,
                            BinaryOp::Add
                                | BinaryOp::Subtract
                                | BinaryOp::Multiply
                                | BinaryOp::Divide
                        ) {
                        self.fault_continuation(continuation, active_cleanup)
                    } else {
                        continuation
                    };
                    let right = self.build_expr(right, operation, active_cleanup);
                    self.build_expr(left, right, active_cleanup)
                }
            }
            ExprKind::Block(block) => self.build_block(block, continuation, active_cleanup),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_entry = self.build_block(then_branch, continuation, active_cleanup);
                let else_entry = self.build_block(else_branch, continuation, active_cleanup);
                let branch = self.node([], [], [then_entry, else_entry]);
                self.build_expr(condition, branch, active_cleanup)
            }
            ExprKind::Match { scrutinee, arms } => {
                let arm_entries = arms
                    .iter()
                    .map(|arm| self.build_match_arm(arm, continuation, active_cleanup))
                    .collect::<Vec<_>>();
                let branch = self.node([], [], arm_entries);
                self.build_expr(scrutinee, branch, active_cleanup)
            }
            ExprKind::Record {
                fields,
                construction,
                ..
            } => {
                let construction = if matches!(
                    construction,
                    ConstructionMode::Recheck | ConstructionMode::Runtime
                ) {
                    self.fault_continuation(continuation, active_cleanup)
                } else {
                    continuation
                };
                self.build_exprs(fields, construction, active_cleanup)
            }
            ExprKind::Variant { payload, .. } => {
                self.build_exprs(payload, continuation, active_cleanup)
            }
            ExprKind::Refine {
                value,
                construction,
                ..
            } => {
                let construction = if matches!(
                    construction,
                    ConstructionMode::Recheck | ConstructionMode::Runtime
                ) {
                    self.fault_continuation(continuation, active_cleanup)
                } else {
                    continuation
                };
                self.build_expr(value, construction, active_cleanup)
            }
            ExprKind::Call { arguments, .. } => {
                let inout = arguments.iter().filter_map(|argument| match argument {
                    CallArgument::InOut(place) => Some(place.local),
                    CallArgument::Value(_) => None,
                });
                // An InOut call both consumes the current value and defines its
                // post-call value. Keeping both sets models the write without
                // losing the mandatory pre-call read.
                let uses = inout.clone().collect::<Vec<_>>();
                let call_continuation = if expression.ty == Type::Never {
                    continuation
                } else {
                    self.fault_continuation(continuation, active_cleanup)
                };
                let call = self.node(
                    uses.iter().copied(),
                    uses.iter().copied(),
                    [call_continuation],
                );
                arguments.iter().rev().fold(call, |next, argument| {
                    if let CallArgument::Value(value) = argument {
                        self.build_expr(value, next, active_cleanup)
                    } else {
                        next
                    }
                })
            }
            ExprKind::MakeView {
                value, writeback, ..
            } => {
                let next = writeback
                    .as_ref()
                    .map_or(continuation, |place| self.place_read(place, continuation));
                self.build_expr(value, next, active_cleanup)
            }
            ExprKind::ReborrowView { owner, .. } => self.place_read(owner, continuation),
            ExprKind::Await { state, task } => {
                let suspend = self.suspension_node(*state, continuation, active_cleanup);
                self.build_expr(task, suspend, active_cleanup)
            }
            ExprKind::Sleep { milliseconds } => {
                let sleep = self.fault_continuation(continuation, active_cleanup);
                self.build_expr(milliseconds, sleep, active_cleanup)
            }
            ExprKind::TaskJoin { arguments, .. } => {
                let join = self.fault_continuation(continuation, active_cleanup);
                self.build_exprs(arguments, join, active_cleanup)
            }
        }
    }

    fn build_match_arm(
        &mut self,
        arm: &'mir MatchArm,
        continuation: NodeId,
        active_cleanup: Option<CleanupId>,
    ) -> NodeId {
        let value = self.build_expr(&arm.value, continuation, active_cleanup);
        self.node([], arm.bindings.iter().copied(), [value])
    }

    fn build_exprs(
        &mut self,
        expressions: &'mir [Expr],
        continuation: NodeId,
        active_cleanup: Option<CleanupId>,
    ) -> NodeId {
        expressions.iter().rev().fold(continuation, |next, value| {
            self.build_expr(value, next, active_cleanup)
        })
    }

    fn place_read(&mut self, place: &Place, continuation: NodeId) -> NodeId {
        self.node([place.local], [], [continuation])
    }
}

fn joined_successors(sets: &mut LiveSetArena, node: &Node, live_in: &[LiveSet]) -> LiveSet {
    let mut successors = node.successors.iter();
    let mut joined = successors
        .next()
        .map_or(EMPTY_LIVE_SET, |successor| live_in[*successor]);
    for successor in successors {
        joined = sets.union(joined, live_in[*successor]);
    }
    joined
}

fn solve_liveness(builder: &CfgBuilder<'_>) -> (BTreeMap<u32, Vec<LocalId>>, (usize, usize)) {
    let mut sets = LiveSetArena::new();
    let mut live_in = vec![EMPTY_LIVE_SET; builder.nodes.len()];

    // Revisit only predecessors of a node whose live-in set changed. The CPS
    // builder generally numbers a predecessor after its continuation, so a
    // repeated reverse full-table scan would move information only one edge
    // per pass along a long cleanup chain.
    let mut predecessors = vec![Vec::new(); builder.nodes.len()];
    for (id, node) in builder.nodes.iter().enumerate() {
        for successor in &node.successors {
            if let Some(incoming) = predecessors.get_mut(*successor) {
                incoming.push(id);
            }
        }
    }
    let mut pending = (0..builder.nodes.len()).collect::<VecDeque<_>>();
    let mut queued = vec![true; builder.nodes.len()];
    while let Some(id) = pending.pop_front() {
        queued[id] = false;
        let node = &builder.nodes[id];
        let mut next_in = joined_successors(&mut sets, node, &live_in);
        for local in &node.defs {
            next_in = sets.remove(next_in, *local);
        }
        for local in &node.uses {
            next_in = sets.insert(next_in, *local);
        }
        let old_count = sets.count(live_in[id]);
        let next_count = sets.count(next_in);
        // This fresh solve starts from bottom and only propagates additions.
        // Every transfer input therefore grows monotonically. Equal cardinality
        // means equal contents even when independently built but structurally
        // equivalent trie roots have different ids.
        debug_assert!(old_count <= next_count);
        if old_count != next_count {
            live_in[id] = next_in;
            for predecessor in &predecessors[id] {
                if !queued[*predecessor] {
                    queued[*predecessor] = true;
                    pending.push_back(*predecessor);
                }
            }
        }
    }

    let mut result = BTreeMap::<u32, LiveSet>::new();
    for node in &builder.nodes {
        if let Some(state) = node.suspension {
            let live_out = joined_successors(&mut sets, node, &live_in);
            let prior = result.get(&state).copied().unwrap_or(EMPTY_LIVE_SET);
            result.insert(state, sets.union(prior, live_out));
        }
    }
    let stats = (sets.nodes.len(), sets.union_cache.len());
    let result = result
        .into_iter()
        .map(|(state, locals)| (state, sets.collect_sorted(locals)))
        .collect();
    (result, stats)
}

/// Computes the locals whose current values must survive each async suspension.
///
/// The result is the least fixed point over a structured control-flow graph.
/// A suspension's set describes resume/cancellation liveness, so locals used
/// only to construct its task operand are deliberately absent. Values needed by
/// registered lexical cleanups are retained through the corresponding await.
/// Each vector is strictly sorted by [`LocalId`] and contains no duplicates.
#[must_use]
pub fn analyze_suspension_liveness(body: &Block) -> BTreeMap<u32, Vec<LocalId>> {
    let mut builder = CfgBuilder::new();
    let _entry = builder.build_block(body, builder.exit, None);
    solve_liveness(&builder).0
}

/// Returns the exact parameter locals read by normal-exit contracts.
///
/// A receiver, when present, occupies parameter slot zero. Contract argument
/// indices address only the explicit parameter suffix. Invalid indices are
/// ignored here and remain the independent MIR validator's responsibility.
/// The result is strictly sorted and contains no duplicates.
#[must_use]
pub fn exit_contract_parameter_locals(
    params: &[LocalDecl],
    receiver: Option<Receiver>,
    call_plan: &CallPlan,
) -> Vec<LocalId> {
    let mut pending = Vec::<&ContractExpr>::new();
    if let Some(invariant) = &call_plan.receiver_invariant {
        pending.push(&invariant.expression);
    }
    pending.extend(
        call_plan
            .ensures
            .iter()
            .map(|contract| &contract.expression),
    );

    let receiver_offset = usize::from(receiver.is_some());
    let mut locals = BTreeSet::new();
    while let Some(expression) = pending.pop() {
        match &expression.kind {
            ContractExprKind::Value(value) => {
                let parameter = match value {
                    ContractValue::SelfValue | ContractValue::OldSelf => {
                        receiver.and_then(|_| params.first())
                    }
                    ContractValue::Argument(index) | ContractValue::OldArgument(index) => {
                        usize::try_from(*index)
                            .ok()
                            .and_then(|index| receiver_offset.checked_add(index))
                            .and_then(|index| params.get(index))
                    }
                    ContractValue::Result => None,
                };
                if let Some(parameter) = parameter {
                    locals.insert(parameter.id);
                }
            }
            ContractExprKind::Field(owner, _)
            | ContractExprKind::Unary(_, owner)
            | ContractExprKind::IsFinite(owner) => pending.push(owner),
            ContractExprKind::Binary(_, left, right) => {
                pending.push(right);
                pending.push(left);
            }
            ContractExprKind::Match { scrutinee, arms } => {
                for arm in arms.iter().rev() {
                    pending.push(&arm.value);
                }
                pending.push(scrutinee);
            }
            ContractExprKind::Constant(_) | ContractExprKind::Binding(_) => {}
        }
    }
    locals.into_iter().collect()
}

/// Computes suspension liveness including only parameters actually referenced
/// by the receiver invariant or postconditions executed on normal exit.
///
/// This is the canonical contract-aware query used both when constructing MIR
/// suspension metadata and when independently validating serialized MIR.
#[must_use]
pub fn analyze_suspension_liveness_with_exit_contracts(
    body: &Block,
    params: &[LocalDecl],
    receiver: Option<Receiver>,
    call_plan: &CallPlan,
) -> BTreeMap<u32, Vec<LocalId>> {
    let exit_uses = exit_contract_parameter_locals(params, receiver, call_plan);
    let mut builder = CfgBuilder::with_exit_uses(exit_uses);
    let _entry = builder.build_block(body, builder.exit, None);
    solve_liveness(&builder).0
}

#[cfg(test)]
mod tests {
    use loom_core::Span;

    use super::*;
    use crate::{
        CallTarget, Constant, Contract, Expr, ExprKind, FunctionId, Pattern, ScopedDisposal,
        Statement, TaskJoinMode, TypeId,
    };

    fn expression(kind: ExprKind, ty: Type) -> Expr {
        Expr::new(kind, ty, Span::default())
    }

    fn sleep_await(state: u32) -> Expr {
        expression(
            ExprKind::Await {
                state,
                task: Box::new(expression(
                    ExprKind::Sleep {
                        milliseconds: Box::new(expression(
                            ExprKind::Constant(Constant::Int(0)),
                            Type::Int,
                        )),
                    },
                    Type::Task(Box::new(Type::Unit)),
                )),
            },
            Type::Unit,
        )
    }

    #[test]
    fn sparse_live_sets_match_btree_sets_and_cache_symmetric_unions() {
        let keys = [0, 1, 17, 1_u32 << 31, u32::MAX];
        let mut arena = LiveSetArena::new();
        let mut root = EMPTY_LIVE_SET;
        let mut reference = BTreeSet::new();
        for key in keys {
            root = arena.insert(root, LocalId(key));
            reference.insert(LocalId(key));
            assert_eq!(arena.count(root), reference.len());
            assert_eq!(
                arena.collect_sorted(root),
                reference.iter().copied().collect::<Vec<_>>()
            );
        }
        for key in [17, 0, u32::MAX] {
            root = arena.remove(root, LocalId(key));
            reference.remove(&LocalId(key));
            assert_eq!(arena.count(root), reference.len());
            assert_eq!(
                arena.collect_sorted(root),
                reference.iter().copied().collect::<Vec<_>>()
            );
        }

        let mut left = EMPTY_LIVE_SET;
        let mut right = EMPTY_LIVE_SET;
        let mut expected = BTreeSet::new();
        for key in [0, 17, 1_u32 << 31] {
            left = arena.insert(left, LocalId(key));
            expected.insert(LocalId(key));
        }
        for key in [1, 17, u32::MAX] {
            right = arena.insert(right, LocalId(key));
            expected.insert(LocalId(key));
        }
        let union = arena.union(left, right);
        let nodes_after_first = arena.nodes.len();
        let cache_after_first = arena.union_cache.len();
        let reversed = arena.union(right, left);
        let repeated = arena.union(left, right);
        assert_eq!(union, reversed);
        assert_eq!(union, repeated);
        assert_eq!(arena.nodes.len(), nodes_after_first);
        assert_eq!(arena.union_cache.len(), cache_after_first);
        assert_eq!(
            arena.collect_sorted(union),
            expected.into_iter().collect::<Vec<_>>()
        );
    }

    fn evaluate(value: Expr) -> Statement {
        Statement {
            kind: StatementKind::Evaluate(value),
            span: Span::default(),
        }
    }

    fn defer(cleanup: Block) -> Statement {
        Statement {
            kind: StatementKind::Defer(cleanup),
            span: Span::default(),
        }
    }

    fn read_cleanup(local: LocalId) -> Block {
        Block {
            statements: vec![evaluate(expression(
                ExprKind::Copy(Place::local(local)),
                Type::Int,
            ))],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        }
    }

    fn fault_then_overwrite_cleanup(faulting: Statement, overwritten: LocalId) -> Block {
        Block {
            statements: vec![
                faulting,
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(overwritten),
                        value: expression(ExprKind::Constant(Constant::Int(2)), Type::Int),
                    },
                    span: Span::default(),
                },
            ],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        }
    }

    fn body_with_faulting_newer_cleanup(faulting: Statement) -> Block {
        let value = LocalId(0);
        Block {
            statements: vec![
                defer(read_cleanup(value)),
                defer(fault_then_overwrite_cleanup(faulting, value)),
                evaluate(sleep_await(1)),
            ],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        }
    }

    #[test]
    fn exit_contracts_keep_only_the_parameters_they_reference_live() {
        let parameter = |id, name: &str| LocalDecl {
            id: LocalId(id),
            name: name.to_owned(),
            ty: Type::Int,
            mutable: false,
            span: Span::default(),
        };
        let value = |value| ContractExpr {
            kind: ContractExprKind::Value(value),
            span: Span::default(),
        };
        let contract = |expression| Contract {
            code: "test.contract".to_owned(),
            span: Span::default(),
            expression,
        };
        let params = vec![
            parameter(0, "self"),
            parameter(1, "unused"),
            parameter(2, "current"),
            parameter(3, "old"),
        ];
        let call_plan = CallPlan {
            receiver_invariant: Some(contract(value(ContractValue::SelfValue))),
            requires: vec![contract(value(ContractValue::Argument(0)))],
            ensures: vec![contract(ContractExpr {
                kind: ContractExprKind::Binary(
                    BinaryOp::And,
                    Box::new(value(ContractValue::Argument(1))),
                    Box::new(ContractExpr {
                        kind: ContractExprKind::Binary(
                            BinaryOp::And,
                            Box::new(value(ContractValue::OldSelf)),
                            Box::new(value(ContractValue::OldArgument(2))),
                        ),
                        span: Span::default(),
                    }),
                ),
                span: Span::default(),
            })],
        };
        let body = Block {
            statements: vec![evaluate(sleep_await(1))],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        let expected = [LocalId(0), LocalId(2), LocalId(3)];
        assert_eq!(
            exit_contract_parameter_locals(&params, Some(Receiver::Readonly), &call_plan),
            expected
        );
        let liveness = analyze_suspension_liveness_with_exit_contracts(
            &body,
            &params,
            Some(Receiver::Readonly),
            &call_plan,
        );
        assert_eq!(
            liveness[&1], expected,
            "requires-only and unreferenced parameters must not enter the frame"
        );
    }

    #[test]
    fn task_operands_and_killed_values_do_not_leak_into_later_states() {
        let body = Block {
            statements: vec![
                Statement {
                    kind: StatementKind::Evaluate(expression(
                        ExprKind::Await {
                            state: 1,
                            task: Box::new(expression(
                                ExprKind::Copy(Place::local(LocalId(0))),
                                Type::Task(Box::new(Type::Unit)),
                            )),
                        },
                        Type::Unit,
                    )),
                    span: Span::default(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expression(
                        ExprKind::Move(Place::local(LocalId(2))),
                        Type::Text,
                    )),
                    span: Span::default(),
                },
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(LocalId(4)),
                        value: expression(ExprKind::Constant(Constant::Int(9)), Type::Int),
                    },
                    span: Span::default(),
                },
                Statement {
                    kind: StatementKind::Evaluate(expression(
                        ExprKind::Call {
                            target: CallTarget::Direct(FunctionId(0)),
                            type_arguments: Vec::new(),
                            arguments: vec![CallArgument::InOut(Place::local(LocalId(3)))],
                            witnesses: Vec::new(),
                        },
                        Type::Unit,
                    )),
                    span: Span::default(),
                },
                Statement {
                    kind: StatementKind::Let {
                        local: LocalId(5),
                        value: expression(ExprKind::Copy(Place::local(LocalId(1))), Type::Int),
                    },
                    span: Span::default(),
                },
                Statement {
                    kind: StatementKind::Evaluate(sleep_await(2)),
                    span: Span::default(),
                },
            ],
            tail: Some(Box::new(expression(
                ExprKind::Copy(Place::local(LocalId(5))),
                Type::Int,
            ))),
            span: Span::default(),
        };

        let liveness = analyze_suspension_liveness(&body);
        assert_eq!(liveness[&1], [LocalId(1), LocalId(2), LocalId(3)]);
        assert_eq!(liveness[&2], [LocalId(5)]);
    }

    #[test]
    fn short_circuit_path_keeps_pre_rhs_value_live_across_await() {
        let task = LocalId(0);
        let value = LocalId(1);
        for operator in [BinaryOp::And, BinaryOp::Or] {
            let body = Block {
                statements: vec![
                    Statement {
                        kind: StatementKind::Evaluate(expression(
                            ExprKind::Binary(
                                operator,
                                Box::new(expression(
                                    ExprKind::Await {
                                        state: 1,
                                        task: Box::new(expression(
                                            ExprKind::Copy(Place::local(task)),
                                            Type::Task(Box::new(Type::Bool)),
                                        )),
                                    },
                                    Type::Bool,
                                )),
                                Box::new(expression(
                                    ExprKind::Block(Block {
                                        statements: vec![Statement {
                                            kind: StatementKind::Assign {
                                                place: Place::local(value),
                                                value: expression(
                                                    ExprKind::Constant(Constant::Int(1)),
                                                    Type::Int,
                                                ),
                                            },
                                            span: Span::default(),
                                        }],
                                        tail: Some(Box::new(expression(
                                            ExprKind::Constant(Constant::Bool(true)),
                                            Type::Bool,
                                        ))),
                                        span: Span::default(),
                                    }),
                                    Type::Bool,
                                )),
                            ),
                            Type::Bool,
                        )),
                        span: Span::default(),
                    },
                    Statement {
                        kind: StatementKind::Evaluate(expression(
                            ExprKind::Copy(Place::local(value)),
                            Type::Int,
                        )),
                        span: Span::default(),
                    },
                ],
                tail: Some(Box::new(expression(
                    ExprKind::Constant(Constant::Unit),
                    Type::Unit,
                ))),
                span: Span::default(),
            };

            let liveness = analyze_suspension_liveness(&body);
            assert_eq!(liveness[&1], [value], "operator {operator:?}");
        }
    }

    #[test]
    fn faulting_newer_cleanup_keeps_value_needed_by_older_cleanup_live() {
        let value = LocalId(0);
        let condition = LocalId(1);
        let faulting_assert = Statement {
            kind: StatementKind::Assert {
                condition: expression(ExprKind::Copy(Place::local(condition)), Type::Bool),
            },
            span: Span::default(),
        };

        let liveness =
            analyze_suspension_liveness(&body_with_faulting_newer_cleanup(faulting_assert));
        assert_eq!(liveness[&1], [value, condition]);
    }

    #[test]
    fn every_runtime_fault_family_has_a_cleanup_unwind_edge() {
        let int = || expression(ExprKind::Constant(Constant::Int(1)), Type::Int);
        let runtime_ty = TypeId(0);
        let faulting_expressions = vec![
            expression(ExprKind::Unary(UnaryOp::Negate, Box::new(int())), Type::Int),
            expression(
                ExprKind::Binary(BinaryOp::Add, Box::new(int()), Box::new(int())),
                Type::Int,
            ),
            expression(
                ExprKind::Record {
                    ty: runtime_ty,
                    type_arguments: Vec::new(),
                    fields: Vec::new(),
                    construction: ConstructionMode::Runtime,
                },
                Type::Nominal(runtime_ty, Vec::new()),
            ),
            expression(
                ExprKind::Refine {
                    ty: runtime_ty,
                    value: Box::new(int()),
                    construction: ConstructionMode::Runtime,
                },
                Type::Nominal(runtime_ty, Vec::new()),
            ),
            expression(
                ExprKind::Call {
                    target: CallTarget::Direct(FunctionId(0)),
                    type_arguments: Vec::new(),
                    arguments: Vec::new(),
                    witnesses: Vec::new(),
                },
                Type::Unit,
            ),
            expression(
                ExprKind::Sleep {
                    milliseconds: Box::new(int()),
                },
                Type::Task(Box::new(Type::Unit)),
            ),
            expression(
                ExprKind::TaskJoin {
                    mode: TaskJoinMode::All,
                    arguments: Vec::new(),
                },
                Type::Task(Box::new(Type::Tuple(Vec::new()))),
            ),
        ];

        for faulting in faulting_expressions {
            let name = format!("{:?}", faulting.kind);
            let body = body_with_faulting_newer_cleanup(evaluate(faulting));
            assert!(
                analyze_suspension_liveness(&body)[&1].contains(&LocalId(0)),
                "faulting expression {name} must preserve the older cleanup input"
            );
        }
    }

    #[test]
    fn long_cleanup_chain_propagates_one_live_local_with_a_worklist() {
        const COUNT: usize = 8_192;
        let captured = LocalId(0);
        let mut statements = Vec::with_capacity(COUNT + 1);
        statements.extend((0..COUNT).map(|_| defer(read_cleanup(captured))));
        statements.push(evaluate(sleep_await(1)));
        let body = Block {
            statements,
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        assert_eq!(analyze_suspension_liveness(&body)[&1], [captured]);
    }

    #[test]
    fn each_await_observes_only_its_registered_cleanup_prefix() {
        let first = LocalId(0);
        let second = LocalId(1);
        let body = Block {
            statements: vec![
                defer(read_cleanup(first)),
                evaluate(sleep_await(1)),
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(second),
                        value: expression(ExprKind::Constant(Constant::Int(2)), Type::Int),
                    },
                    span: Span::default(),
                },
                defer(read_cleanup(second)),
                evaluate(sleep_await(2)),
            ],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        let liveness = analyze_suspension_liveness(&body);
        assert_eq!(liveness[&1], [first]);
        assert_eq!(liveness[&2], [first, second]);
    }

    #[test]
    fn scoped_initializer_suspension_does_not_register_its_own_cleanup() {
        let older = LocalId(0);
        let initializing = LocalId(1);
        let body = Block {
            statements: vec![
                defer(read_cleanup(older)),
                Statement {
                    kind: StatementKind::Scoped {
                        local: initializing,
                        value: sleep_await(1),
                        disposal: ScopedDisposal::FileClose,
                    },
                    span: Span::default(),
                },
            ],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        assert_eq!(analyze_suspension_liveness(&body)[&1], [older]);
    }

    #[test]
    fn interleaved_defers_and_awaits_share_cleanup_suffix_cfg() {
        const COUNT: usize = 2_048;
        const CAPTURED: [LocalId; 4] = [LocalId(0), LocalId(1), LocalId(2), LocalId(3)];
        let mut statements = Vec::with_capacity(COUNT * 2);
        let state_count = u32::try_from(COUNT).expect("test state count fits u32");
        for state in 1..=state_count {
            let index = usize::try_from(state - 1).expect("u32 state fits usize") % CAPTURED.len();
            statements.push(defer(read_cleanup(CAPTURED[index])));
            statements.push(evaluate(sleep_await(state)));
        }
        let body = Block {
            statements,
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        let mut builder = CfgBuilder::new();
        let _entry = builder.build_block(&body, builder.exit, None);
        let (liveness, stats) = solve_liveness(&builder);
        assert_eq!(liveness.len(), COUNT);
        assert!(liveness.values().all(|locals| locals == &CAPTURED));
        assert_eq!(builder.cleanups.len(), COUNT);
        assert!(
            builder.nodes.len() < COUNT * 10,
            "shared cleanup suffixes must keep the CFG linear, found {} nodes for {COUNT} defers/awaits",
            builder.nodes.len()
        );
        assert!(
            stats.0 < COUNT,
            "identical live sets must share sparse-trie roots, found {} nodes",
            stats.0
        );
        assert!(
            stats.1 < COUNT * 12,
            "repeated small unions must keep a linear cache, found {} entries",
            stats.1
        );
    }

    #[test]
    fn fifty_thousand_distinct_cleanup_captures_use_linear_sparse_storage() {
        const COUNT: usize = 50_000;
        let mut statements = Vec::with_capacity(COUNT + 1);
        for index in 0..COUNT {
            let local = LocalId(u32::try_from(index).expect("test local id fits u32"));
            statements.push(defer(read_cleanup(local)));
        }
        statements.push(evaluate(sleep_await(1)));
        let body = Block {
            statements,
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        let mut builder = CfgBuilder::new();
        let _entry = builder.build_block(&body, builder.exit, None);
        let (liveness, stats) = solve_liveness(&builder);
        let locals = &liveness[&1];
        assert_eq!(locals.len(), COUNT);
        assert_eq!(locals.first(), Some(&LocalId(0)));
        assert_eq!(
            locals.last(),
            Some(&LocalId(
                u32::try_from(COUNT - 1).expect("test local id fits u32")
            ))
        );
        assert!(
            stats.0 < COUNT * 70,
            "persistent sparse sets must allocate O(LocalId::BITS * N), found {} nodes",
            stats.0
        );
        assert!(
            stats.1 < COUNT * 4,
            "distinct cleanup unions must keep a linear cache, found {} entries",
            stats.1
        );
    }

    #[test]
    fn loop_with_defs_and_uses_reaches_a_stable_sparse_fixed_point() {
        let carried = LocalId(0);
        let after_loop = LocalId(1);
        let induction = LocalId(2);
        let body = Block {
            statements: vec![
                evaluate(sleep_await(1)),
                Statement {
                    kind: StatementKind::ForRange {
                        local: induction,
                        start: Box::new(expression(
                            ExprKind::Constant(Constant::Int(0)),
                            Type::Int,
                        )),
                        end: Box::new(expression(ExprKind::Constant(Constant::Int(4)), Type::Int)),
                        body: Box::new(Block {
                            statements: vec![
                                evaluate(expression(
                                    ExprKind::Copy(Place::local(carried)),
                                    Type::Int,
                                )),
                                Statement {
                                    kind: StatementKind::Assign {
                                        place: Place::local(carried),
                                        value: expression(
                                            ExprKind::Constant(Constant::Int(1)),
                                            Type::Int,
                                        ),
                                    },
                                    span: Span::default(),
                                },
                            ],
                            tail: Some(Box::new(expression(
                                ExprKind::Constant(Constant::Unit),
                                Type::Unit,
                            ))),
                            span: Span::default(),
                        }),
                    },
                    span: Span::default(),
                },
                evaluate(expression(
                    ExprKind::Copy(Place::local(after_loop)),
                    Type::Int,
                )),
            ],
            tail: Some(Box::new(expression(
                ExprKind::Constant(Constant::Unit),
                Type::Unit,
            ))),
            span: Span::default(),
        };

        assert_eq!(
            analyze_suspension_liveness(&body)[&1],
            [carried, after_loop]
        );
    }

    #[test]
    fn many_returns_share_one_cleanup_unwind_cfg() {
        const COUNT: usize = 2_048;
        let captured = LocalId(0);
        let mut statements = (0..COUNT)
            .map(|_| defer(read_cleanup(captured)))
            .collect::<Vec<_>>();
        let arms = (0..COUNT)
            .map(|value| MatchArm {
                pattern: Pattern::Constant(Constant::Int(
                    i64::try_from(value).expect("test match value fits i64"),
                )),
                bindings: Vec::new(),
                value: expression(
                    ExprKind::Block(Block {
                        statements: vec![Statement {
                            kind: StatementKind::Return(None),
                            span: Span::default(),
                        }],
                        tail: None,
                        span: Span::default(),
                    }),
                    Type::Never,
                ),
            })
            .collect();
        statements.push(evaluate(expression(
            ExprKind::Match {
                scrutinee: Box::new(expression(ExprKind::Constant(Constant::Int(0)), Type::Int)),
                arms,
            },
            Type::Never,
        )));
        let body = Block {
            statements,
            tail: None,
            span: Span::default(),
        };

        let mut builder = CfgBuilder::new();
        let _entry = builder.build_block(&body, builder.exit, None);
        assert_eq!(builder.cleanups.len(), COUNT);
        assert!(
            builder.nodes.len() < COUNT * 8,
            "Return exits must reuse one unwind DAG, found {} nodes for {COUNT} defers/returns",
            builder.nodes.len()
        );
    }
}
