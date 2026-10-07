//! Checked control-flow liveness at complete expression/statement boundaries.
//! Pending operands have separate snapshot roots. Cleanup captures and borrowed
//! owner slots stay rooted independently of ordinary local use.

use super::*;

type Live = BTreeSet<usize>;

#[derive(Default)]
pub(super) struct Plan {
    pub entries: HashMap<*const checked::Block, Live>,
    pub retired: HashMap<*const checked::Stmt, Live>,
    pub pinned: Live,
}

impl Plan {
    pub fn new(body: &checked::Block, pinned: Live) -> Self {
        let mut plan = Self {
            pinned,
            ..Self::default()
        };
        plan.block(body, Live::new(), None, &mut Live::new());
        plan
    }

    fn block(
        &mut self,
        body: &checked::Block,
        mut live: Live,
        targets: Option<(&Live, &Live)>,
        definitions: &mut Live,
    ) -> Live {
        if let Some(tail) = &body.tail {
            live = self.expression(tail, live, targets, definitions);
        }
        for statement in body.statements.iter().rev() {
            let after = live.clone();
            let mut written = Live::new();
            match &statement.kind {
                checked::StmtKind::Let { local, value }
                | checked::StmtKind::Assign { local, value } => {
                    live.remove(local);
                    written.insert(*local);
                    live = self.expression(value, live, targets, &mut written);
                }
                checked::StmtKind::Return(value) => {
                    live.clear();
                    if let Some(value) = value {
                        live = self.expression(value, live, targets, &mut written);
                    }
                }
                checked::StmtKind::Assert { condition, .. }
                | checked::StmtKind::Discard(condition)
                | checked::StmtKind::Expr(condition) => {
                    live = self.expression(condition, live, targets, &mut written);
                }
                checked::StmtKind::While { condition, body } => {
                    let mut head = Live::new();
                    loop {
                        let before =
                            self.block(body, head.clone(), Some((&after, &head)), &mut written);
                        let next = self.expression(
                            condition,
                            after.union(&before).copied().collect(),
                            targets,
                            &mut written,
                        );
                        if next == head {
                            break;
                        }
                        head = next;
                    }
                    live = head;
                }
                checked::StmtKind::Break => {
                    live = targets.map_or_else(Live::new, |(exit, _)| exit.clone());
                }
                checked::StmtKind::Continue => {
                    live = targets.map_or_else(Live::new, |(_, head)| head.clone());
                }
                // These bodies run separately and can run on fault paths. The
                // owner's captures are pinned; callback locals get their own plan.
                checked::StmtKind::Defer { .. } | checked::StmtKind::Cleanup { .. } => {}
            }
            self.retired.insert(
                statement,
                live.union(&written)
                    .copied()
                    .filter(|id| !after.contains(id))
                    .collect(),
            );
            definitions.extend(written);
        }
        self.entries.insert(body, live.clone());
        live
    }

    fn expression(
        &mut self,
        value: &checked::Expr,
        mut live: Live,
        targets: Option<(&Live, &Live)>,
        definitions: &mut Live,
    ) -> Live {
        match &value.kind {
            checked::ExprKind::Local(id) => {
                live.insert(*id);
            }
            checked::ExprKind::Unary(_, value)
            | checked::ExprKind::Coerce(value)
            | checked::ExprKind::Field(value, _)
            | checked::ExprKind::Closure {
                environment: value, ..
            }
            | checked::ExprKind::DynBox { value, .. } => {
                live = self.expression(value, live, targets, definitions);
            }
            checked::ExprKind::Binary(operation, left, right) => {
                let after = live.clone();
                live = self.expression(right, live, targets, definitions);
                if matches!(operation, Binary::And | Binary::Or) {
                    live.extend(after);
                }
                live = self.expression(left, live, targets, definitions);
            }
            checked::ExprKind::Call(_, values)
            | checked::ExprKind::Primitive(_, values)
            | checked::ExprKind::List(values)
            | checked::ExprKind::Variant { fields: values, .. } => {
                for value in values.iter().rev() {
                    live = self.expression(value, live, targets, definitions);
                }
            }
            checked::ExprKind::IndirectCall { callee, arguments }
            | checked::ExprKind::DynCall {
                receiver: callee,
                arguments,
                ..
            } => {
                for value in arguments.iter().rev() {
                    live = self.expression(value, live, targets, definitions);
                }
                live = self.expression(callee, live, targets, definitions);
            }
            checked::ExprKind::Record(fields) | checked::ExprKind::FrameNew(fields) => {
                for (_, value) in fields.iter().rev() {
                    live = self.expression(value, live, targets, definitions);
                }
            }
            checked::ExprKind::FrameStore { frame, value, .. } => {
                live = self.expression(value, live, targets, definitions);
                live = self.expression(frame, live, targets, definitions);
            }
            checked::ExprKind::Block(body) => {
                live = self.block(body, live, targets, definitions);
            }
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let mut branches = self.block(then_body, live.clone(), targets, definitions);
                branches.extend(if let Some(body) = else_body {
                    self.block(body, live, targets, definitions)
                } else {
                    live
                });
                live = self.expression(condition, branches, targets, definitions);
            }
            checked::ExprKind::Match { value, arms } => {
                let mut branches = Live::new();
                for arm in arms {
                    let mut before = self.block(&arm.body, live.clone(), targets, definitions);
                    for id in arm.bindings.iter().chain([&arm.whole]).flatten() {
                        before.remove(id);
                        definitions.insert(*id);
                    }
                    branches.extend(before);
                }
                live = self.expression(value, branches, targets, definitions);
            }
            checked::ExprKind::Int(_)
            | checked::ExprKind::Float(_)
            | checked::ExprKind::Bool(_)
            | checked::ExprKind::Text(_)
            | checked::ExprKind::Bytes(_)
            | checked::ExprKind::FunctionRef(_) => {}
        }
        live
    }
}
