//! Checked control-flow liveness at complete expression/statement boundaries.
//! Pending operands have separate snapshot roots. Cleanup captures and borrowed
//! owner slots stay rooted independently of ordinary local use.

use super::*;

type Live = BTreeSet<usize>;

struct Collections<'a> {
    query: gc::TemporarySlots,
    allocating: &'a BTreeSet<usize>,
}

impl Collections<'_> {
    fn expression(&mut self, value: &checked::Expr) -> bool {
        self.query.may_allocate(self.allocating, value)
    }

    fn block(&mut self, body: &checked::Block) -> bool {
        self.query.block_allocates(self.allocating, body)
    }
}

#[derive(Default)]
pub(super) struct Plan {
    // Only live-set differences on selected edges, not every nonlive slot.
    pub entries: HashMap<*const checked::Block, Live>,
    pub retired: HashMap<*const checked::Stmt, Live>,
    pub pinned: Live,
}

impl Plan {
    pub fn new(
        body: &checked::Block,
        parameters: usize,
        pinned: Live,
        allocating: &BTreeSet<usize>,
        shared: bool,
    ) -> Self {
        let mut plan = Self {
            pinned,
            ..Self::default()
        };
        let mut collections = Collections {
            query: gc::TemporarySlots::allocation_query(shared),
            allocating,
        };
        let before = plan.block(
            body,
            Live::new(),
            None,
            &mut Live::new(),
            shared,
            &mut collections,
        );
        let collects = collections.block(body);
        plan.entries.insert(
            body,
            (0..parameters)
                .filter(|id| collects && !before.contains(id))
                .collect(),
        );
        plan
    }

    fn block(
        &mut self,
        body: &checked::Block,
        mut live: Live,
        targets: Option<(&Live, &Live)>,
        definitions: &mut Live,
        mut future: bool,
        collections: &mut Collections<'_>,
    ) -> Live {
        if let Some(tail) = &body.tail {
            live = self.expression(tail, live, targets, definitions, future, collections);
            future |= collections.expression(tail);
        }
        for statement in body.statements.iter().rev() {
            let after = live.clone();
            let after_collection = future;
            let mut written = Live::new();
            match &statement.kind {
                checked::StmtKind::Let { local, value }
                | checked::StmtKind::Assign { local, value } => {
                    live.remove(local);
                    written.insert(*local);
                    live = self.expression(value, live, targets, &mut written, future, collections);
                    future |= collections.expression(value);
                }
                checked::StmtKind::Return(value) => {
                    live.clear();
                    future = false;
                    if let Some(value) = value {
                        live =
                            self.expression(value, live, targets, &mut written, false, collections);
                        future = collections.expression(value);
                    }
                }
                checked::StmtKind::Assert { condition, .. }
                | checked::StmtKind::Discard(condition)
                | checked::StmtKind::Expr(condition) => {
                    live = self.expression(
                        condition,
                        live,
                        targets,
                        &mut written,
                        future,
                        collections,
                    );
                    future |= collections.expression(condition);
                }
                checked::StmtKind::While { condition, body } => {
                    future |= collections.expression(condition) || collections.block(body);
                    let mut head = Live::new();
                    loop {
                        let before = self.block(
                            body,
                            head.clone(),
                            Some((&after, &head)),
                            &mut written,
                            future,
                            collections,
                        );
                        let next = self.expression(
                            condition,
                            after.union(&before).copied().collect(),
                            targets,
                            &mut written,
                            future,
                            collections,
                        );
                        if next == head {
                            self.entries.insert(
                                body,
                                head.difference(&before)
                                    .copied()
                                    .filter(|_| future)
                                    .collect(),
                            );
                            break;
                        }
                        head = next;
                    }
                    live = head;
                }
                checked::StmtKind::Break => {
                    live = targets.map_or_else(Live::new, |(exit, _)| exit.clone());
                    future = true;
                }
                checked::StmtKind::Continue => {
                    live = targets.map_or_else(Live::new, |(_, head)| head.clone());
                    future = true;
                }
                // These bodies run separately and can run on fault paths. The
                // owner's captures are pinned; callback locals get their own plan.
                checked::StmtKind::Cleanup { body, .. } => future |= collections.block(body),
                checked::StmtKind::Defer { .. } => {}
            }
            self.retired.insert(
                statement,
                live.union(&written)
                    .copied()
                    .filter(|id| after_collection && !after.contains(id))
                    .collect(),
            );
            definitions.extend(written);
        }
        live
    }

    fn expression(
        &mut self,
        value: &checked::Expr,
        mut live: Live,
        targets: Option<(&Live, &Live)>,
        definitions: &mut Live,
        future: bool,
        collections: &mut Collections<'_>,
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
            } => {
                live = self.expression(value, live, targets, definitions, future, collections);
            }
            checked::ExprKind::DynBox { value, .. } => {
                live = self.expression(value, live, targets, definitions, true, collections);
            }
            checked::ExprKind::Binary(operation, left, right) => {
                let after = live.clone();
                live = self.expression(right, live, targets, definitions, future, collections);
                if matches!(operation, Binary::And | Binary::Or) {
                    live.extend(after);
                }
                let future = future || collections.expression(right);
                live = self.expression(left, live, targets, definitions, future, collections);
            }
            checked::ExprKind::Call(_, values)
            | checked::ExprKind::Primitive(_, values)
            | checked::ExprKind::List(values)
            | checked::ExprKind::Variant { fields: values, .. } => {
                let mut future = future
                    || match value.kind {
                        checked::ExprKind::Call(target, _) => {
                            collections.allocating.contains(&target)
                        }
                        checked::ExprKind::Primitive(operation, _) => gc::allocates(operation),
                        checked::ExprKind::List(_) => true,
                        _ => false,
                    };
                for value in values.iter().rev() {
                    live = self.expression(value, live, targets, definitions, future, collections);
                    future |= collections.expression(value);
                }
            }
            checked::ExprKind::IndirectCall { callee, arguments }
            | checked::ExprKind::DynCall {
                receiver: callee,
                arguments,
                ..
            } => {
                for value in arguments.iter().rev() {
                    live = self.expression(value, live, targets, definitions, true, collections);
                }
                live = self.expression(callee, live, targets, definitions, true, collections);
            }
            checked::ExprKind::Record(fields) | checked::ExprKind::FrameNew(fields) => {
                let mut future = future || matches!(value.kind, checked::ExprKind::FrameNew(_));
                for (_, value) in fields.iter().rev() {
                    live = self.expression(value, live, targets, definitions, future, collections);
                    future |= collections.expression(value);
                }
            }
            checked::ExprKind::FrameStore { frame, value, .. } => {
                live = self.expression(value, live, targets, definitions, future, collections);
                let future = future || collections.expression(value);
                live = self.expression(frame, live, targets, definitions, future, collections);
            }
            checked::ExprKind::Block(body) => {
                live = self.block(body, live, targets, definitions, future, collections);
            }
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let yes = self.block(
                    then_body,
                    live.clone(),
                    targets,
                    definitions,
                    future,
                    collections,
                );
                let no = if let Some(body) = else_body {
                    self.block(body, live, targets, definitions, future, collections)
                } else {
                    live
                };
                let branches: Live = yes.union(&no).copied().collect();
                let then_collects = future || collections.block(then_body);
                self.entries.insert(
                    then_body,
                    branches
                        .difference(&yes)
                        .copied()
                        .filter(|_| then_collects)
                        .collect(),
                );
                if let Some(body) = else_body {
                    let collects = future || collections.block(body);
                    self.entries.insert(
                        body,
                        branches
                            .difference(&no)
                            .copied()
                            .filter(|_| collects)
                            .collect(),
                    );
                }
                let future = then_collects
                    || else_body
                        .as_ref()
                        .is_some_and(|body| collections.block(body));
                live = self.expression(
                    condition,
                    branches,
                    targets,
                    definitions,
                    future,
                    collections,
                );
            }
            checked::ExprKind::Match { value, arms } => {
                let mut branches = Live::new();
                let mut incoming = Vec::new();
                let mut collecting = future;
                for arm in arms {
                    let mut before = self.block(
                        &arm.body,
                        live.clone(),
                        targets,
                        definitions,
                        future,
                        collections,
                    );
                    for id in arm.bindings.iter().chain([&arm.whole]).flatten() {
                        before.remove(id);
                        definitions.insert(*id);
                    }
                    branches.extend(&before);
                    let collects = future || collections.block(&arm.body);
                    collecting |= collects;
                    incoming.push((&arm.body, before, collects));
                }
                for (body, before, collects) in incoming {
                    self.entries.insert(
                        body,
                        branches
                            .difference(&before)
                            .copied()
                            .filter(|_| collects)
                            .collect(),
                    );
                }
                live = self.expression(
                    value,
                    branches,
                    targets,
                    definitions,
                    collecting,
                    collections,
                );
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
