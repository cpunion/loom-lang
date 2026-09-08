//! Conservative last-read clearing for local shadow roots, not expression snapshots.
//!
//! Lowered blocks are not source scopes: cleanup and destructuring can read their
//! locals later. Pass the actual continuation backwards through every operand.
//! Definitions never kill reads, and loops retain all reads across the backedge.

use crate::model::checked::{Block, Expr, ExprKind, Function, Stmt, StmtKind};
use std::collections::{BTreeSet, HashMap};

type Locals = BTreeSet<usize>;

#[derive(Default)]
pub(super) struct Plan {
    pub after_requires: Vec<usize>,
    pub after_statement: HashMap<*const Stmt, Vec<usize>>,
    pub after_block: HashMap<*const Block, Vec<usize>>,
}

pub(super) fn plan(function: &Function, rooted: impl Iterator<Item = usize>) -> Plan {
    let rooted = rooted.collect::<Locals>();
    if rooted.is_empty() {
        return Plan::default();
    }
    let mut analysis = Analysis {
        rooted,
        plan: Plan::default(),
    };
    let mut live = Locals::new();
    analysis.block(&function.body, &mut live);
    analysis.plan.after_requires = analysis
        .rooted
        .difference(&live)
        .copied()
        .filter(|local| *local < function.params.len())
        .collect();
    for requirement in function.requires.iter().rev() {
        analysis.expr(requirement, &mut live);
    }
    analysis.plan
}

struct Analysis {
    rooted: Locals,
    plan: Plan,
}

impl Analysis {
    fn dead(&self, touched: &Locals, live: &Locals) -> Vec<usize> {
        touched.difference(live).copied().collect()
    }

    /// Updates `live` to the input read set and returns all reads/definitions.
    fn block(&mut self, block: &Block, live: &mut Locals) -> Locals {
        let out = live.clone();
        let mut touched = Locals::new();
        if let Some(tail) = &block.tail {
            touched.extend(self.expr(tail, live));
        }
        for statement in block.statements.iter().rev() {
            let after = live.clone();
            let used = self.statement(statement, live);
            let dead = self.dead(&used, &after);
            if !dead.is_empty() {
                self.plan.after_statement.insert(statement, dead);
            }
            touched.extend(used);
        }
        let dead = self.dead(&touched, &out);
        if !dead.is_empty() {
            self.plan.after_block.insert(block, dead);
        }
        touched
    }

    fn statement(&mut self, statement: &Stmt, live: &mut Locals) -> Locals {
        match &statement.kind {
            StmtKind::Let { local, value } | StmtKind::Assign { local, value } => {
                let mut touched = self.expr(value, live);
                if self.rooted.contains(local) {
                    touched.insert(*local);
                }
                touched
            }
            StmtKind::Return(Some(value))
            | StmtKind::Assert(value)
            | StmtKind::Discard(value)
            | StmtKind::Expr(value) => self.expr(value, live),
            StmtKind::Return(None) => Locals::new(),
            StmtKind::While { condition, body } => {
                // No break/continue in this IR. Retaining every loop read avoids
                // a dataflow fixpoint while protecting condition/body backedges.
                read_block(body, live, &self.rooted);
                read_expr(condition, live, &self.rooted);
                let mut touched = self.block(body, live);
                touched.extend(self.expr(condition, live));
                touched
            }
        }
    }

    fn operands<'a>(
        &mut self,
        values: impl DoubleEndedIterator<Item = &'a Expr>,
        live: &mut Locals,
    ) -> Locals {
        let mut touched = Locals::new();
        for value in values.rev() {
            touched.extend(self.expr(value, live));
        }
        touched
    }

    fn expr(&mut self, expr: &Expr, live: &mut Locals) -> Locals {
        match &expr.kind {
            ExprKind::Local(local) => {
                if !self.rooted.contains(local) {
                    return Locals::new();
                }
                live.insert(*local);
                Locals::from([*local])
            }
            ExprKind::Primitive(_, values)
            | ExprKind::Call(_, values)
            | ExprKind::Variant { fields: values, .. } => self.operands(values.iter(), live),
            ExprKind::Unary(_, value)
            | ExprKind::Coerce(value)
            | ExprKind::Field(value, _)
            | ExprKind::DynBox { value, .. } => self.expr(value, live),
            ExprKind::Binary(_, left, right) => {
                let mut touched = self.expr(right, live);
                touched.extend(self.expr(left, live));
                touched
            }
            ExprKind::IndirectCall { callee, arguments }
            | ExprKind::DynCall {
                receiver: callee,
                arguments,
                ..
            } => {
                let mut touched = self.operands(arguments.iter(), live);
                touched.extend(self.expr(callee, live));
                touched
            }
            ExprKind::Record(fields) => self.operands(fields.iter().map(|(_, value)| value), live),
            ExprKind::Block(block) => self.block(block, live),
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let mut other = live.clone();
                let mut touched = self.block(then_body, live);
                if let Some(body) = else_body {
                    touched.extend(self.block(body, &mut other));
                }
                live.extend(other);
                touched.extend(self.expr(condition, live));
                touched
            }
            ExprKind::Match { value, arms } => {
                let mut inputs = live.clone();
                let mut touched = Locals::new();
                for arm in arms {
                    let mut branch = live.clone();
                    let mut arm_touched = self.block(&arm.body, &mut branch);
                    // These slots are initialized by match lowering, not Let.
                    arm_touched.extend(arm.whole.filter(|local| self.rooted.contains(local)));
                    arm_touched.extend(
                        arm.bindings
                            .iter()
                            .flatten()
                            .copied()
                            .filter(|local| self.rooted.contains(local)),
                    );
                    let dead = self.dead(&arm_touched, live);
                    if !dead.is_empty() {
                        self.plan.after_block.insert(&arm.body, dead);
                    }
                    touched.extend(arm_touched);
                    inputs.extend(branch);
                }
                *live = inputs;
                touched.extend(self.expr(value, live));
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

fn read_block(block: &Block, reads: &mut Locals, rooted: &Locals) {
    for statement in &block.statements {
        match &statement.kind {
            StmtKind::Let { value, .. }
            | StmtKind::Assign { value, .. }
            | StmtKind::Return(Some(value))
            | StmtKind::Assert(value)
            | StmtKind::Discard(value)
            | StmtKind::Expr(value) => read_expr(value, reads, rooted),
            StmtKind::While { condition, body } => {
                read_expr(condition, reads, rooted);
                read_block(body, reads, rooted);
            }
            StmtKind::Return(None) => {}
        }
    }
    if let Some(tail) = &block.tail {
        read_expr(tail, reads, rooted);
    }
}

fn read_expr(expr: &Expr, reads: &mut Locals, rooted: &Locals) {
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
                read_expr(value, reads, rooted);
            }
        }
        ExprKind::Unary(_, value)
        | ExprKind::Coerce(value)
        | ExprKind::Field(value, _)
        | ExprKind::DynBox { value, .. } => read_expr(value, reads, rooted),
        ExprKind::Binary(_, left, right) => {
            read_expr(left, reads, rooted);
            read_expr(right, reads, rooted);
        }
        ExprKind::IndirectCall { callee, arguments }
        | ExprKind::DynCall {
            receiver: callee,
            arguments,
            ..
        } => {
            read_expr(callee, reads, rooted);
            for argument in arguments {
                read_expr(argument, reads, rooted);
            }
        }
        ExprKind::Record(fields) => {
            for (_, value) in fields {
                read_expr(value, reads, rooted);
            }
        }
        ExprKind::Block(block) => read_block(block, reads, rooted),
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            read_expr(condition, reads, rooted);
            read_block(then_body, reads, rooted);
            if let Some(body) = else_body {
                read_block(body, reads, rooted);
            }
        }
        ExprKind::Match { value, arms } => {
            read_expr(value, reads, rooted);
            for arm in arms {
                read_block(&arm.body, reads, rooted);
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
        let source = function(block(vec![discard(call), discard(local(0))], None));
        let plan = plan(&source, 0..3);
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
    }

    #[test]
    fn hidden_definitions_survive_outside_their_declaration_block() {
        let inner = block(
            vec![statement(StmtKind::Let {
                local: 0,
                value: local(1),
            })],
            None,
        );
        let source = function(block(
            vec![
                discard(expression(ExprKind::Block(inner))),
                discard(local(0)),
            ],
            None,
        ));
        let plan = plan(&source, 0..3);
        let StmtKind::Discard(value) = &source.body.statements[0].kind else {
            panic!()
        };
        let ExprKind::Block(inner) = &value.kind else {
            panic!()
        };
        assert_eq!(plan.after_block[&(inner as *const Block)], [1]);
        assert_eq!(
            plan.after_statement[&(&inner.statements[0] as *const Stmt)],
            [1]
        );
    }

    #[test]
    fn loop_backedges_retain_reads_but_loop_exit_releases_them() {
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
                condition: local(0),
                body,
            })],
            None,
        ));
        let plan = plan(&source, 0..3);
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
        assert_eq!(plan.after_block[&(body as *const Block)], [2]);
        assert_eq!(
            plan.after_statement[&(&source.body.statements[0] as *const Stmt)],
            [0, 1, 2]
        );
    }

    #[test]
    fn match_bindings_and_branch_continuations_are_not_lexical_scopes() {
        let arm = MatchArm {
            variant: None,
            bindings: vec![Some(1)],
            whole: Some(2),
            body: block(vec![], None),
        };
        let matched = expression(ExprKind::Match {
            value: Box::new(local(0)),
            arms: vec![arm],
        });
        let source = function(block(vec![discard(matched), discard(local(1))], None));
        let plan = plan(&source, 0..3);
        let StmtKind::Discard(value) = &source.body.statements[0].kind else {
            panic!()
        };
        let ExprKind::Match { arms, .. } = &value.kind else {
            panic!()
        };
        assert_eq!(plan.after_block[&(&arms[0].body as *const Block)], [2]);
        assert_eq!(
            plan.after_statement[&(&source.body.statements[0] as *const Stmt)],
            [0, 2]
        );
        let scalar = super::plan(&source, std::iter::empty());
        assert!(scalar.after_statement.is_empty() && scalar.after_block.is_empty());
    }
}
