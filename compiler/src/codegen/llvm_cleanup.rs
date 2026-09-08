//! Typed lexical cleanup callbacks. Only captured bindings become addressable;
//! ordinary function calls do not use a runtime executor.

use super::*;

pub(super) struct Plan<'a> {
    pub id: usize,
    pub body: &'a checked::Block,
    pub captures: Vec<usize>,
    pub locals: BTreeSet<usize>,
}

pub(super) struct Site<'ctx> {
    callback: FunctionValue<'ctx>,
    record: PointerValue<'ctx>,
    captures: PointerValue<'ctx>,
}

// Cleanup marker children are latent callback bodies, not owner expressions.
fn walk<'a>(
    body: &'a checked::Block,
    statement: &mut impl FnMut(&'a checked::Stmt),
    expression: &mut impl FnMut(&'a checked::Expr),
) {
    for value in &body.statements {
        statement(value);
        match &value.kind {
            checked::StmtKind::Let { value, .. }
            | checked::StmtKind::Assign { value, .. }
            | checked::StmtKind::Return(Some(value))
            | checked::StmtKind::Assert {
                condition: value, ..
            }
            | checked::StmtKind::Discard(value)
            | checked::StmtKind::Expr(value) => walk_expr(value, statement, expression),
            checked::StmtKind::While { condition, body } => {
                walk_expr(condition, statement, expression);
                walk(body, statement, expression);
            }
            _ => {}
        }
    }
    if let Some(tail) = &body.tail {
        walk_expr(tail, statement, expression);
    }
}

fn walk_expr<'a>(
    value: &'a checked::Expr,
    statement: &mut impl FnMut(&'a checked::Stmt),
    expression: &mut impl FnMut(&'a checked::Expr),
) {
    expression(value);
    match &value.kind {
        checked::ExprKind::Unary(_, value)
        | checked::ExprKind::Coerce(value)
        | checked::ExprKind::Field(value, _)
        | checked::ExprKind::DynBox { value, .. } => walk_expr(value, statement, expression),
        checked::ExprKind::Binary(_, left, right) => {
            walk_expr(left, statement, expression);
            walk_expr(right, statement, expression);
        }
        checked::ExprKind::DynCall {
            receiver,
            arguments,
            ..
        }
        | checked::ExprKind::IndirectCall {
            callee: receiver,
            arguments,
        } => {
            walk_expr(receiver, statement, expression);
            for value in arguments {
                walk_expr(value, statement, expression);
            }
        }
        checked::ExprKind::Call(_, values)
        | checked::ExprKind::Primitive(_, values)
        | checked::ExprKind::List(values)
        | checked::ExprKind::Variant { fields: values, .. } => {
            for value in values {
                walk_expr(value, statement, expression);
            }
        }
        checked::ExprKind::Record(fields) | checked::ExprKind::FrameNew(fields) => {
            for (_, value) in fields {
                walk_expr(value, statement, expression);
            }
        }
        checked::ExprKind::FrameStore { frame, value, .. } => {
            walk_expr(frame, statement, expression);
            walk_expr(value, statement, expression);
        }
        checked::ExprKind::Block(body) => walk(body, statement, expression),
        checked::ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            walk_expr(condition, statement, expression);
            walk(then_body, statement, expression);
            if let Some(body) = else_body {
                walk(body, statement, expression);
            }
        }
        checked::ExprKind::Match { value, arms } => {
            walk_expr(value, statement, expression);
            for arm in arms {
                walk(&arm.body, statement, expression);
            }
        }
        _ => {}
    }
}

pub(super) fn plans(source: &checked::Function) -> NativeResult<Vec<Plan<'_>>> {
    let mut definitions = Vec::new();
    let mut invoked = BTreeSet::new();
    walk(
        &source.body,
        &mut |value| match &value.kind {
            checked::StmtKind::Defer { id, body } => definitions.push((*id, body)),
            checked::StmtKind::Cleanup { id, .. } => {
                invoked.insert(*id);
            }
            _ => {}
        },
        &mut |_| {},
    );
    let mut ids = BTreeSet::new();
    let mut plans = Vec::new();
    for (id, body) in definitions {
        if !ids.insert(id) {
            return Err("duplicate checked cleanup registration".into());
        }
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        let mut declarations = BTreeSet::new();
        let mut patterns = BTreeSet::<usize>::new();
        let mut nested = false;
        walk(
            body,
            &mut |value| match &value.kind {
                checked::StmtKind::Let { local, .. } => {
                    declarations.insert(*local);
                }
                checked::StmtKind::Assign { local, .. } => {
                    writes.insert(*local);
                }
                checked::StmtKind::Defer { .. }
                | checked::StmtKind::Cleanup { .. }
                | checked::StmtKind::Return(_) => nested = true,
                _ => {}
            },
            &mut |value| match &value.kind {
                checked::ExprKind::Local(id) => {
                    reads.insert(*id);
                }
                checked::ExprKind::Match { arms, .. } => {
                    for arm in arms {
                        patterns.extend(arm.bindings.iter().chain([&arm.whole]).flatten());
                    }
                }
                _ => {}
            },
        );
        if nested {
            return Err("checked cleanup cannot return or register nested cleanup".into());
        }
        declarations.extend(patterns);
        reads.extend(writes);
        let mut captures = Vec::new();
        for local in reads.difference(&declarations) {
            let ty = source.locals.get(*local).ok_or("invalid cleanup capture")?;
            if *ty != Type::Unit {
                captures.push(*local);
            }
        }
        plans.push(Plan {
            id,
            body,
            captures,
            locals: declarations,
        });
    }
    if !invoked.is_subset(&ids) {
        return Err("checked cleanup has no registration".into());
    }
    Ok(plans)
}

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    pub(super) fn prepare_cleanups(
        &mut self,
        owner: usize,
        plans: &[Plan<'_>],
    ) -> NativeResult<()> {
        let pointer = self.context.ptr_type(AddressSpace::default());
        let record = self.context.struct_type(&[pointer.into(); 3], false);
        for plan in plans {
            let callback = self.module.add_function(
                &format!("loom.cleanup.{owner}.{}", plan.id),
                self.context.void_type().fn_type(&[pointer.into()], false),
                Some(Linkage::Internal),
            );
            let layout = pointer.array_type(u32::try_from(plan.captures.len())?);
            let captures = self.builder.build_alloca(layout, "cleanup.captures")?;
            let mut values = layout.const_zero();
            for (index, local) in plan.captures.iter().enumerate() {
                let address = if gc::managed(self.program, self.local_types[*local]) {
                    *self
                        .roots
                        .locals
                        .get(local)
                        .ok_or("unrooted cleanup capture")?
                } else {
                    self.locals[*local].ok_or("missing cleanup capture storage")?
                };
                values = self
                    .builder
                    .build_insert_value(values, address, index as u32, "cleanup.capture")?
                    .into_array_value();
            }
            self.builder.build_store(captures, values)?;
            self.cleanups.insert(
                plan.id,
                Site {
                    callback,
                    record: self.builder.build_alloca(record, "cleanup.record")?,
                    captures,
                },
            );
        }
        Ok(())
    }

    pub(super) fn register_cleanup(&self, id: usize) -> NativeResult<()> {
        let site = self
            .cleanups
            .get(&id)
            .ok_or("missing cleanup registration")?;
        self.runtime_call(
            "cleanup_push",
            None,
            &[
                site.record.into(),
                site.callback.as_global_value().as_pointer_value().into(),
                site.captures.into(),
            ],
        )?;
        Ok(())
    }

    pub(super) fn run_cleanup(&self, id: usize) -> NativeResult<()> {
        let site = self
            .cleanups
            .get(&id)
            .ok_or("missing cleanup registration")?;
        self.runtime_call("cleanup_pop", None, &[site.record.into()])?;
        self.builder
            .build_call(site.callback, &[site.captures.into()], "")?;
        // Captured assignments are write-through, including before a nested
        // fault. Normal execution must see updated/relocated authoritative roots.
        self.restore_locals()
    }

    pub(super) fn emit_cleanups(
        &mut self,
        source: &checked::Function,
        plans: &[Plan<'_>],
    ) -> NativeResult<()> {
        let pointer = self.context.ptr_type(AddressSpace::default());
        for plan in plans {
            let function = self.cleanups[&plan.id].callback;
            self.builder
                .position_at_end(self.context.append_basic_block(function, "entry"));
            let environment = function
                .get_first_param()
                .ok_or("missing cleanup environment")?
                .into_pointer_value();
            let mut locals = vec![None; source.locals.len()];
            let mut borrowed = HashMap::new();
            for (index, id) in plan.captures.iter().enumerate() {
                // SAFETY: prepare_cleanups stores exactly this ordered array of
                // pointers to live owner slots; registration cannot outlive them.
                #[allow(unsafe_code)]
                let slot = unsafe {
                    self.builder.build_in_bounds_gep(
                        pointer,
                        environment,
                        &[self.size_type.const_int(index as u64, false)],
                        "cleanup.capture.address",
                    )?
                };
                let address = self
                    .builder
                    .build_load(pointer, slot, "cleanup.capture")?
                    .into_pointer_value();
                locals[*id] = Some(address);
                if gc::managed(self.program, source.locals[*id]) {
                    borrowed.insert(*id, address);
                }
            }
            for id in &plan.locals {
                if source.locals[*id] != Type::Unit {
                    locals[*id] = Some(self.builder.build_alloca(
                        native_type(self.context, self.program, source.locals[*id])?,
                        "local",
                    )?);
                }
            }
            let roots = gc::root_function(
                self.context,
                self.module,
                self.builder,
                self.program,
                self.allocating,
                source,
                Some(plan.body),
                &locals,
                &borrowed,
                self.tracers,
            )?;
            let mut callback = FunctionEmitter {
                context: self.context,
                module: self.module,
                builder: self.builder,
                functions: self.functions,
                witnesses: self.witnesses,
                function,
                locals,
                local_types: &source.locals,
                allocating: self.allocating,
                program: self.program,
                size_type: self.size_type,
                roots,
                tracers: self.tracers,
                loop_targets: Vec::new(),
                cleanups: HashMap::new(),
                runtime_fault: true,
            };
            callback.block(plan.body)?;
            if callback.live() {
                callback.leave_roots()?;
                callback.builder.build_return(None)?;
            }
        }
        Ok(())
    }
}
