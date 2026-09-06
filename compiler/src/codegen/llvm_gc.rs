//! Precise, fixed stack roots for managed values; scalar functions need none.

use super::*;

pub(super) struct RootFrame<'ctx> {
    pub checkpoint: Option<IntValue<'ctx>>,
    pub expressions: HashMap<*const checked::Expr, PointerValue<'ctx>>,
}

impl RootFrame<'_> {
    pub fn empty() -> Self {
        Self {
            checkpoint: None,
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
                checked::ExprKind::DynBox { .. } | checked::ExprKind::DynCall { .. } => {
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

fn allocates(operation: Primitive) -> bool {
    match operation {
        Primitive::FloatFormat
        | Primitive::TextConcat
        | Primitive::TextSlice
        | Primitive::ArgText
        | Primitive::BytesNew
        | Primitive::BytesPush
        | Primitive::BytesTextCopy
        | Primitive::ListNew
        | Primitive::ListPush
        | Primitive::Read
        | Primitive::DirectoryRead
        | Primitive::PathCanonical => true,
        Primitive::FloatFromInt
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
        | Primitive::BytesUtf8
        | Primitive::ListLen
        | Primitive::ListGet
        | Primitive::ListSet
        | Primitive::Open
        | Primitive::Create
        | Primitive::Write
        | Primitive::Close
        | Primitive::PathKind => false,
    }
}

pub(super) fn managed(program: &checked::Program, ty: Type) -> bool {
    match ty {
        Type::Text | Type::Bytes | Type::List(_) | Type::Dyn(_) => true,
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Refined(base) => managed(program, *base),
            checked::DataKind::Record(fields) => fields.iter().any(|(_, ty)| managed(program, *ty)),
            checked::DataKind::Enum(variants) => variants
                .iter()
                .any(|(_, fields)| fields.iter().any(|ty| managed(program, *ty))),
        },
        Type::Int | Type::Float | Type::Bool | Type::Unit | Type::Parameter(_) => false,
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
        module.add_function(&name, ty, None)
    })
}

#[derive(Default)]
struct TemporarySlots {
    // Physical slots follow first source use, never HashMap iteration order.
    types: Vec<Type>,
    pools: HashMap<Type, Vec<usize>>,
    used: HashMap<Type, usize>,
    expressions: HashMap<*const checked::Expr, usize>,
}

impl TemporarySlots {
    fn expression(&mut self, program: &checked::Program, value: &checked::Expr) {
        if managed(program, value.ty) {
            let used = self.used.entry(value.ty).or_default();
            let pool = self.pools.entry(value.ty).or_default();
            if *used == pool.len() {
                pool.push(self.types.len());
                self.types.push(value.ty);
            }
            self.expressions.insert(value, pool[*used]);
            *used += 1;
        }
        // Reserve the result before its children. Previously evaluated sibling
        // arguments/fields and enclosing results remain outside nested scopes.
        match &value.kind {
            checked::ExprKind::Unary(_, value)
            | checked::ExprKind::Field(value, _)
            | checked::ExprKind::Coerce(value)
            | checked::ExprKind::DynBox { value, .. } => self.expression(program, value),
            checked::ExprKind::DynCall {
                receiver,
                arguments,
                ..
            } => {
                self.expression(program, receiver);
                for argument in arguments {
                    self.expression(program, argument);
                }
            }
            checked::ExprKind::Binary(_, left, right) => {
                self.expression(program, left);
                self.expression(program, right);
            }
            checked::ExprKind::Call(_, args)
            | checked::ExprKind::Primitive(_, args)
            | checked::ExprKind::Variant { fields: args, .. } => {
                for arg in args {
                    self.expression(program, arg);
                }
            }
            checked::ExprKind::Record(fields) => {
                for (_, field) in fields {
                    self.expression(program, field);
                }
            }
            checked::ExprKind::Block(body) => self.block(program, body),
            checked::ExprKind::Match { value, arms } => {
                self.expression(program, value);
                self.branches(program, arms.iter().map(|arm| &arm.body));
            }
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.expression(program, condition);
                self.branches(program, std::iter::once(then_body).chain(else_body));
            }
            _ => {}
        }
    }

    fn branches<'a>(
        &mut self,
        program: &checked::Program,
        bodies: impl IntoIterator<Item = &'a checked::Block>,
    ) {
        let base = self.used.clone();
        let mut peak = base.clone();
        for body in bodies {
            self.used.clone_from(&base);
            self.block(program, body);
            for (ty, used) in &self.used {
                let maximum = peak.entry(*ty).or_default();
                *maximum = (*maximum).max(*used);
            }
        }
        self.used = peak;
    }

    fn block(&mut self, program: &checked::Program, body: &checked::Block) {
        let base = self.used.clone();
        for statement in &body.statements {
            match &statement.kind {
                checked::StmtKind::Let { value, .. }
                | checked::StmtKind::Assign { value, .. }
                | checked::StmtKind::Assert(value)
                | checked::StmtKind::Discard(value)
                | checked::StmtKind::Expr(value)
                | checked::StmtKind::Return(Some(value)) => self.expression(program, value),
                checked::StmtKind::While { condition, body } => {
                    self.expression(program, condition);
                    self.block(program, body);
                }
                checked::StmtKind::Return(None) => {}
            }
            // Let/Assign have copied into permanent local roots; other completed
            // statements retain no value. A return never reaches the next one.
            self.used.clone_from(&base);
        }
        if let Some(tail) = &body.tail {
            self.expression(program, tail);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn root_function<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    program: &checked::Program,
    source: &checked::Function,
    locals: &[Option<PointerValue<'ctx>>],
    size_type: IntType<'ctx>,
    tracers: &mut HashMap<Type, FunctionValue<'ctx>>,
) -> NativeResult<RootFrame<'ctx>> {
    let mut slots = Vec::new();
    for (index, ty) in source.locals.iter().enumerate() {
        if managed(program, *ty) {
            let slot = locals[index].ok_or("missing managed local")?;
            if index >= source.params.len() {
                builder.build_store(slot, native_type(context, program, *ty)?.const_zero())?;
            }
            slots.push((slot, *ty));
        }
    }
    let mut temporary = TemporarySlots::default();
    for value in &source.requires {
        temporary.expression(program, value);
        temporary.used.clear();
    }
    temporary.block(program, &source.body);
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
    if slots.is_empty() {
        return Ok(RootFrame {
            checkpoint: None,
            expressions,
        });
    }
    let pointer = context.ptr_type(AddressSpace::default());
    let root_type = context.struct_type(&[pointer.into(), pointer.into()], false);
    let table_type =
        root_type.array_type(u32::try_from(slots.len()).map_err(|_| "too many root slots")?);
    let table = builder.build_alloca(table_type, "gc.roots")?;
    let mut entries = table_type.const_zero();
    for (index, (slot, ty)) in slots.iter().enumerate() {
        let trace = tracer(context, module, program, *ty, tracers)?
            .as_global_value()
            .as_pointer_value();
        let entry = builder
            .build_insert_value(root_type.const_zero(), *slot, 0, "gc.address")?
            .into_struct_value();
        let entry = builder
            .build_insert_value(entry, trace, 1, "gc.trace")?
            .into_struct_value();
        entries = builder
            .build_insert_value(entries, entry, index as u32, "gc.entry")?
            .into_array_value();
    }
    builder.build_store(table, entries)?;
    let enter = runtime_function(
        context,
        module,
        "roots_enter",
        Some(size_type.into()),
        &[pointer.into(), size_type.into()],
    );
    let checkpoint = builder
        .build_call(
            enter,
            &[
                table.into(),
                size_type.const_int(slots.len() as u64, false).into(),
            ],
            "gc.checkpoint",
        )?
        .try_as_basic_value()
        .basic()
        .ok_or("missing root checkpoint")?
        .into_int_value();
    Ok(RootFrame {
        checkpoint: Some(checkpoint),
        expressions,
    })
}

fn expressions<'a>(value: &'a checked::Expr, values: &mut Vec<&'a checked::Expr>) {
    values.push(value);
    match &value.kind {
        checked::ExprKind::Unary(_, value)
        | checked::ExprKind::Field(value, _)
        | checked::ExprKind::Coerce(value)
        | checked::ExprKind::DynBox { value, .. } => expressions(value, values),
        checked::ExprKind::DynCall {
            receiver,
            arguments,
            ..
        } => {
            expressions(receiver, values);
            for argument in arguments {
                expressions(argument, values);
            }
        }
        checked::ExprKind::Binary(_, left, right) => {
            expressions(left, values);
            expressions(right, values);
        }
        checked::ExprKind::Call(_, args)
        | checked::ExprKind::Primitive(_, args)
        | checked::ExprKind::Variant { fields: args, .. } => {
            for arg in args {
                expressions(arg, values);
            }
        }
        checked::ExprKind::Record(fields) => {
            for (_, field) in fields {
                expressions(field, values);
            }
        }
        checked::ExprKind::Block(body) => block_expressions(body, values),
        checked::ExprKind::Match { value, arms } => {
            expressions(value, values);
            for arm in arms {
                block_expressions(&arm.body, values);
            }
        }
        checked::ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            expressions(condition, values);
            block_expressions(then_body, values);
            if let Some(body) = else_body {
                block_expressions(body, values);
            }
        }
        _ => {}
    }
}

fn block_expressions<'a>(value: &'a checked::Block, values: &mut Vec<&'a checked::Expr>) {
    for statement in &value.statements {
        match &statement.kind {
            checked::StmtKind::Let { value, .. }
            | checked::StmtKind::Assign { value, .. }
            | checked::StmtKind::Assert(value)
            | checked::StmtKind::Discard(value)
            | checked::StmtKind::Expr(value)
            | checked::StmtKind::Return(Some(value)) => expressions(value, values),
            checked::StmtKind::While { condition, body } => {
                expressions(condition, values);
                block_expressions(body, values);
            }
            checked::StmtKind::Return(None) => {}
        }
    }
    if let Some(tail) = &value.tail {
        expressions(tail, values);
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
    let value = builder.build_load(
        native_type(context, program, ty)?,
        function
            .get_first_param()
            .ok_or("missing trace parameter")?
            .into_pointer_value(),
        "root.value",
    )?;
    let trace = TraceEmitter {
        context,
        module,
        program,
        function,
        builder: &builder,
    };
    trace.value(ty, value)?;
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
    fn mark(&self, pointer: PointerValue<'ctx>) -> NativeResult<()> {
        let mark = runtime_function(
            self.context,
            self.module,
            "mark",
            None,
            &[pointer.get_type().into()],
        );
        self.builder.build_call(mark, &[pointer.into()], "")?;
        Ok(())
    }

    fn value(&self, ty: Type, value: BasicValueEnum<'ctx>) -> NativeResult<()> {
        match ty {
            Type::Text | Type::Bytes | Type::List(_) => self.mark(value.into_pointer_value())?,
            Type::Dyn(_) => {
                let data = self.builder.build_extract_value(
                    value.into_struct_value(),
                    0,
                    "trace.dyn.data",
                )?;
                self.mark(data.into_pointer_value())?;
            }
            Type::Data(id) => match &self.program.types[id].kind {
                checked::DataKind::Refined(base) => self.value(*base, value)?,
                checked::DataKind::Record(fields) => {
                    for (index, (_, ty)) in fields.iter().enumerate() {
                        if managed(self.program, *ty) {
                            self.value(
                                *ty,
                                self.builder.build_extract_value(
                                    value.into_struct_value(),
                                    index as u32,
                                    "trace.field",
                                )?,
                            )?;
                        }
                    }
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
                    self.enum_words(tag, payload, variants, 0)?;
                }
            },
            _ => {}
        }
        Ok(())
    }

    fn enum_words(
        &self,
        tag: IntValue<'ctx>,
        payload: inkwell::values::ArrayValue<'ctx>,
        variants: &[(String, Vec<Type>)],
        offset: usize,
    ) -> NativeResult<()> {
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
        for (_, fields, block) in cases {
            self.builder.position_at_end(block);
            let mut field_offset = offset;
            for ty in fields {
                self.words(*ty, payload, field_offset)?;
                field_offset += value_words(self.program, *ty)?;
            }
            self.builder.build_unconditional_branch(done)?;
        }
        self.builder.position_at_end(done);
        Ok(())
    }

    fn words(
        &self,
        ty: Type,
        payload: inkwell::values::ArrayValue<'ctx>,
        offset: usize,
    ) -> NativeResult<()> {
        match ty {
            Type::Text | Type::Bytes | Type::List(_) | Type::Dyn(_) => {
                let word = self
                    .builder
                    .build_extract_value(payload, offset as u32, "trace.word")?
                    .into_int_value();
                self.mark(self.builder.build_int_to_ptr(
                    word,
                    self.context.ptr_type(AddressSpace::default()),
                    "trace.pointer",
                )?)?;
            }
            Type::Data(id) => match &self.program.types[id].kind {
                checked::DataKind::Refined(base) => self.words(*base, payload, offset)?,
                checked::DataKind::Record(fields) => {
                    let mut offset = offset;
                    for (_, ty) in fields {
                        self.words(*ty, payload, offset)?;
                        offset += value_words(self.program, *ty)?;
                    }
                }
                checked::DataKind::Enum(variants) => {
                    let tag = self
                        .builder
                        .build_extract_value(payload, offset as u32, "trace.nested.tag")?
                        .into_int_value();
                    self.enum_words(tag, payload, variants, offset + 1)?;
                }
            },
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Span;

    fn program() -> checked::Program {
        checked::Program {
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

    #[test]
    fn statement_temporaries_reuse_exact_types_but_not_enclosing_results() {
        let value = expr(
            checked::ExprKind::Block(block(
                vec![local(Type::Text), local(Type::Bytes), local(Type::Text)],
                local(Type::Text),
            )),
            Type::Text,
        );
        let mut slots = TemporarySlots::default();
        slots.expression(&program(), &value);
        assert_eq!(slots.types, [Type::Text, Type::Text, Type::Bytes]);
        let checked::ExprKind::Block(body) = &value.kind else {
            panic!();
        };
        for (index, statement) in body.statements.iter().enumerate() {
            let checked::StmtKind::Discard(value) = &statement.kind else {
                panic!();
            };
            assert_eq!(slot(&slots, value), if index == 1 { 2 } else { 1 });
        }
        assert_eq!(slot(&slots, body.tail.as_ref().unwrap()), 1);
        assert_eq!(slot(&slots, &value), 0);
    }

    #[test]
    fn branch_pools_preserve_argument_condition_and_scrutinee_prefixes() {
        let conditional = expr(
            checked::ExprKind::If {
                condition: Box::new(expr(
                    checked::ExprKind::Primitive(
                        Primitive::TextEqual,
                        vec![local(Type::Text), local(Type::Text)],
                    ),
                    Type::Bool,
                )),
                then_body: block(vec![], local(Type::Text)),
                else_body: Some(block(vec![], local(Type::Text))),
            },
            Type::Text,
        );
        let matched = expr(
            checked::ExprKind::Match {
                value: Box::new(local(Type::Data(0))),
                arms: vec![conditional, local(Type::Text)]
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
        slots.expression(&program(), &call);
        let checked::ExprKind::Call(_, arguments) = &call.kind else {
            panic!();
        };
        let checked::ExprKind::Match { value, arms } = &arguments[1].kind else {
            panic!();
        };
        let first = arms[0].body.tail.as_ref().unwrap();
        let second = arms[1].body.tail.as_ref().unwrap();
        assert_eq!(slot(&slots, first), slot(&slots, second));
        assert_ne!(slot(&slots, first), slot(&slots, &arguments[0]));
        assert_ne!(slot(&slots, first), slot(&slots, &arguments[1]));
        let checked::StmtKind::Discard(inner) = &arms[0].body.statements[0].kind else {
            panic!();
        };
        assert_ne!(slot(&slots, value), slot(&slots, inner));
        let checked::ExprKind::If {
            condition,
            then_body,
            else_body,
        } = &first.kind
        else {
            panic!();
        };
        let yes = then_body.tail.as_ref().unwrap();
        let no = else_body.as_ref().unwrap().tail.as_ref().unwrap();
        assert_eq!(slot(&slots, yes), slot(&slots, no));
        let checked::ExprKind::Primitive(_, values) = &condition.kind else {
            panic!();
        };
        for condition in values {
            assert_ne!(slot(&slots, condition), slot(&slots, yes));
        }
    }
}
