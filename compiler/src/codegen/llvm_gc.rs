//! Precise, fixed stack roots for managed values; scalar functions need none.

use super::*;

pub(super) struct RootFrame<'ctx> {
    pub slots: Vec<PointerValue<'ctx>>,
    pub locals: HashMap<usize, PointerValue<'ctx>>,
    pub expressions: HashMap<*const checked::Expr, PointerValue<'ctx>>,
}

impl RootFrame<'_> {
    pub fn empty() -> Self {
        Self {
            slots: Vec::new(),
            locals: HashMap::new(),
            expressions: HashMap::new(),
        }
    }
}

/// A function with no transitive allocation has no GC safe point. Keeping
/// simple Text accessors and scalar helpers root-free matters in lexer loops.
pub(super) fn allocating_functions(
    program: &checked::Program,
    reachable: &BTreeSet<usize>,
) -> BTreeSet<usize> {
    let mut allocating = BTreeSet::new();
    let mut calls = HashMap::<usize, Vec<usize>>::new();
    for id in reachable {
        let source = &program.functions[*id];
        let mut values = Vec::new();
        for value in &source.requires {
            expressions(value, &mut values);
        }
        block_expressions(&source.body, &mut values);
        for value in values {
            match value.kind {
                checked::ExprKind::DynBox { .. }
                | checked::ExprKind::DynCall { .. }
                | checked::ExprKind::List(_)
                | checked::ExprKind::FrameNew(_)
                | checked::ExprKind::IndirectCall { .. } => {
                    allocating.insert(*id);
                }
                checked::ExprKind::Primitive(operation, _) => {
                    if allocates(operation) {
                        allocating.insert(*id);
                    }
                }
                checked::ExprKind::Call(target, _) => {
                    calls.entry(*id).or_default().push(target);
                }
                _ => {}
            }
        }
    }
    loop {
        let before = allocating.len();
        for (id, targets) in &calls {
            if targets.iter().any(|target| allocating.contains(target)) {
                allocating.insert(*id);
            }
        }
        if allocating.len() == before {
            return allocating;
        }
    }
}

pub(super) fn allocates(operation: Primitive) -> bool {
    match operation {
        Primitive::FloatFormat
        | Primitive::TaskRun
        | Primitive::TaskFileReadResult
        | Primitive::TextConcat
        | Primitive::TextSlice
        | Primitive::ArgText
        | Primitive::ProcessCaptureConfigured
        | Primitive::ProcessCaptureInputConfigured
        | Primitive::EnvGet
        | Primitive::BytesNew
        | Primitive::BytesPush
        | Primitive::BytesTextCopy
        | Primitive::ListNew
        | Primitive::ListPush
        | Primitive::Read
        | Primitive::DirectoryRead
        | Primitive::PathCanonical => true,
        Primitive::FloatFromInt
        | Primitive::TaskCreate
        | Primitive::TaskAdopt
        | Primitive::TaskReturn
        | Primitive::TaskCleanupPush
        | Primitive::TaskCleanupPop
        | Primitive::TaskWaitTimer
        | Primitive::TaskWaitFileRead
        | Primitive::TaskWaitFileWrite
        | Primitive::TaskWaitFileWriteBytes
        | Primitive::TaskFileResult
        | Primitive::MonotonicNs
        | Primitive::TaskAwait
        | Primitive::TaskResult
        | Primitive::TaskRelease
        | Primitive::FloatToInt
        | Primitive::FloatParse
        | Primitive::TextLen
        | Primitive::UnicodeAlphabetic
        | Primitive::UnicodeAlphanumeric
        | Primitive::UnicodeWhitespace
        | Primitive::ArgCount
        | Primitive::ProcessRun
        | Primitive::ProcessRunInput
        | Primitive::Exit
        | Primitive::TextByte
        | Primitive::TextEqual
        | Primitive::BytesLen
        | Primitive::BytesGet
        | Primitive::BytesSet
        | Primitive::BytesUtf8
        | Primitive::ListLen
        | Primitive::ListGet
        | Primitive::ListSet
        | Primitive::Open
        | Primitive::Create
        | Primitive::Write
        | Primitive::WriteBytes
        | Primitive::Close
        | Primitive::PathKind
        | Primitive::DirectoryCreate
        | Primitive::PathRename
        | Primitive::FileRemove
        | Primitive::DirectoryRemove
        | Primitive::PathEntryKind => false,
    }
}

pub(super) fn managed(program: &checked::Program, ty: Type) -> bool {
    match ty {
        Type::Text | Type::Bytes | Type::List(_) | Type::Dyn(_) => true,
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Task(_) => false,
            checked::DataKind::Frame(_) => true,
            checked::DataKind::Refined(base) => managed(program, *base),
            checked::DataKind::Record(fields) => fields.iter().any(|(_, ty)| managed(program, *ty)),
            checked::DataKind::Enum(variants) => variants
                .iter()
                .any(|(_, fields)| fields.iter().any(|ty| managed(program, *ty))),
        },
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::Unit
        | Type::Parameter(_)
        | Type::Function(_) => false,
    }
}

pub(super) fn runtime_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    name: &str,
    result: Option<BasicTypeEnum<'ctx>>,
    params: &[BasicTypeEnum<'ctx>],
) -> FunctionValue<'ctx> {
    let name = format!("loom_rt_{name}");
    module.get_function(&name).unwrap_or_else(|| {
        let params = params.iter().map(|ty| (*ty).into()).collect::<Vec<_>>();
        let ty = match result {
            Some(ty) => ty.fn_type(&params, false),
            None => context.void_type().fn_type(&params, false),
        };
        let function = module.add_function(&name, ty, None);
        if matches!(
            name.as_str(),
            "loom_rt_box_new"
                | "loom_rt_list_new"
                | "loom_rt_bytes_new"
                | "loom_rt_text_new"
                | "loom_rt_text_concat"
                | "loom_rt_text_slice"
                | "loom_rt_bytes_text_copy"
                | "loom_rt_float_format"
                | "loom_rt_process_arg_text"
        ) {
            // These private allocation boundaries return fresh storage or
            // terminate. In particular, it cannot alias a generated root slot.
            // Do not apply this promise to accessors or shared input values.
            for attribute in ["noalias", "nonnull"] {
                function.add_attribute(
                    inkwell::attributes::AttributeLoc::Return,
                    context.create_enum_attribute(
                        inkwell::attributes::Attribute::get_named_enum_kind_id(attribute),
                        0,
                    ),
                );
            }
        }
        function
    })
}

#[derive(Default)]
struct TemporarySlots {
    // Physical slots follow first source use, never HashMap iteration order.
    types: Vec<Type>,
    pools: HashMap<Type, Vec<usize>>,
    used: HashMap<Type, usize>,
    expressions: HashMap<*const checked::Expr, usize>,
    allocation: HashMap<*const checked::Expr, bool>,
}

impl TemporarySlots {
    fn may_allocate(&mut self, allocating: &BTreeSet<usize>, value: &checked::Expr) -> bool {
        if let Some(result) = self.allocation.get(&(value as *const checked::Expr)) {
            return *result;
        }
        let result = match &value.kind {
            checked::ExprKind::DynBox { .. }
            | checked::ExprKind::DynCall { .. }
            | checked::ExprKind::List(_)
            | checked::ExprKind::IndirectCall { .. } => true,
            checked::ExprKind::Unary(_, value)
            | checked::ExprKind::Field(value, _)
            | checked::ExprKind::Coerce(value) => self.may_allocate(allocating, value),
            checked::ExprKind::Binary(_, left, right) => {
                self.may_allocate(allocating, left) || self.may_allocate(allocating, right)
            }
            checked::ExprKind::Call(target, args) => {
                allocating.contains(target)
                    || args.iter().any(|arg| self.may_allocate(allocating, arg))
            }
            checked::ExprKind::Primitive(operation, args) => {
                allocates(*operation) || args.iter().any(|arg| self.may_allocate(allocating, arg))
            }
            checked::ExprKind::Variant { fields, .. } => fields
                .iter()
                .any(|field| self.may_allocate(allocating, field)),
            checked::ExprKind::FrameNew(_) => true,
            checked::ExprKind::FrameStore { frame, value, .. } => {
                self.may_allocate(allocating, frame) || self.may_allocate(allocating, value)
            }
            checked::ExprKind::Record(fields) => fields
                .iter()
                .any(|(_, field)| self.may_allocate(allocating, field)),
            checked::ExprKind::Block(body) => self.block_allocates(allocating, body),
            checked::ExprKind::Match { value, arms } => {
                self.may_allocate(allocating, value)
                    || arms
                        .iter()
                        .any(|arm| self.block_allocates(allocating, &arm.body))
            }
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.may_allocate(allocating, condition)
                    || self.block_allocates(allocating, then_body)
                    || else_body
                        .as_ref()
                        .is_some_and(|body| self.block_allocates(allocating, body))
            }
            _ => false,
        };
        self.allocation.insert(value, result);
        result
    }

    fn block_allocates(&mut self, allocating: &BTreeSet<usize>, body: &checked::Block) -> bool {
        body.statements
            .iter()
            .any(|statement| match &statement.kind {
                checked::StmtKind::Let { value, .. }
                | checked::StmtKind::Assign { value, .. }
                | checked::StmtKind::Assert {
                    condition: value, ..
                }
                | checked::StmtKind::Discard(value)
                | checked::StmtKind::Expr(value)
                | checked::StmtKind::Return(Some(value)) => self.may_allocate(allocating, value),
                checked::StmtKind::While { condition, body } => {
                    self.may_allocate(allocating, condition)
                        || self.block_allocates(allocating, body)
                }
                checked::StmtKind::Cleanup { body, .. } => self.block_allocates(allocating, body),
                checked::StmtKind::Defer { .. } => false,
                checked::StmtKind::Return(None)
                | checked::StmtKind::Break
                | checked::StmtKind::Continue => false,
            })
            || body
                .tail
                .as_ref()
                .is_some_and(|tail| self.may_allocate(allocating, tail))
    }

    fn siblings<'a>(
        &mut self,
        program: &checked::Program,
        allocating: &BTreeSet<usize>,
        values: impl IntoIterator<Item = &'a checked::Expr>,
        mut later_allocation: bool,
    ) {
        let values = values.into_iter().collect::<Vec<_>>();
        let mut protect = vec![false; values.len()];
        for index in (0..values.len()).rev() {
            protect[index] = later_allocation;
            later_allocation |= self.may_allocate(allocating, values[index]);
        }
        for (value, protect) in values.into_iter().zip(protect) {
            self.expression(program, allocating, value, protect);
        }
    }

    fn expression(
        &mut self,
        program: &checked::Program,
        allocating: &BTreeSet<usize>,
        value: &checked::Expr,
        protect: bool,
    ) {
        if protect && managed(program, value.ty) {
            let used = self.used.entry(value.ty).or_default();
            let pool = self.pools.entry(value.ty).or_default();
            if *used == pool.len() {
                pool.push(self.types.len());
                self.types.push(value.ty);
            }
            self.expressions.insert(value, pool[*used]);
            *used += 1;
        }
        // Only a result crossing a later allocation needs a snapshot. Reserve
        // it before children, then release child slots after the result is
        // copied; earlier arguments and enclosing results stay outside this scope.
        let base = self.used.clone();
        match &value.kind {
            checked::ExprKind::Unary(_, value)
            | checked::ExprKind::Field(value, _)
            | checked::ExprKind::Coerce(value) => {
                self.expression(program, allocating, value, false)
            }
            checked::ExprKind::DynBox { value, .. } => {
                self.expression(program, allocating, value, true)
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
                self.siblings(
                    program,
                    allocating,
                    std::iter::once(receiver.as_ref()).chain(arguments),
                    true,
                );
            }
            checked::ExprKind::Binary(_, left, right) => {
                self.siblings(program, allocating, [left.as_ref(), right.as_ref()], false);
            }
            checked::ExprKind::Call(target, args) => {
                self.siblings(program, allocating, args, allocating.contains(target));
            }
            checked::ExprKind::Primitive(operation, args) => {
                self.siblings(program, allocating, args, allocates(*operation));
            }
            checked::ExprKind::Variant { fields, .. } => {
                self.siblings(program, allocating, fields, false);
            }
            checked::ExprKind::List(elements) => {
                self.siblings(program, allocating, elements, true);
            }
            checked::ExprKind::Record(fields) => {
                self.siblings(
                    program,
                    allocating,
                    fields.iter().map(|(_, field)| field),
                    false,
                );
            }
            checked::ExprKind::FrameNew(fields) => {
                self.siblings(
                    program,
                    allocating,
                    fields.iter().map(|(_, field)| field),
                    true,
                );
            }
            checked::ExprKind::FrameStore { frame, value, .. } => {
                self.siblings(program, allocating, [frame.as_ref(), value.as_ref()], false);
            }
            checked::ExprKind::Block(body) => self.block(program, allocating, body),
            checked::ExprKind::Match { value, arms } => {
                // The emitter copies pattern bindings into permanent local
                // roots before entering an arm; matching itself cannot collect.
                self.expression(program, allocating, value, false);
                self.branches(program, allocating, arms.iter().map(|arm| &arm.body));
            }
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.expression(program, allocating, condition, false);
                self.branches(
                    program,
                    allocating,
                    std::iter::once(then_body).chain(else_body),
                );
            }
            _ => {}
        }
        self.used = base;
    }

    fn branches<'a>(
        &mut self,
        program: &checked::Program,
        allocating: &BTreeSet<usize>,
        bodies: impl IntoIterator<Item = &'a checked::Block>,
    ) {
        let base = self.used.clone();
        let mut peak = base.clone();
        for body in bodies {
            self.used.clone_from(&base);
            self.block(program, allocating, body);
            for (ty, used) in &self.used {
                let maximum = peak.entry(*ty).or_default();
                *maximum = (*maximum).max(*used);
            }
        }
        self.used = peak;
    }

    fn block(
        &mut self,
        program: &checked::Program,
        allocating: &BTreeSet<usize>,
        body: &checked::Block,
    ) {
        let base = self.used.clone();
        for statement in &body.statements {
            match &statement.kind {
                checked::StmtKind::Let { value, .. }
                | checked::StmtKind::Assign { value, .. }
                | checked::StmtKind::Assert {
                    condition: value, ..
                }
                | checked::StmtKind::Discard(value)
                | checked::StmtKind::Expr(value)
                | checked::StmtKind::Return(Some(value)) => {
                    self.expression(program, allocating, value, false)
                }
                checked::StmtKind::While { condition, body } => {
                    self.expression(program, allocating, condition, false);
                    self.block(program, allocating, body);
                }
                checked::StmtKind::Return(None)
                | checked::StmtKind::Break
                | checked::StmtKind::Continue
                | checked::StmtKind::Defer { .. }
                | checked::StmtKind::Cleanup { .. } => {}
            }
            // Let/Assign have copied into permanent local roots; other completed
            // statements retain no value. A return never reaches the next one.
            self.used.clone_from(&base);
        }
        if let Some(tail) = &body.tail {
            self.expression(program, allocating, tail, false);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn root_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    program: &checked::Program,
    allocating: &BTreeSet<usize>,
    source: &checked::Function,
    callback_body: Option<&checked::Block>,
    locals: &[Option<PointerValue<'ctx>>],
    borrowed: &HashMap<usize, PointerValue<'ctx>>,
    tracers: &mut HashMap<Type, FunctionValue<'ctx>>,
) -> NativeResult<RootFrame<'ctx>> {
    let mut slots = Vec::new();
    let mut rooted_locals = borrowed.clone();
    let handoffs = immediate_match_handoffs(program, source);
    for (index, ty) in source.locals.iter().enumerate() {
        if managed(program, *ty)
            && locals[index].is_some()
            && !borrowed.contains_key(&index)
            && !handoffs.contains(&index)
        {
            // Keep normal locals nonescaping so LLVM can promote them to SSA.
            // The collector rewrites a separate shadow slot; allocating calls
            // restore ordinary locals from it before source execution resumes.
            let native = native_type(context, program, *ty)?;
            let slot = builder.build_alloca(native, "gc.local")?;
            let initial = if callback_body.is_none() && index < source.params.len() {
                builder.build_load(
                    native,
                    locals[index].ok_or("missing managed parameter")?,
                    "gc.parameter",
                )?
            } else {
                native.const_zero()
            };
            builder.build_store(slot, initial)?;
            rooted_locals.insert(index, slot);
            slots.push((slot, *ty));
        }
    }
    let mut temporary = TemporarySlots::default();
    if callback_body.is_none() {
        for value in &source.requires {
            temporary.expression(program, allocating, value, false);
            temporary.used.clear();
        }
    }
    temporary.block(program, allocating, callback_body.unwrap_or(&source.body));
    let mut storage = Vec::new();
    for ty in temporary.types {
        let native = native_type(context, program, ty)?;
        let slot = builder.build_alloca(native, "gc.temporary")?;
        builder.build_store(slot, native.const_zero())?;
        storage.push(slot);
        slots.push((slot, ty));
    }
    let expressions = temporary
        .expressions
        .into_iter()
        .map(|(value, slot)| (value, storage[slot]))
        .collect();
    for (slot, ty) in &slots {
        let trace = tracer(context, module, program, *ty, tracers)?
            .as_global_value()
            .as_pointer_value();
        super::gc_lower::begin(context, module, builder, *slot, trace)?;
    }
    Ok(RootFrame {
        slots: slots.into_iter().map(|(slot, _)| slot).collect(),
        locals: rooted_locals,
        expressions,
    })
}

fn expressions<'a>(value: &'a checked::Expr, values: &mut Vec<&'a checked::Expr>) {
    walk_expression(value, &mut |value| values.push(value), &mut |_| {});
}

pub(super) fn block_expressions<'a>(
    value: &'a checked::Block,
    values: &mut Vec<&'a checked::Expr>,
) {
    walk_block(value, &mut |value| values.push(value), &mut |_| {});
}

/// Pattern extraction followed immediately by its sole local read has no safe
/// point. The enclosing expression's TemporarySlots handle the result handoff.
/// Count the whole function: checked input can reuse IDs outside lexical scope.
fn immediate_match_handoffs(
    program: &checked::Program,
    source: &checked::Function,
) -> BTreeSet<usize> {
    let mut reads = vec![0usize; source.locals.len()];
    let mut bindings = vec![0usize; source.locals.len()];
    let mut handoffs = BTreeSet::new();
    let mut writes = BTreeSet::new();
    let mut expression = |value: &checked::Expr| match &value.kind {
        checked::ExprKind::Local(id) => {
            if let Some(count) = reads.get_mut(*id) {
                *count += 1;
            }
        }
        checked::ExprKind::Match { arms, .. } => {
            for arm in arms {
                for id in arm.bindings.iter().flatten().chain(arm.whole.iter()) {
                    if let Some(count) = bindings.get_mut(*id) {
                        *count += 1;
                    }
                    if arm.body.statements.is_empty()
                        && arm.body.falls_through
                        && arm.body.tail.as_ref().is_some_and(|tail| {
                            matches!(tail.kind, checked::ExprKind::Local(tail_id) if tail_id == *id)
                        })
                    {
                        handoffs.insert(*id);
                    }
                }
            }
        }
        _ => {}
    };
    let mut statement = |value: &checked::Stmt| {
        if let checked::StmtKind::Let { local, .. } | checked::StmtKind::Assign { local, .. } =
            &value.kind
        {
            writes.insert(*local);
        }
    };
    for value in &source.requires {
        walk_expression(value, &mut expression, &mut statement);
    }
    walk_block(&source.body, &mut expression, &mut statement);
    handoffs.retain(|id| {
        *id >= source.params.len()
            && !writes.contains(id)
            && source
                .locals
                .get(*id)
                .is_some_and(|ty| managed(program, *ty))
            && reads[*id] == 1
            && bindings[*id] == 1
    });
    handoffs
}

fn walk_expression<'a>(
    value: &'a checked::Expr,
    expression: &mut impl FnMut(&'a checked::Expr),
    statement: &mut impl FnMut(&'a checked::Stmt),
) {
    expression(value);
    match &value.kind {
        checked::ExprKind::Unary(_, value)
        | checked::ExprKind::Field(value, _)
        | checked::ExprKind::Coerce(value)
        | checked::ExprKind::DynBox { value, .. } => walk_expression(value, expression, statement),
        checked::ExprKind::DynCall {
            receiver,
            arguments,
            ..
        }
        | checked::ExprKind::IndirectCall {
            callee: receiver,
            arguments,
        } => {
            walk_expression(receiver, expression, statement);
            for argument in arguments {
                walk_expression(argument, expression, statement);
            }
        }
        checked::ExprKind::Binary(_, left, right) => {
            walk_expression(left, expression, statement);
            walk_expression(right, expression, statement);
        }
        checked::ExprKind::Call(_, args)
        | checked::ExprKind::Primitive(_, args)
        | checked::ExprKind::List(args)
        | checked::ExprKind::Variant { fields: args, .. } => {
            for arg in args {
                walk_expression(arg, expression, statement);
            }
        }
        checked::ExprKind::Record(fields) | checked::ExprKind::FrameNew(fields) => {
            for (_, field) in fields {
                walk_expression(field, expression, statement);
            }
        }
        checked::ExprKind::FrameStore { frame, value, .. } => {
            walk_expression(frame, expression, statement);
            walk_expression(value, expression, statement);
        }
        checked::ExprKind::Block(body) => walk_block(body, expression, statement),
        checked::ExprKind::Match { value, arms } => {
            walk_expression(value, expression, statement);
            for arm in arms {
                walk_block(&arm.body, expression, statement);
            }
        }
        checked::ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            walk_expression(condition, expression, statement);
            walk_block(then_body, expression, statement);
            if let Some(body) = else_body {
                walk_block(body, expression, statement);
            }
        }
        _ => {}
    }
}

fn walk_block<'a>(
    value: &'a checked::Block,
    expression: &mut impl FnMut(&'a checked::Expr),
    statement: &mut impl FnMut(&'a checked::Stmt),
) {
    for item in &value.statements {
        statement(item);
        match &item.kind {
            checked::StmtKind::Let { value, .. }
            | checked::StmtKind::Assign { value, .. }
            | checked::StmtKind::Assert {
                condition: value, ..
            }
            | checked::StmtKind::Discard(value)
            | checked::StmtKind::Expr(value)
            | checked::StmtKind::Return(Some(value)) => {
                walk_expression(value, expression, statement)
            }
            checked::StmtKind::While { condition, body } => {
                walk_expression(condition, expression, statement);
                walk_block(body, expression, statement);
            }
            checked::StmtKind::Defer { body, .. } | checked::StmtKind::Cleanup { body, .. } => {
                walk_block(body, expression, statement);
            }
            checked::StmtKind::Return(None)
            | checked::StmtKind::Break
            | checked::StmtKind::Continue => {}
        }
    }
    if let Some(tail) = &value.tail {
        walk_expression(tail, expression, statement);
    }
}

pub(super) fn tracer<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &checked::Program,
    ty: Type,
    tracers: &mut HashMap<Type, FunctionValue<'ctx>>,
) -> NativeResult<FunctionValue<'ctx>> {
    if let Some(function) = tracers.get(&ty) {
        return Ok(*function);
    }
    let pointer = context.ptr_type(AddressSpace::default());
    let function = module.add_function(
        &format!("loom.trace.{}", tracers.len()),
        context.void_type().fn_type(&[pointer.into()], false),
        Some(Linkage::Internal),
    );
    tracers.insert(ty, function);
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let address = function
        .get_first_param()
        .ok_or("missing trace parameter")?
        .into_pointer_value();
    let value = builder.build_load(native_type(context, program, ty)?, address, "root.value")?;
    let trace = TraceEmitter {
        context,
        module,
        program,
        function,
        builder: &builder,
    };
    builder.build_store(address, trace.value(ty, value)?)?;
    builder.build_return(None)?;
    Ok(function)
}

pub(super) fn frame_tracer<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    program: &checked::Program,
    ty: Type,
) -> NativeResult<FunctionValue<'ctx>> {
    let Type::Data(id) = ty else {
        return Err("invalid generated frame type".into());
    };
    let checked::DataKind::Frame(fields) = &program.types[id].kind else {
        return Err("invalid generated frame payload".into());
    };
    let name = format!("loom.frame.trace.{id}");
    if let Some(function) = module.get_function(&name) {
        return Ok(function);
    }
    let pointer = context.ptr_type(AddressSpace::default());
    let function = module.add_function(
        &name,
        context.void_type().fn_type(&[pointer.into()], false),
        Some(Linkage::Internal),
    );
    let builder = context.create_builder();
    builder.position_at_end(context.append_basic_block(function, "entry"));
    let payload = function.get_first_param().unwrap().into_pointer_value();
    let layout = tasks::frame_layout(context, program, ty)?;
    let trace = TraceEmitter {
        context,
        module,
        program,
        function,
        builder: &builder,
    };
    for (index, (_, field)) in fields.iter().enumerate() {
        if managed(program, *field) {
            let address =
                builder.build_struct_gep(layout, payload, index as u32, "frame.trace.field")?;
            let value = builder.build_load(
                native_type(context, program, *field)?,
                address,
                "frame.trace.value",
            )?;
            builder.build_store(address, trace.value(*field, value)?)?;
        }
    }
    builder.build_return(None)?;
    Ok(function)
}

struct TraceEmitter<'a, 'ctx> {
    context: &'ctx Context,
    module: &'a Module<'ctx>,
    program: &'a checked::Program,
    function: FunctionValue<'ctx>,
    builder: &'a Builder<'ctx>,
}

impl<'ctx> TraceEmitter<'_, 'ctx> {
    fn visit(&self, pointer: PointerValue<'ctx>) -> NativeResult<PointerValue<'ctx>> {
        let visit = runtime_function(
            self.context,
            self.module,
            "visit",
            Some(pointer.get_type().into()),
            &[pointer.get_type().into()],
        );
        Ok(self
            .builder
            .build_call(visit, &[pointer.into()], "trace.visited")?
            .try_as_basic_value()
            .basic()
            .ok_or("missing relocated pointer")?
            .into_pointer_value())
    }

    fn value(&self, ty: Type, value: BasicValueEnum<'ctx>) -> NativeResult<BasicValueEnum<'ctx>> {
        Ok(match ty {
            Type::Text | Type::Bytes | Type::List(_) => {
                self.visit(value.into_pointer_value())?.into()
            }
            Type::Dyn(_) => {
                let data = self.builder.build_extract_value(
                    value.into_struct_value(),
                    0,
                    "trace.dyn.data",
                )?;
                self.builder
                    .build_insert_value(
                        value.into_struct_value(),
                        self.visit(data.into_pointer_value())?,
                        0,
                        "trace.dyn",
                    )?
                    .into_struct_value()
                    .into()
            }
            Type::Data(id) => match &self.program.types[id].kind {
                checked::DataKind::Task(_) => value,
                checked::DataKind::Frame(_) => self.visit(value.into_pointer_value())?.into(),
                checked::DataKind::Refined(base) => self.value(*base, value)?,
                checked::DataKind::Record(fields) => {
                    let mut updated = value.into_struct_value();
                    for (index, (_, ty)) in fields.iter().enumerate() {
                        if managed(self.program, *ty) {
                            let field = self.value(
                                *ty,
                                self.builder.build_extract_value(
                                    updated,
                                    index as u32,
                                    "trace.field",
                                )?,
                            )?;
                            updated = self
                                .builder
                                .build_insert_value(updated, field, index as u32, "trace.record")?
                                .into_struct_value();
                        }
                    }
                    updated.into()
                }
                checked::DataKind::Enum(variants) => {
                    let tag = self
                        .builder
                        .build_extract_value(value.into_struct_value(), 0, "trace.tag")?
                        .into_int_value();
                    let payload = self
                        .builder
                        .build_extract_value(value.into_struct_value(), 1, "trace.payload")?
                        .into_array_value();
                    self.builder
                        .build_insert_value(
                            value.into_struct_value(),
                            self.enum_words(tag, payload, variants, 0)?,
                            1,
                            "trace.enum",
                        )?
                        .into_struct_value()
                        .into()
                }
            },
            _ => value,
        })
    }

    fn enum_words(
        &self,
        tag: IntValue<'ctx>,
        payload: inkwell::values::ArrayValue<'ctx>,
        variants: &[(String, Vec<Type>)],
        offset: usize,
    ) -> NativeResult<inkwell::values::ArrayValue<'ctx>> {
        let done = self.context.append_basic_block(self.function, "trace.done");
        let cases = variants
            .iter()
            .enumerate()
            .filter(|(_, (_, fields))| fields.iter().any(|ty| managed(self.program, *ty)))
            .map(|(index, (_, fields))| {
                (
                    index,
                    fields,
                    self.context
                        .append_basic_block(self.function, "trace.variant"),
                )
            })
            .collect::<Vec<_>>();
        let origin = self
            .builder
            .get_insert_block()
            .ok_or("missing trace block")?;
        self.builder.build_switch(
            tag,
            done,
            &cases
                .iter()
                .map(|(index, _, block)| {
                    (
                        self.context.i64_type().const_int(*index as u64, false),
                        *block,
                    )
                })
                .collect::<Vec<_>>(),
        )?;
        let mut incoming = vec![(payload, origin)];
        for (_, fields, block) in cases {
            self.builder.position_at_end(block);
            let mut field_offset = offset;
            let mut updated = payload;
            for ty in fields {
                updated = self.words(*ty, updated, field_offset)?;
                field_offset += value_words(self.program, *ty)?;
            }
            incoming.push((
                updated,
                self.builder
                    .get_insert_block()
                    .ok_or("missing trace block")?,
            ));
            self.builder.build_unconditional_branch(done)?;
        }
        self.builder.position_at_end(done);
        let phi = self
            .builder
            .build_phi(payload.get_type(), "trace.payload.updated")?;
        for (value, block) in &incoming {
            phi.add_incoming(&[(value, *block)]);
        }
        Ok(phi.as_basic_value().into_array_value())
    }

    fn words(
        &self,
        ty: Type,
        payload: inkwell::values::ArrayValue<'ctx>,
        offset: usize,
    ) -> NativeResult<inkwell::values::ArrayValue<'ctx>> {
        Ok(match ty {
            Type::Text | Type::Bytes | Type::List(_) | Type::Dyn(_) => {
                let word = self
                    .builder
                    .build_extract_value(payload, offset as u32, "trace.word")?
                    .into_int_value();
                let pointer = self.visit(self.builder.build_int_to_ptr(
                    word,
                    self.context.ptr_type(AddressSpace::default()),
                    "trace.pointer",
                )?)?;
                let word = self.builder.build_ptr_to_int(
                    pointer,
                    self.context.i64_type(),
                    "trace.word.updated",
                )?;
                self.builder
                    .build_insert_value(payload, word, offset as u32, "trace.payload.word")?
                    .into_array_value()
            }
            Type::Data(id) => match &self.program.types[id].kind {
                checked::DataKind::Task(_) => payload,
                checked::DataKind::Frame(_) => self.words(Type::Text, payload, offset)?,
                checked::DataKind::Refined(base) => self.words(*base, payload, offset)?,
                checked::DataKind::Record(fields) => {
                    let mut offset = offset;
                    let mut updated = payload;
                    for (_, ty) in fields {
                        updated = self.words(*ty, updated, offset)?;
                        offset += value_words(self.program, *ty)?;
                    }
                    updated
                }
                checked::DataKind::Enum(variants) => {
                    let tag = self
                        .builder
                        .build_extract_value(payload, offset as u32, "trace.nested.tag")?
                        .into_int_value();
                    self.enum_words(tag, payload, variants, offset + 1)?
                }
            },
            _ => payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Span;

    fn program() -> checked::Program {
        checked::Program {
            function_types: vec![],
            interfaces: vec![],
            witnesses: vec![],
            types: vec![checked::Data {
                name: "Choice".into(),
                kind: checked::DataKind::Enum(vec![
                    ("A".into(), vec![Type::Text]),
                    ("B".into(), vec![Type::Text]),
                ]),
            }],
            lists: vec![],
            functions: vec![],
            entry: None,
            tests: vec![],
            test_names: vec![],
            exports: vec![],
        }
    }

    fn expr(kind: checked::ExprKind, ty: Type) -> checked::Expr {
        checked::Expr {
            kind,
            ty,
            span: Span::default(),
        }
    }

    fn block(statements: Vec<checked::Expr>, tail: checked::Expr) -> checked::Block {
        checked::Block {
            statements: statements
                .into_iter()
                .map(|value| checked::Stmt {
                    kind: checked::StmtKind::Discard(value),
                    span: Span::default(),
                })
                .collect(),
            tail: Some(Box::new(tail)),
            falls_through: true,
        }
    }

    fn local(ty: Type) -> checked::Expr {
        expr(checked::ExprKind::Local(0), ty)
    }

    fn slot(slots: &TemporarySlots, value: &checked::Expr) -> usize {
        slots.expressions[&(value as *const checked::Expr)]
    }

    fn allocating_call(ty: Type) -> checked::Expr {
        expr(checked::ExprKind::Call(0, vec![local(ty)]), ty)
    }

    fn concat() -> checked::Expr {
        expr(
            checked::ExprKind::Primitive(
                Primitive::TextConcat,
                vec![local(Type::Text), local(Type::Text)],
            ),
            Type::Text,
        )
    }

    #[test]
    fn match_handoffs_require_one_binding_one_read_and_no_intervening_code() {
        let source = || checked::Function {
            name: "handoff".into(),
            params: vec![],
            result: Type::Text,
            locals: vec![Type::Text],
            requires: vec![],
            span: Span::default(),
            body: block(
                vec![],
                expr(
                    checked::ExprKind::Match {
                        value: Box::new(expr(
                            checked::ExprKind::Variant {
                                variant: 0,
                                fields: vec![expr(
                                    checked::ExprKind::Text("text".into()),
                                    Type::Text,
                                )],
                            },
                            Type::Data(0),
                        )),
                        arms: vec![checked::MatchArm {
                            variant: Some(0),
                            bindings: vec![Some(0)],
                            whole: None,
                            body: block(vec![], local(Type::Text)),
                        }],
                    },
                    Type::Text,
                ),
            ),
        };
        assert_eq!(
            immediate_match_handoffs(&program(), &source()),
            BTreeSet::from([0])
        );

        // Checked input may reuse IDs in ways the source checker never emits.
        for case in 0..9 {
            let mut function = source();
            let checked::ExprKind::Match { arms, .. } =
                &mut function.body.tail.as_mut().unwrap().kind
            else {
                panic!();
            };
            match case {
                0 => function.params.push(Type::Text),
                1 => arms.push(arms[0].clone()),
                2 => arms[0].bindings.push(Some(0)),
                3 => {
                    arms[0].body = block(
                        vec![expr(
                            checked::ExprKind::Text("statement".into()),
                            Type::Text,
                        )],
                        local(Type::Text),
                    );
                }
                4 => function.requires.push(local(Type::Text)),
                5 | 6 => function.body.statements.push(checked::Stmt {
                    kind: if case == 5 {
                        checked::StmtKind::Let {
                            local: 0,
                            value: expr(checked::ExprKind::Text("write".into()), Type::Text),
                        }
                    } else {
                        checked::StmtKind::Assign {
                            local: 0,
                            value: expr(checked::ExprKind::Text("write".into()), Type::Text),
                        }
                    },
                    span: Span::default(),
                }),
                7 | 8 => function.body.statements.push(checked::Stmt {
                    kind: if case == 7 {
                        checked::StmtKind::Defer {
                            id: 0,
                            body: block(vec![local(Type::Text)], concat()),
                        }
                    } else {
                        checked::StmtKind::Cleanup {
                            id: 0,
                            body: block(vec![local(Type::Text)], concat()),
                        }
                    },
                    span: Span::default(),
                }),
                _ => unreachable!(),
            }
            assert!(
                immediate_match_handoffs(&program(), &function).is_empty(),
                "case {case}"
            );
        }

        let mut whole = source();
        whole.locals[0] = Type::Data(0);
        whole.result = Type::Data(0);
        let tail = whole.body.tail.as_mut().unwrap();
        tail.ty = Type::Data(0);
        let checked::ExprKind::Match { arms, .. } = &mut tail.kind else {
            panic!();
        };
        arms[0].variant = None;
        arms[0].bindings.clear();
        arms[0].whole = Some(0);
        arms[0].body = block(vec![], local(Type::Data(0)));
        assert_eq!(
            immediate_match_handoffs(&program(), &whole),
            BTreeSet::from([0])
        );
    }

    #[test]
    fn process_operations_root_arguments_even_with_a_scalar_result() {
        for (operation, params) in [
            (
                Primitive::ProcessCaptureConfigured,
                vec![
                    Type::List(0),
                    Type::Text,
                    Type::Int,
                    Type::List(0),
                    Type::Bytes,
                    Type::Bytes,
                ],
            ),
            (
                Primitive::ProcessCaptureInputConfigured,
                vec![
                    Type::List(0),
                    Type::Bytes,
                    Type::Text,
                    Type::Int,
                    Type::List(0),
                    Type::Bytes,
                    Type::Bytes,
                ],
            ),
            (Primitive::EnvGet, vec![Type::Text, Type::Bytes]),
        ] {
            let value = expr(
                checked::ExprKind::Primitive(
                    operation,
                    params.iter().map(|ty| local(*ty)).collect(),
                ),
                Type::Int,
            );
            let mut slots = TemporarySlots::default();
            slots.expression(&program(), &BTreeSet::new(), &value, false);
            assert_eq!(
                slots.types,
                params
                    .into_iter()
                    .filter(|ty| managed(&program(), *ty))
                    .collect::<Vec<_>>()
            );
            assert!(allocates(operation));
        }
    }

    #[test]
    fn statement_temporaries_reuse_exact_types_but_not_enclosing_results() {
        let value = expr(
            checked::ExprKind::Block(block(
                vec![
                    allocating_call(Type::Text),
                    allocating_call(Type::Bytes),
                    allocating_call(Type::Text),
                ],
                local(Type::Text),
            )),
            Type::Text,
        );
        let mut slots = TemporarySlots::default();
        slots.expression(&program(), &BTreeSet::from([0]), &value, true);
        assert_eq!(slots.types, [Type::Text, Type::Text, Type::Bytes]);
        let checked::ExprKind::Block(body) = &value.kind else {
            panic!();
        };
        for (index, statement) in body.statements.iter().enumerate() {
            let checked::StmtKind::Discard(value) = &statement.kind else {
                panic!();
            };
            let checked::ExprKind::Call(_, args) = &value.kind else {
                panic!();
            };
            assert_eq!(slot(&slots, &args[0]), if index == 1 { 2 } else { 1 });
            assert!(
                !slots
                    .expressions
                    .contains_key(&(value as *const checked::Expr))
            );
        }
        assert!(
            !slots
                .expressions
                .contains_key(&(body.tail.as_ref().unwrap().as_ref() as *const checked::Expr))
        );
        assert_eq!(slot(&slots, &value), 0);

        // Reads, discarded calls with a nonallocating target, and immediate
        // local/return handoffs need no temporary roots.
        let mut slots = TemporarySlots::default();
        slots.expression(&program(), &BTreeSet::new(), &value, false);
        assert!(slots.types.is_empty());
    }

    #[test]
    fn branch_pools_preserve_pending_arguments_without_rooting_pure_reads() {
        let conditional = expr(
            checked::ExprKind::If {
                condition: Box::new(expr(
                    checked::ExprKind::Primitive(
                        Primitive::TextEqual,
                        vec![local(Type::Text), local(Type::Text)],
                    ),
                    Type::Bool,
                )),
                then_body: block(vec![], concat()),
                else_body: Some(block(vec![], concat())),
            },
            Type::Text,
        );
        let matched = expr(
            checked::ExprKind::Match {
                value: Box::new(local(Type::Data(0))),
                arms: vec![conditional, concat()]
                    .into_iter()
                    .enumerate()
                    .map(|(variant, tail)| checked::MatchArm {
                        variant: Some(variant),
                        bindings: vec![None],
                        whole: None,
                        body: block(vec![local(Type::Data(0))], tail),
                    })
                    .collect(),
            },
            Type::Text,
        );
        let call = expr(
            checked::ExprKind::Call(0, vec![local(Type::Text), matched]),
            Type::Text,
        );
        let mut slots = TemporarySlots::default();
        slots.expression(&program(), &BTreeSet::from([0]), &call, false);
        assert_eq!(slots.types, [Type::Text; 4]);
        let checked::ExprKind::Call(_, arguments) = &call.kind else {
            panic!();
        };
        let checked::ExprKind::Match { value, arms } = &arguments[1].kind else {
            panic!();
        };
        let first = arms[0].body.tail.as_ref().unwrap();
        let second = arms[1].body.tail.as_ref().unwrap();
        assert_ne!(slot(&slots, &arguments[0]), slot(&slots, &arguments[1]));
        let checked::StmtKind::Discard(inner) = &arms[0].body.statements[0].kind else {
            panic!();
        };
        assert!(
            !slots
                .expressions
                .contains_key(&(value.as_ref() as *const checked::Expr))
        );
        assert!(
            !slots
                .expressions
                .contains_key(&(inner as *const checked::Expr))
        );
        let checked::ExprKind::If {
            condition,
            then_body,
            else_body,
        } = &first.kind
        else {
            panic!();
        };
        let checked::ExprKind::Primitive(_, yes) = &then_body.tail.as_ref().unwrap().kind else {
            panic!();
        };
        let checked::ExprKind::Primitive(_, no) =
            &else_body.as_ref().unwrap().tail.as_ref().unwrap().kind
        else {
            panic!();
        };
        let checked::ExprKind::Primitive(_, other) = &second.kind else {
            panic!();
        };
        for index in 0..2 {
            assert_eq!(slot(&slots, &yes[index]), slot(&slots, &no[index]));
            assert_eq!(slot(&slots, &yes[index]), slot(&slots, &other[index]));
            assert_ne!(slot(&slots, &yes[index]), slot(&slots, &arguments[0]));
            assert_ne!(slot(&slots, &yes[index]), slot(&slots, &arguments[1]));
        }
        let checked::ExprKind::Primitive(_, values) = &condition.kind else {
            panic!();
        };
        for condition in values {
            assert!(
                !slots
                    .expressions
                    .contains_key(&(condition as *const checked::Expr))
            );
        }
    }

    #[test]
    fn list_literals_protect_managed_items_but_need_no_scalar_item_roots() {
        for (ty, roots) in [(Type::Int, 0), (Type::Text, 2)] {
            let value = expr(
                checked::ExprKind::List(vec![local(ty), local(ty)]),
                Type::List(0),
            );
            let mut slots = TemporarySlots::default();
            slots.expression(&program(), &BTreeSet::new(), &value, false);
            assert_eq!(slots.types.len(), roots);
            assert!(slots.may_allocate(&BTreeSet::new(), &value));
            if let checked::ExprKind::List(elements) = &value.kind
                && roots > 0
            {
                assert_ne!(slot(&slots, &elements[0]), slot(&slots, &elements[1]));
            }
        }
    }

    #[test]
    fn earlier_fields_survive_allocating_siblings_without_rooting_the_result() {
        let value = expr(
            checked::ExprKind::Variant {
                variant: 0,
                fields: vec![local(Type::Text), concat()],
            },
            Type::Data(0),
        );
        let mut slots = TemporarySlots::default();
        slots.expression(&program(), &BTreeSet::new(), &value, false);
        let checked::ExprKind::Variant { fields, .. } = &value.kind else {
            panic!();
        };
        let checked::ExprKind::Primitive(_, args) = &fields[1].kind else {
            panic!();
        };
        assert_eq!(slots.types, [Type::Text; 3]);
        assert_ne!(slot(&slots, &fields[0]), slot(&slots, &args[0]));
        assert_ne!(slot(&slots, &fields[0]), slot(&slots, &args[1]));
        assert!(
            !slots
                .expressions
                .contains_key(&(&fields[1] as *const checked::Expr))
        );
        assert!(
            !slots
                .expressions
                .contains_key(&(&value as *const checked::Expr))
        );
    }
}
