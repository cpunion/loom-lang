use std::collections::{BTreeMap, BTreeSet};

use crate::{
    BinaryOp, Block, CallArgument, Expr, ExprKind, LocalId, MatchArm, Place, Statement,
    StatementKind, Type,
};

type NodeId = usize;

#[derive(Default)]
struct Node {
    uses: BTreeSet<LocalId>,
    defs: BTreeSet<LocalId>,
    successors: Vec<NodeId>,
    suspension: Option<u32>,
}

struct CfgBuilder {
    nodes: Vec<Node>,
    exit: NodeId,
}

impl CfgBuilder {
    fn new() -> Self {
        Self {
            nodes: vec![Node::default()],
            exit: 0,
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
        active_cleanups: &[&Block],
    ) -> NodeId {
        // A parked coroutine may either resume normally or leave immediately
        // through cancellation/task failure. The latter path must preserve the
        // values read by every cleanup registered at this exact source point.
        let cancellation = self.build_cleanup_sequence(active_cleanups, self.exit, &[]);
        let mut successors = vec![continuation];
        if cancellation != continuation {
            successors.push(cancellation);
        }
        let id = self.node([], [], successors);
        self.nodes[id].suspension = Some(state);
        id
    }

    fn build_block(
        &mut self,
        block: &Block,
        continuation: NodeId,
        outer_cleanups: &[&Block],
    ) -> NodeId {
        let block_cleanups = block
            .statements
            .iter()
            .filter_map(|statement| match &statement.kind {
                StatementKind::Defer(cleanup) => Some(cleanup),
                _ => None,
            })
            .collect::<Vec<_>>();

        let normal_exit =
            self.build_cleanup_sequence(&block_cleanups, continuation, outer_cleanups);
        let mut active_cleanups = Vec::with_capacity(outer_cleanups.len() + block_cleanups.len());
        active_cleanups.extend_from_slice(outer_cleanups);
        active_cleanups.extend(block_cleanups.iter().copied());

        let mut entry = block.tail.as_deref().map_or(normal_exit, |tail| {
            self.build_expr(tail, normal_exit, &active_cleanups)
        });

        for statement in block.statements.iter().rev() {
            if let StatementKind::Defer(cleanup) = &statement.kind {
                let removed = active_cleanups.pop();
                debug_assert!(removed.is_some_and(|candidate| std::ptr::eq(candidate, cleanup)));
                continue;
            }
            entry = self.build_statement(statement, entry, &active_cleanups);
        }
        entry
    }

    fn build_cleanup_sequence(
        &mut self,
        cleanups: &[&Block],
        continuation: NodeId,
        outer_cleanups: &[&Block],
    ) -> NodeId {
        let mut entry = continuation;
        let mut older = Vec::with_capacity(outer_cleanups.len() + cleanups.len());
        older.extend_from_slice(outer_cleanups);
        for cleanup in cleanups {
            // Cleanups execute in LIFO order. Building oldest-to-newest in CPS
            // makes the newest cleanup the resulting entry. A malformed return
            // in a cleanup sees only older registrations, avoiding a self-edge;
            // MIR validation rejects such control flow independently.
            entry = self.build_block(cleanup, entry, &older);
            older.push(cleanup);
        }
        entry
    }

    fn build_statement(
        &mut self,
        statement: &Statement,
        continuation: NodeId,
        active_cleanups: &[&Block],
    ) -> NodeId {
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let store = self.node([], [*local], [continuation]);
                self.build_expr(value, store, active_cleanups)
            }
            StatementKind::LetTuple { locals, value } => {
                let store = self.node([], locals.iter().copied(), [continuation]);
                self.build_expr(value, store, active_cleanups)
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let decision = self.node([], [], [continuation]);
                let body_entry = self.build_block(body, decision, active_cleanups);
                let iteration = self.node([], [*local], [body_entry]);
                self.nodes[decision].successors.push(iteration);
                let end_entry = self.build_expr(end, decision, active_cleanups);
                self.build_expr(start, end_entry, active_cleanups)
            }
            StatementKind::Assign { place, value } => {
                let store = if place.projection.is_empty() {
                    self.node([], [place.local], [continuation])
                } else {
                    self.node([place.local], [], [continuation])
                };
                self.build_expr(value, store, active_cleanups)
            }
            StatementKind::Assert { condition } => {
                self.build_expr(condition, continuation, active_cleanups)
            }
            StatementKind::Evaluate(expression) => {
                self.build_expr(expression, continuation, active_cleanups)
            }
            StatementKind::Defer(_) => continuation,
            StatementKind::Return(value) => {
                let return_exit = self.build_cleanup_sequence(active_cleanups, self.exit, &[]);
                value.as_ref().map_or(return_exit, |value| {
                    self.build_expr(value, return_exit, active_cleanups)
                })
            }
        }
    }

    fn build_expr(
        &mut self,
        expression: &Expr,
        continuation: NodeId,
        active_cleanups: &[&Block],
    ) -> NodeId {
        let continuation = if expression.ty == Type::Never {
            self.build_cleanup_sequence(active_cleanups, self.exit, &[])
        } else {
            continuation
        };
        match &expression.kind {
            ExprKind::Constant(_) => continuation,
            ExprKind::Tuple(elements) | ExprKind::List(elements) => {
                self.build_exprs(elements, continuation, active_cleanups)
            }
            ExprKind::Copy(place) => self.place_read(place, continuation),
            ExprKind::Move(place) => self.node([place.local], [place.local], [continuation]),
            ExprKind::Unary(_, operand) | ExprKind::Unrefine(operand) => {
                self.build_expr(operand, continuation, active_cleanups)
            }
            ExprKind::Binary(operator, left, right) => {
                let right = self.build_expr(right, continuation, active_cleanups);
                if matches!(operator, BinaryOp::And | BinaryOp::Or) {
                    // A short-circuiting left operand can reach the
                    // continuation without executing RHS definitions. That
                    // edge is essential when computing values that must be
                    // preserved across a suspension in the left operand.
                    let decision = self.node([], [], [continuation, right]);
                    self.build_expr(left, decision, active_cleanups)
                } else {
                    self.build_expr(left, right, active_cleanups)
                }
            }
            ExprKind::Block(block) => self.build_block(block, continuation, active_cleanups),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let then_entry = self.build_block(then_branch, continuation, active_cleanups);
                let else_entry = self.build_block(else_branch, continuation, active_cleanups);
                let branch = self.node([], [], [then_entry, else_entry]);
                self.build_expr(condition, branch, active_cleanups)
            }
            ExprKind::Match { scrutinee, arms } => {
                let arm_entries = arms
                    .iter()
                    .map(|arm| self.build_match_arm(arm, continuation, active_cleanups))
                    .collect::<Vec<_>>();
                let branch = self.node([], [], arm_entries);
                self.build_expr(scrutinee, branch, active_cleanups)
            }
            ExprKind::Record { fields, .. } => {
                self.build_exprs(fields, continuation, active_cleanups)
            }
            ExprKind::Variant { payload, .. } => {
                self.build_exprs(payload, continuation, active_cleanups)
            }
            ExprKind::Refine { value, .. } => self.build_expr(value, continuation, active_cleanups),
            ExprKind::Call { arguments, .. } => {
                let inout = arguments.iter().filter_map(|argument| match argument {
                    CallArgument::InOut(place) => Some(place.local),
                    CallArgument::Value(_) => None,
                });
                // An InOut call both consumes the current value and defines its
                // post-call value. Keeping both sets models the write without
                // losing the mandatory pre-call read.
                let uses = inout.clone().collect::<Vec<_>>();
                let call = self.node(uses.iter().copied(), uses.iter().copied(), [continuation]);
                arguments.iter().rev().fold(call, |next, argument| {
                    if let CallArgument::Value(value) = argument {
                        self.build_expr(value, next, active_cleanups)
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
                self.build_expr(value, next, active_cleanups)
            }
            ExprKind::ReborrowView { owner, .. } => self.place_read(owner, continuation),
            ExprKind::Await { state, task } => {
                let suspend = self.suspension_node(*state, continuation, active_cleanups);
                self.build_expr(task, suspend, active_cleanups)
            }
            ExprKind::Sleep { milliseconds } => {
                self.build_expr(milliseconds, continuation, active_cleanups)
            }
            ExprKind::WaitFd { descriptor, .. } => {
                self.build_expr(descriptor, continuation, active_cleanups)
            }
            ExprKind::TaskJoin { arguments, .. } => {
                self.build_exprs(arguments, continuation, active_cleanups)
            }
        }
    }

    fn build_match_arm(
        &mut self,
        arm: &MatchArm,
        continuation: NodeId,
        active_cleanups: &[&Block],
    ) -> NodeId {
        let value = self.build_expr(&arm.value, continuation, active_cleanups);
        self.node([], arm.bindings.iter().copied(), [value])
    }

    fn build_exprs(
        &mut self,
        expressions: &[Expr],
        continuation: NodeId,
        active_cleanups: &[&Block],
    ) -> NodeId {
        expressions.iter().rev().fold(continuation, |next, value| {
            self.build_expr(value, next, active_cleanups)
        })
    }

    fn place_read(&mut self, place: &Place, continuation: NodeId) -> NodeId {
        self.node([place.local], [], [continuation])
    }
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
    let _entry = builder.build_block(body, builder.exit, &[]);
    let mut live_in = vec![BTreeSet::new(); builder.nodes.len()];
    let mut live_out = vec![BTreeSet::new(); builder.nodes.len()];

    loop {
        let mut changed = false;
        for (id, node) in builder.nodes.iter().enumerate().rev() {
            let mut next_out = BTreeSet::new();
            for successor in &node.successors {
                next_out.extend(live_in[*successor].iter().copied());
            }
            let mut next_in = next_out.clone();
            for local in &node.defs {
                next_in.remove(local);
            }
            next_in.extend(node.uses.iter().copied());
            if next_out != live_out[id] {
                live_out[id] = next_out;
                changed = true;
            }
            if next_in != live_in[id] {
                live_in[id] = next_in;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut result = BTreeMap::<u32, BTreeSet<LocalId>>::new();
    for (id, node) in builder.nodes.iter().enumerate() {
        if let Some(state) = node.suspension {
            result
                .entry(state)
                .or_default()
                .extend(live_out[id].iter().copied());
        }
    }
    result
        .into_iter()
        .map(|(state, locals)| (state, locals.into_iter().collect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use loom_core::Span;

    use super::*;
    use crate::{CallTarget, Constant, Expr, ExprKind, FunctionId, Statement};

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
}
