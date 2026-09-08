//! Conservative last-read clearing for local shadow roots, not expression snapshots.
//!
//! Lowered blocks are not source scopes: cleanup and destructuring can read their
//! locals later. Pass the actual continuation backwards through every operand.
//! Definitions never kill reads, and loops retain all reads across the backedge.
//! Clearing matters only before a possible later collection, not frame exit.

use crate::model::checked::{Block, Expr, ExprKind, Function, Stmt, StmtKind};
use std::collections::{BTreeSet, HashMap};

type Locals = BTreeSet<usize>;

#[derive(Default)]
pub(super) struct Plan {
    pub after_requires: Vec<usize>,
    pub after_statement: HashMap<*const Stmt, Vec<usize>>,
    pub after_block: HashMap<*const Block, Vec<usize>>,
}

pub(super) fn plan(
    function: &Function,
    rooted: impl Iterator<Item = usize>,
    allocating: &BTreeSet<usize>,
) -> Plan {
    let rooted = rooted.collect::<Locals>();
    if rooted.is_empty() {
        return Plan::default();
    }
    let mut analysis = Analysis {
        rooted,
        allocating,
        plan: Plan::default(),
    };
    let mut live = Locals::new();
    // No collection follows function fallthrough before the frame is detached.
    let mut future = false;
    analysis.block(&function.body, &mut live, &mut future);
    if future {
        analysis.plan.after_requires = analysis
            .rooted
            .difference(&live)
            .copied()
            .filter(|local| *local < function.params.len())
            .collect();
    }
    for requirement in function.requires.iter().rev() {
        analysis.expr(requirement, &mut live, &mut future);
    }
    analysis.plan
}

struct Analysis<'a> {
    rooted: Locals,
    allocating: &'a BTreeSet<usize>,
    plan: Plan,
}

impl Analysis<'_> {
    fn dead(&self, touched: &Locals, live: &Locals) -> Vec<usize> {
        touched.difference(live).copied().collect()
    }

    /// Updates `live` to the input read set and returns all reads/definitions.
    fn block(&mut self, block: &Block, live: &mut Locals, future: &mut bool) -> Locals {
        let out = live.clone();
        let out_allocates = *future;
        let mut touched = block
            .tail
            .as_ref()
            .map_or_else(Locals::new, |tail| self.expr(tail, live, future));
        // Completed statements already clear their dead inputs/definitions.
        // Only a tail has no statement boundary of its own.
        let dead = self.dead(&touched, &out);
        if out_allocates && !dead.is_empty() {
            self.plan.after_block.insert(block, dead);
        }
        for statement in block.statements.iter().rev() {
            let after = live.clone();
            let after_allocates = *future;
            let used = self.statement(statement, live, future);
            let dead = self.dead(&used, &after);
            if after_allocates && !dead.is_empty() {
                self.plan.after_statement.insert(statement, dead);
            }
            touched.extend(used);
        }
        touched
    }

    fn statement(&mut self, statement: &Stmt, live: &mut Locals, future: &mut bool) -> Locals {
        match &statement.kind {
            StmtKind::Let { local, value } | StmtKind::Assign { local, value } => {
                let mut touched = self.expr(value, live, future);
                if self.rooted.contains(local) {
                    touched.insert(*local);
                }
                touched
            }
            StmtKind::Return(Some(value))
            | StmtKind::Assert(value)
            | StmtKind::Discard(value)
            | StmtKind::Expr(value) => self.expr(value, live, future),
            StmtKind::Return(None) => Locals::new(),
            StmtKind::While { condition, body } => {
                // No break/continue in this IR. Retaining every loop read avoids
                // a dataflow fixpoint while protecting condition/body backedges.
                read_block(body, live, &self.rooted, self.allocating, future);
                read_expr(condition, live, &self.rooted, self.allocating, future);
                let mut touched = self.block(body, live, future);
                touched.extend(self.expr(condition, live, future));
                touched
            }
        }
    }

    fn operands<'a>(
        &mut self,
        values: impl DoubleEndedIterator<Item = &'a Expr>,
        live: &mut Locals,
        future: &mut bool,
    ) -> Locals {
        let mut touched = Locals::new();
        for value in values.rev() {
            touched.extend(self.expr(value, live, future));
        }
        touched
    }

    fn expr(&mut self, expr: &Expr, live: &mut Locals, future: &mut bool) -> Locals {
        match &expr.kind {
            ExprKind::Local(local) => {
                if !self.rooted.contains(local) {
                    return Locals::new();
                }
                live.insert(*local);
                Locals::from([*local])
            }
            ExprKind::Primitive(operation, values) => {
                *future |= super::gc::allocates(*operation);
                self.operands(values.iter(), live, future)
            }
            ExprKind::Call(target, values) => {
                *future |= self.allocating.contains(target);
                self.operands(values.iter(), live, future)
            }
            ExprKind::Variant { fields: values, .. } => self.operands(values.iter(), live, future),
            ExprKind::Unary(_, value) | ExprKind::Coerce(value) | ExprKind::Field(value, _) => {
                self.expr(value, live, future)
            }
            ExprKind::DynBox { value, .. } => {
                *future = true;
                self.expr(value, live, future)
            }
            ExprKind::Binary(_, left, right) => {
                let mut touched = self.expr(right, live, future);
                touched.extend(self.expr(left, live, future));
                touched
            }
            ExprKind::IndirectCall { callee, arguments }
            | ExprKind::DynCall {
                receiver: callee,
                arguments,
                ..
            } => {
                *future = true;
                let mut touched = self.operands(arguments.iter(), live, future);
                touched.extend(self.expr(callee, live, future));
                touched
            }
            ExprKind::Record(fields) => {
                self.operands(fields.iter().map(|(_, value)| value), live, future)
            }
            ExprKind::Block(block) => self.block(block, live, future),
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let mut other = live.clone();
                let mut other_allocates = *future;
                let mut touched = self.block(then_body, live, future);
                if let Some(body) = else_body {
                    touched.extend(self.block(body, &mut other, &mut other_allocates));
                }
                live.extend(other);
                *future |= other_allocates;
                touched.extend(self.expr(condition, live, future));
                touched
            }
            ExprKind::Match { value, arms } => {
                let mut inputs = live.clone();
                let mut inputs_allocate = *future;
                let mut touched = Locals::new();
                for arm in arms {
                    let mut branch = live.clone();
                    let mut branch_allocates = *future;
                    let mut arm_touched = self.block(&arm.body, &mut branch, &mut branch_allocates);
                    // These slots are initialized by match lowering, not Let.
                    let mut bindings = Locals::new();
                    bindings.extend(arm.whole.filter(|local| self.rooted.contains(local)));
                    bindings.extend(
                        arm.bindings
                            .iter()
                            .flatten()
                            .copied()
                            .filter(|local| self.rooted.contains(local)),
                    );
                    // Read/assigned bindings already have a statement or tail
                    // clear. Only unused pattern slots need this extra boundary.
                    let dead = bindings
                        .difference(&arm_touched)
                        .filter(|local| !live.contains(local))
                        .copied()
                        .collect::<Vec<_>>();
                    if *future && !dead.is_empty() {
                        let clears = self.plan.after_block.entry(&arm.body).or_default();
                        clears.extend(dead);
                        clears.sort_unstable();
                    }
                    arm_touched.extend(bindings);
                    touched.extend(arm_touched);
                    inputs.extend(branch);
                    inputs_allocate |= branch_allocates;
                }
                *live = inputs;
                *future = inputs_allocate;
                touched.extend(self.expr(value, live, future));
                touched
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Text(_)
            | ExprKind::FunctionRef(_) => Locals::new(),
        }
    }
}

fn read_block(
    block: &Block,
    reads: &mut Locals,
    rooted: &Locals,
    allocating: &BTreeSet<usize>,
    future: &mut bool,
) {
    for statement in &block.statements {
        match &statement.kind {
            StmtKind::Let { value, .. }
            | StmtKind::Assign { value, .. }
            | StmtKind::Return(Some(value))
            | StmtKind::Assert(value)
            | StmtKind::Discard(value)
            | StmtKind::Expr(value) => read_expr(value, reads, rooted, allocating, future),
            StmtKind::While { condition, body } => {
                read_expr(condition, reads, rooted, allocating, future);
                read_block(body, reads, rooted, allocating, future);
            }
            StmtKind::Return(None) => {}
        }
    }
    if let Some(tail) = &block.tail {
        read_expr(tail, reads, rooted, allocating, future);
    }
}

fn read_expr(
    expr: &Expr,
    reads: &mut Locals,
    rooted: &Locals,
    allocating: &BTreeSet<usize>,
    future: &mut bool,
) {
    *future |= match &expr.kind {
        ExprKind::Primitive(operation, _) => super::gc::allocates(*operation),
        ExprKind::Call(target, _) => allocating.contains(target),
        ExprKind::DynBox { .. } | ExprKind::DynCall { .. } | ExprKind::IndirectCall { .. } => true,
        _ => false,
    };
    match &expr.kind {
        ExprKind::Local(local) => {
            if rooted.contains(local) {
                reads.insert(*local);
            }
        }
        ExprKind::Primitive(_, values)
        | ExprKind::Call(_, values)
        | ExprKind::Variant { fields: values, .. } => {
            for value in values {
                read_expr(value, reads, rooted, allocating, future);
            }
        }
        ExprKind::Unary(_, value)
        | ExprKind::Coerce(value)
        | ExprKind::Field(value, _)
        | ExprKind::DynBox { value, .. } => read_expr(value, reads, rooted, allocating, future),
        ExprKind::Binary(_, left, right) => {
            read_expr(left, reads, rooted, allocating, future);
            read_expr(right, reads, rooted, allocating, future);
        }
        ExprKind::IndirectCall { callee, arguments }
        | ExprKind::DynCall {
            receiver: callee,
            arguments,
            ..
        } => {
            read_expr(callee, reads, rooted, allocating, future);
            for argument in arguments {
                read_expr(argument, reads, rooted, allocating, future);
            }
        }
        ExprKind::Record(fields) => {
            for (_, value) in fields {
                read_expr(value, reads, rooted, allocating, future);
            }
        }
        ExprKind::Block(block) => read_block(block, reads, rooted, allocating, future),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            read_expr(condition, reads, rooted, allocating, future);
            read_block(then_body, reads, rooted, allocating, future);
            if let Some(body) = else_body {
                read_block(body, reads, rooted, allocating, future);
            }
        }
        ExprKind::Match { value, arms } => {
            read_expr(value, reads, rooted, allocating, future);
            for arm in arms {
                read_block(&arm.body, reads, rooted, allocating, future);
            }
        }
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Text(_)
        | ExprKind::FunctionRef(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Span, Type, checked::MatchArm};

    fn expression(kind: ExprKind) -> Expr {
        Expr {
            kind,
            ty: Type::Text,
            span: Span::default(),
        }
    }

    fn local(index: usize) -> Expr {
        expression(ExprKind::Local(index))
    }

    fn statement(kind: StmtKind) -> Stmt {
        Stmt {
            kind,
            span: Span::default(),
        }
    }

    fn discard(value: Expr) -> Stmt {
        statement(StmtKind::Discard(value))
    }

    fn allocate() -> Stmt {
        discard(expression(ExprKind::Call(0, vec![])))
    }

    fn block(statements: Vec<Stmt>, tail: Option<Expr>) -> Block {
        Block {
            statements,
            tail: tail.map(Box::new),
            falls_through: true,
        }
    }

    fn function(body: Block) -> Function {
        Function {
            name: "liveness".into(),
            params: vec![Type::Text; 3],
            result: Type::Unit,
            locals: vec![Type::Text; 3],
            requires: vec![],
            body,
            span: Span::default(),
        }
    }

    #[test]
    fn nested_blocks_keep_cleanup_and_later_operand_reads() {
        let nested = expression(ExprKind::Block(block(
            vec![discard(local(0))],
            Some(local(1)),
        )));
        let call = expression(ExprKind::Call(0, vec![nested, local(1)]));
        let mut source = function(block(
            vec![discard(call), discard(local(0)), allocate()],
            None,
        ));
        let plan = plan(&source, 0..3, &BTreeSet::from([0]));
        let StmtKind::Discard(call) = &source.body.statements[0].kind else {
            panic!()
        };
        let ExprKind::Call(_, args) = &call.kind else {
            panic!()
        };
        let ExprKind::Block(inner) = &args[0].kind else {
            panic!()
        };
        assert!(!plan.after_block.contains_key(&(inner as *const Block)));
        assert!(
            !plan
                .after_statement
                .contains_key(&(&inner.statements[0] as *const Stmt))
        );
        assert_eq!(
            plan.after_statement[&(&source.body.statements[0] as *const Stmt)],
            [1]
        );
        assert_eq!(
            plan.after_statement[&(&source.body.statements[1] as *const Stmt)],
            [0]
        );
        assert_eq!(plan.after_requires, [2]);
        let no_gc = super::plan(&source, 0..3, &BTreeSet::new());
        assert!(no_gc.after_requires.is_empty());
        assert!(no_gc.after_statement.is_empty() && no_gc.after_block.is_empty());
        let inner_statement = &inner.statements[0] as *const Stmt;
        let outer_statement = &source.body.statements[0] as *const Stmt;
        source.body.statements.truncate(1);
        let final_call = super::plan(&source, 0..3, &BTreeSet::from([0]));
        assert_eq!(final_call.after_statement[&inner_statement], [0]);
        assert!(!final_call.after_statement.contains_key(&outer_statement));
    }

    #[test]
    fn hidden_definitions_survive_outside_their_declaration_block() {
        let inner = block(
            vec![statement(StmtKind::Let {
                local: 0,
                value: local(1),
            })],
            Some(local(2)),
        );
        let source = function(block(
            vec![
                discard(expression(ExprKind::Block(inner))),
                discard(local(0)),
                allocate(),
            ],
            None,
        ));
        let plan = plan(&source, 0..3, &BTreeSet::from([0]));
        let StmtKind::Discard(value) = &source.body.statements[0].kind else {
            panic!()
        };
        let ExprKind::Block(inner) = &value.kind else {
            panic!()
        };
        assert_eq!(plan.after_block[&(inner as *const Block)], [2]);
        assert_eq!(
            plan.after_statement[&(&inner.statements[0] as *const Stmt)],
            [1]
        );
        assert!(
            !plan
                .after_block
                .contains_key(&(&source.body as *const Block))
        );
    }

    #[test]
    fn loop_backedges_keep_reads_and_clear_dead_slots_only_before_collection() {
        let body = block(
            vec![
                discard(local(0)),
                discard(local(1)),
                statement(StmtKind::Let {
                    local: 2,
                    value: expression(ExprKind::Text("unused".into())),
                }),
            ],
            None,
        );
        let source = function(block(
            vec![statement(StmtKind::While {
                condition: expression(ExprKind::Call(0, vec![local(0)])),
                body,
            })],
            None,
        ));
        let plan = plan(&source, 0..3, &BTreeSet::from([0]));
        let StmtKind::While { body, .. } = &source.body.statements[0].kind else {
            panic!()
        };
        assert!(
            !plan
                .after_statement
                .contains_key(&(&body.statements[0] as *const Stmt))
        );
        assert!(
            !plan
                .after_statement
                .contains_key(&(&body.statements[1] as *const Stmt))
        );
        assert!(!plan.after_block.contains_key(&(body as *const Block)));
        assert_eq!(
            plan.after_statement[&(&body.statements[2] as *const Stmt)],
            [2]
        );
        assert!(
            !plan
                .after_statement
                .contains_key(&(&source.body.statements[0] as *const Stmt))
        );
    }

    #[test]
    fn match_bindings_and_branch_continuations_are_not_lexical_scopes() {
        let arm = MatchArm {
            variant: None,
            bindings: vec![Some(1), Some(2), Some(3)],
            whole: Some(4),
            body: block(vec![discard(local(2))], Some(local(3))),
        };
        let matched = expression(ExprKind::Match {
            value: Box::new(local(0)),
            arms: vec![arm],
        });
        let mut source = function(block(
            vec![discard(matched), discard(local(1)), allocate()],
            None,
        ));
        source.locals.extend([Type::Text; 2]);
        let plan = plan(&source, 0..5, &BTreeSet::from([0]));
        let StmtKind::Discard(value) = &source.body.statements[0].kind else {
            panic!()
        };
        let ExprKind::Match { arms, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(plan.after_block[&(&arms[0].body as *const Block)], [3, 4]);
        assert_eq!(
            plan.after_statement[&(&arms[0].body.statements[0] as *const Stmt)],
            [2]
        );
        assert_eq!(
            plan.after_statement[&(&source.body.statements[0] as *const Stmt)],
            [0, 2, 3, 4]
        );
        let scalar = super::plan(&source, std::iter::empty(), &BTreeSet::from([0]));
        assert!(scalar.after_statement.is_empty() && scalar.after_block.is_empty());
    }
}
