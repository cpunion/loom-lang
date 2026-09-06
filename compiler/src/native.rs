//! Direct native lowering for the checked seed language. No universal values or executor.

use crate::model::{Binary, Primitive, Type, Unary, checked};
use inkwell::{
    AddressSpace, IntPredicate, OptimizationLevel,
    basic_block::BasicBlock,
    builder::Builder,
    context::Context,
    intrinsics::Intrinsic,
    module::{Linkage, Module},
    passes::PassBuilderOptions,
    targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine},
    types::{BasicType, BasicTypeEnum, IntType},
    values::{BasicValueEnum, FunctionValue, IntValue, PointerValue},
};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
};

#[path = "native_gc.rs"]
mod gc;

type NativeResult<T> = Result<T, Box<dyn std::error::Error>>;

pub fn emit(
    program: &checked::Program,
    test_mode: bool,
    object: &Path,
    llvm_ir: Option<&Path>,
) -> Result<bool, String> {
    emit_checked(program, test_mode, object, llvm_ir).map_err(|error| error.to_string())
}

fn emit_checked(
    program: &checked::Program,
    test_mode: bool,
    object: &Path,
    llvm_ir: Option<&Path>,
) -> NativeResult<bool> {
    if !cfg!(unix) {
        return Err("seed native emission currently requires a Unix host".into());
    }
    let roots = if test_mode {
        program.tests.clone()
    } else if let Some(entry) = program.entry {
        vec![entry]
    } else {
        program.exports.clone()
    };
    let library = !test_mode && program.entry.is_none();
    let reachable = reachable_functions(program, &roots)?;
    let allocating = gc::allocating_functions(program, &reachable);
    Target::initialize_native(&InitializationConfig::default())?;
    let triple = TargetMachine::get_default_triple();
    let machine = Target::from_triple(&triple)
        .map_err(|error| error.to_string())?
        .create_target_machine(
            &triple,
            &TargetMachine::get_host_cpu_name().to_string(),
            &TargetMachine::get_host_cpu_features().to_string(),
            OptimizationLevel::Default,
            RelocMode::PIC,
            CodeModel::Default,
        )
        .ok_or("LLVM could not create a native target machine")?;
    let context = Context::create();
    let module = context.create_module("loom.seed");
    module.set_triple(&triple);
    module.set_data_layout(&machine.get_target_data().get_data_layout());
    let builder = context.create_builder();
    let mut tracers = HashMap::new();
    let mut functions = vec![None; program.functions.len()];
    for &id in &reachable {
        let source = &program.functions[id];
        let params = source
            .params
            .iter()
            .map(|ty| native_type(&context, program, *ty).map(Into::into))
            .collect::<NativeResult<Vec<_>>>()?;
        let ty = match source.result {
            Type::Unit => context.void_type().fn_type(&params, false),
            ty => native_type(&context, program, ty)?.fn_type(&params, false),
        };
        let linkage = if library && program.exports.contains(&id) {
            Linkage::External
        } else {
            Linkage::Internal
        };
        functions[id] = Some(module.add_function(&format!("loom.fn.{id}"), ty, Some(linkage)));
    }
    for &id in &reachable {
        let function = functions[id].ok_or("missing checked function")?;
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);
        let source = &program.functions[id];
        let locals = source
            .locals
            .iter()
            .map(|ty| {
                if *ty == Type::Unit {
                    Ok(None)
                } else {
                    Ok(Some(builder.build_alloca(
                        native_type(&context, program, *ty)?,
                        "local",
                    )?))
                }
            })
            .collect::<NativeResult<Vec<_>>>()?;
        for (index, value) in function.get_param_iter().enumerate() {
            builder.build_store(locals[index].ok_or("invalid checked parameter")?, value)?;
        }
        let size_type = context.ptr_sized_int_type(&machine.get_target_data(), None);
        let roots = if allocating.contains(&id) {
            gc::root_function(
                &context,
                &module,
                &builder,
                program,
                source,
                &locals,
                size_type,
                &mut tracers,
            )?
        } else {
            gc::RootFrame::empty()
        };
        let mut emitter = FunctionEmitter {
            context: &context,
            module: &module,
            builder: &builder,
            functions: &functions,
            function,
            locals,
            program,
            size_type,
            roots,
            tracers: &mut tracers,
        };
        for requirement in &source.requires {
            let condition = emitter
                .expr(requirement)?
                .ok_or("invalid checked precondition")?;
            emitter.guard(condition.into_int_value(), "precondition failed")?;
        }
        let result = emitter.block(&source.body)?;
        if emitter.live() {
            emitter.leave_roots()?;
            match source.result {
                Type::Unit => {
                    builder.build_return(None)?;
                }
                _ => {
                    builder.build_return(Some(&result.ok_or("missing checked return value")?))?;
                }
            }
        }
    }
    if !library {
        let main = module.add_function("main", context.i32_type().fn_type(&[], false), None);
        builder.position_at_end(context.append_basic_block(main, "entry"));
        for root in roots {
            let function = &program.functions[root];
            if !function.params.is_empty() || function.result != Type::Unit {
                return Err("entry points must take no arguments and return no value".into());
            }
            builder.build_call(functions[root].ok_or("missing entry point")?, &[], "")?;
        }
        builder.build_return(Some(&context.i32_type().const_zero()))?;
    }
    module.verify().map_err(|error| error.to_string())?;
    module
        .run_passes("default<O2>", &machine, PassBuilderOptions::create())
        .map_err(|error| error.to_string())?;
    module.verify().map_err(|error| error.to_string())?;
    if let Some(path) = llvm_ir {
        module
            .print_to_file(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    machine
        .write_to_file(&module, FileType::Object, object)
        .map_err(|error| format!("{}: {error}", object.display()))?;
    Ok(module
        .get_functions()
        .any(|function| function.get_name().to_bytes().starts_with(b"loom_rt_")))
}

fn native_type<'ctx>(
    context: &'ctx Context,
    program: &checked::Program,
    ty: Type,
) -> NativeResult<BasicTypeEnum<'ctx>> {
    Ok(match ty {
        Type::Bool => context.bool_type().into(),
        Type::Int => context.i64_type().into(),
        Type::Text | Type::Bytes | Type::List(_) => {
            context.ptr_type(AddressSpace::default()).into()
        }
        Type::Unit => context.struct_type(&[], false).into(),
        Type::Parameter(_) => return Err("unbound type parameter reached native emission".into()),
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Refined(base) => native_type(context, program, *base)?,
            checked::DataKind::Record(fields) => {
                let fields = fields
                    .iter()
                    .map(|(_, ty)| native_type(context, program, *ty))
                    .collect::<NativeResult<Vec<_>>>()?;
                context.struct_type(&fields, false).into()
            }
            checked::DataKind::Enum(variants) => {
                let words = payload_words(program, variants)?;
                let count =
                    u32::try_from(words).map_err(|_| "enum payload exceeds LLVM array limit")?;
                context
                    .struct_type(
                        &[
                            context.i64_type().into(),
                            context.i64_type().array_type(count).into(),
                        ],
                        false,
                    )
                    .into()
            }
        },
    })
}

/// Enum payloads use the largest variant, never the sum of variant layouts.
/// Scalar leaves are normalized to words; constructors zero unused words.
fn payload_words(
    program: &checked::Program,
    variants: &[(String, Vec<Type>)],
) -> NativeResult<usize> {
    variants
        .iter()
        .map(|(_, fields)| {
            fields.iter().try_fold(0usize, |total, ty| {
                total
                    .checked_add(value_words(program, *ty)?)
                    .ok_or_else(|| "enum layout is too large".into())
            })
        })
        .try_fold(0, |largest, count| count.map(|count| largest.max(count)))
}

fn value_words(program: &checked::Program, ty: Type) -> NativeResult<usize> {
    match ty {
        Type::Int | Type::Bool | Type::Text | Type::Bytes | Type::List(_) => Ok(1),
        Type::Unit => Ok(0),
        Type::Parameter(_) => Err("unbound type parameter reached native layout".into()),
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Refined(base) => value_words(program, *base),
            checked::DataKind::Record(fields) => {
                fields.iter().try_fold(0usize, |total, (_, ty)| {
                    total
                        .checked_add(value_words(program, *ty)?)
                        .ok_or_else(|| "record layout is too large".into())
                })
            }
            checked::DataKind::Enum(variants) => payload_words(program, variants)?
                .checked_add(1)
                .ok_or_else(|| "enum layout is too large".into()),
        },
    }
}

struct FunctionEmitter<'a, 'ctx> {
    context: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    functions: &'a [Option<FunctionValue<'ctx>>],
    function: FunctionValue<'ctx>,
    locals: Vec<Option<PointerValue<'ctx>>>,
    program: &'a checked::Program,
    size_type: IntType<'ctx>,
    roots: gc::RootFrame<'ctx>,
    tracers: &'a mut HashMap<Type, FunctionValue<'ctx>>,
}

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    fn runtime_call(
        &self,
        name: &str,
        result: Option<BasicTypeEnum<'ctx>>,
        args: &[BasicValueEnum<'ctx>],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let function = gc::runtime_function(
            self.context,
            self.module,
            name,
            result,
            &args
                .iter()
                .map(|value| value.get_type())
                .collect::<Vec<_>>(),
        );
        Ok(self
            .builder
            .build_call(
                function,
                &args.iter().map(|value| (*value).into()).collect::<Vec<_>>(),
                if result.is_some() { "runtime" } else { "" },
            )?
            .try_as_basic_value()
            .basic())
    }

    fn leave_roots(&self) -> NativeResult<()> {
        if let Some(checkpoint) = self.roots.checkpoint {
            self.runtime_call("roots_leave", None, &[checkpoint.into()])?;
        }
        Ok(())
    }

    fn primitive(
        &mut self,
        result: Type,
        operation: Primitive,
        args: &[checked::Expr],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let mut values = Vec::new();
        for arg in args {
            let Some(value) = self.expr(arg)? else {
                return Ok(None);
            };
            values.push(value);
        }
        let pointer = self.context.ptr_type(AddressSpace::default());
        let i64_type = self.context.i64_type();
        let (name, result_type) = match operation {
            Primitive::TextLen => ("text_len", Some(i64_type.into())),
            Primitive::TextByte => ("text_byte", Some(i64_type.into())),
            Primitive::TextConcat => ("text_concat", Some(pointer.into())),
            Primitive::TextEqual => ("text_equal", Some(self.context.i32_type().into())),
            Primitive::BytesNew => ("bytes_new", Some(pointer.into())),
            Primitive::BytesLen => ("bytes_len", Some(i64_type.into())),
            Primitive::BytesPush => ("bytes_push", None),
            Primitive::BytesUtf8 => ("bytes_utf8", Some(self.context.i32_type().into())),
            Primitive::BytesTextCopy => ("bytes_text_copy", Some(pointer.into())),
            Primitive::ListNew => {
                let Type::List(id) = result else {
                    return Err("invalid list constructor type".into());
                };
                let element = self.program.lists[id];
                let ty = native_type(self.context, self.program, element)?;
                let stride = ty.size_of().ok_or("unsized list element")?;
                let trace = if gc::managed(self.program, element) {
                    gc::tracer(
                        self.context,
                        self.module,
                        self.program,
                        element,
                        self.tracers,
                    )?
                    .as_global_value()
                    .as_pointer_value()
                } else {
                    pointer.const_null()
                };
                values = vec![
                    self.builder
                        .build_int_cast(stride, self.size_type, "element.stride")?
                        .into(),
                    trace.into(),
                ];
                ("list_new", Some(pointer.into()))
            }
            Primitive::ListLen => ("list_len", Some(i64_type.into())),
            Primitive::ListGet | Primitive::ListSet => {
                let slot = self
                    .runtime_call("list_get", Some(pointer.into()), &values[..2])?
                    .ok_or("missing list element slot")?
                    .into_pointer_value();
                if operation == Primitive::ListGet {
                    return Ok(Some(self.builder.build_load(
                        native_type(self.context, self.program, result)?,
                        slot,
                        "list.element",
                    )?));
                }
                self.builder.build_store(slot, values[2])?;
                return Ok(None);
            }
            Primitive::ListPush => {
                // Runtime growth may collect, so the source value remains in
                // its expression root while this ABI copy slot is consumed.
                let entry = self
                    .function
                    .get_first_basic_block()
                    .ok_or("missing entry")?;
                let allocas = self.context.create_builder();
                if let Some(first) = entry.get_first_instruction() {
                    allocas.position_before(&first);
                } else {
                    allocas.position_at_end(entry);
                }
                let item = allocas.build_alloca(values[1].get_type(), "list.item")?;
                self.builder.build_store(item, values[1])?;
                values[1] = item.into();
                ("list_push", None)
            }
            Primitive::Open => ("file_open", Some(i64_type.into())),
            Primitive::Create => ("file_create", Some(i64_type.into())),
            Primitive::Read => ("file_read", Some(i64_type.into())),
            Primitive::Write => ("file_write", Some(i64_type.into())),
            Primitive::Close => ("file_close", Some(i64_type.into())),
        };
        let value = self.runtime_call(name, result_type, &values)?;
        if result == Type::Bool {
            return Ok(Some(
                self.builder
                    .build_int_compare(
                        IntPredicate::NE,
                        value.ok_or("missing runtime Boolean")?.into_int_value(),
                        self.context.i32_type().const_zero(),
                        "runtime.bool",
                    )?
                    .into(),
            ));
        }
        Ok(value)
    }

    fn live(&self) -> bool {
        self.builder
            .get_insert_block()
            .is_some_and(|block| block.get_terminator().is_none())
    }

    fn block(&mut self, block: &checked::Block) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        for statement in &block.statements {
            if !self.live() {
                break;
            }
            match &statement.kind {
                checked::StmtKind::Let { local, value }
                | checked::StmtKind::Assign { local, value } => {
                    if let Some(value) = self.expr(value)? {
                        self.builder.build_store(
                            self.locals[*local].ok_or("invalid checked local")?,
                            value,
                        )?;
                    }
                }
                checked::StmtKind::Return(value) => {
                    let value = match value {
                        Some(value) => self.expr(value)?,
                        None => None,
                    };
                    if self.live() {
                        self.leave_roots()?;
                        match value {
                            Some(value) => {
                                self.builder.build_return(Some(&value))?;
                            }
                            None => {
                                self.builder.build_return(None)?;
                            }
                        }
                    }
                }
                checked::StmtKind::Assert(value) => {
                    if let Some(value) = self.expr(value)? {
                        self.guard(value.into_int_value(), "assertion failed")?;
                    }
                }
                checked::StmtKind::Discard(value) | checked::StmtKind::Expr(value) => {
                    self.expr(value)?;
                }
                checked::StmtKind::While { condition, body } => {
                    let test = self.context.append_basic_block(self.function, "while.test");
                    let run = self.context.append_basic_block(self.function, "while.body");
                    let done = self.context.append_basic_block(self.function, "while.done");
                    self.builder.build_unconditional_branch(test)?;
                    self.builder.position_at_end(test);
                    if let Some(condition) = self.expr(condition)? {
                        self.builder.build_conditional_branch(
                            condition.into_int_value(),
                            run,
                            done,
                        )?;
                        self.builder.position_at_end(run);
                        self.block(body)?;
                        if self.live() {
                            self.builder.build_unconditional_branch(test)?;
                        }
                        self.builder.position_at_end(done);
                    } else {
                        self.builder.position_at_end(run);
                        self.builder.build_unreachable()?;
                        self.builder.position_at_end(done);
                        self.builder.build_unreachable()?;
                    }
                }
            }
        }
        if self.live()
            && let Some(tail) = &block.tail
        {
            return self.expr(tail);
        }
        Ok(None)
    }

    fn expr(&mut self, expr: &checked::Expr) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let value = self.expr_inner(expr)?;
        if let Some(value) = value
            && let Some(slot) = self.roots.expressions.get(&(expr as *const checked::Expr))
        {
            self.builder.build_store(*slot, value)?;
        }
        Ok(value)
    }

    fn expr_inner(&mut self, expr: &checked::Expr) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let value = match &expr.kind {
            checked::ExprKind::Int(value) => self
                .context
                .i64_type()
                .const_int(*value as u64, true)
                .into(),
            checked::ExprKind::Bool(value) => self
                .context
                .bool_type()
                .const_int(u64::from(*value), false)
                .into(),
            checked::ExprKind::Text(text) => {
                let bytes = self.context.const_string(text.as_bytes(), false);
                let value = self.context.const_struct(
                    &[
                        self.size_type.const_int(text.len() as u64, false).into(),
                        bytes.into(),
                    ],
                    false,
                );
                let global = self
                    .module
                    .add_global(value.get_type(), None, "text.literal");
                global.set_initializer(&value);
                global.set_constant(true);
                global.set_linkage(Linkage::Private);
                global.as_pointer_value().into()
            }
            checked::ExprKind::Primitive(operation, args) => {
                return self.primitive(expr.ty, *operation, args);
            }
            checked::ExprKind::Local(local) => {
                let Some(pointer) = self.locals[*local] else {
                    return Ok(None);
                };
                self.builder.build_load(
                    native_type(self.context, self.program, expr.ty)?,
                    pointer,
                    "load",
                )?
            }
            checked::ExprKind::Unary(op, value) => {
                let Some(value) = self.expr(value)? else {
                    return Ok(None);
                };
                let value = value.into_int_value();
                match op {
                    Unary::Not => self.builder.build_not(value, "not")?,
                    Unary::Neg => self.overflow(
                        "llvm.ssub.with.overflow",
                        self.context.i64_type().const_zero(),
                        value,
                    )?,
                }
                .into()
            }
            checked::ExprKind::Binary(op, left, right) => {
                let operand_type = left.ty;
                let Some(left) = self.expr(left)? else {
                    return Ok(None);
                };
                if matches!(op, Binary::And | Binary::Or) {
                    return self.short_circuit(*op, left.into_int_value(), right);
                }
                let Some(right) = self.expr(right)? else {
                    return Ok(None);
                };
                if matches!(op, Binary::Eq | Binary::Ne) {
                    let equal = self.equal(operand_type, left, right)?;
                    if *op == Binary::Ne {
                        self.builder.build_not(equal, "not.equal")?.into()
                    } else {
                        equal.into()
                    }
                } else {
                    self.binary(*op, left.into_int_value(), right.into_int_value())?
                        .into()
                }
            }
            checked::ExprKind::Call(function, args) => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    let Some(value) = self.expr(arg)? else {
                        return Ok(None);
                    };
                    values.push(value.into());
                }
                let call = self.builder.build_call(
                    self.functions[*function].ok_or("unresolved checked call")?,
                    &values,
                    if expr.ty == Type::Unit { "" } else { "call" },
                )?;
                return Ok(call.try_as_basic_value().basic());
            }
            checked::ExprKind::Record(fields) => {
                let mut result = native_type(self.context, self.program, expr.ty)?
                    .into_struct_type()
                    .const_zero();
                // The checked field indices preserve source evaluation order even
                // when construction names fields in a different declaration order.
                for (index, field) in fields {
                    let Some(value) = self.expr(field)? else {
                        return Ok(None);
                    };
                    result = self
                        .builder
                        .build_insert_value(result, value, *index as u32, "record.field")?
                        .into_struct_value();
                }
                result.into()
            }
            checked::ExprKind::Field(value, index) => {
                let Some(value) = self.expr(value)? else {
                    return Ok(None);
                };
                self.builder.build_extract_value(
                    value.into_struct_value(),
                    *index as u32,
                    "field",
                )?
            }
            checked::ExprKind::Variant { variant, fields } => {
                let ty = native_type(self.context, self.program, expr.ty)?.into_struct_type();
                let mut result = self
                    .builder
                    .build_insert_value(
                        ty.const_zero(),
                        self.context.i64_type().const_int(*variant as u64, false),
                        0,
                        "enum.tag",
                    )?
                    .into_struct_value();
                let mut payload = self
                    .builder
                    .build_extract_value(result, 1, "enum.payload")?
                    .into_array_value();
                let mut words = Vec::new();
                for field in fields {
                    let Some(value) = self.expr(field)? else {
                        return Ok(None);
                    };
                    self.flatten(field.ty, value, &mut words)?;
                }
                for (index, word) in words.into_iter().enumerate() {
                    payload = self
                        .builder
                        .build_insert_value(payload, word, index as u32, "payload.word")?
                        .into_array_value();
                }
                result = self
                    .builder
                    .build_insert_value(result, payload, 1, "enum.value")?
                    .into_struct_value();
                result.into()
            }
            checked::ExprKind::Match { value, arms } => {
                return self.match_expr(expr.ty, value, arms);
            }
            checked::ExprKind::Block(body) => return self.block(body),
            checked::ExprKind::Coerce(value) => return self.expr(value),
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                let Some(condition) = self.expr(condition)? else {
                    return Ok(None);
                };
                let then_block = self.context.append_basic_block(self.function, "if.then");
                let else_block = self.context.append_basic_block(self.function, "if.else");
                let done = self.context.append_basic_block(self.function, "if.done");
                self.builder.build_conditional_branch(
                    condition.into_int_value(),
                    then_block,
                    else_block,
                )?;
                let mut incoming = Vec::new();
                let mut live_arms = 0;
                for (start, body) in [
                    (then_block, Some(then_body)),
                    (else_block, else_body.as_ref()),
                ] {
                    self.builder.position_at_end(start);
                    let value = match body {
                        Some(body) => self.block(body)?,
                        None => None,
                    };
                    if self.live() {
                        live_arms += 1;
                        let end = self
                            .builder
                            .get_insert_block()
                            .ok_or("missing conditional block")?;
                        if let Some(value) = value {
                            incoming.push((value, end));
                        }
                        self.builder.build_unconditional_branch(done)?;
                    }
                }
                return self.join(expr.ty, done, live_arms, &incoming);
            }
        };
        Ok(Some(value))
    }

    fn join(
        &self,
        ty: Type,
        done: BasicBlock<'ctx>,
        live_arms: usize,
        incoming: &[(BasicValueEnum<'ctx>, BasicBlock<'ctx>)],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        self.builder.position_at_end(done);
        if live_arms == 0 {
            self.builder.build_unreachable()?;
            return Ok(None);
        }
        if ty == Type::Unit {
            return Ok(None);
        }
        if incoming.len() != live_arms {
            return Err("missing checked branch value".into());
        }
        let phi = self
            .builder
            .build_phi(native_type(self.context, self.program, ty)?, "branch.value")?;
        for (value, predecessor) in incoming {
            phi.add_incoming(&[(value, *predecessor)]);
        }
        Ok(Some(phi.as_basic_value()))
    }

    fn match_expr(
        &mut self,
        result_type: Type,
        value: &checked::Expr,
        arms: &[checked::MatchArm],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let Some(scrutinee) = self.expr(value)? else {
            return Ok(None);
        };
        let blocks = arms
            .iter()
            .map(|_| self.context.append_basic_block(self.function, "match.arm"))
            .collect::<Vec<_>>();
        let done = self.context.append_basic_block(self.function, "match.done");
        let fallback = arms.iter().position(|arm| arm.variant.is_none());
        if arms.iter().any(|arm| arm.variant.is_some()) {
            let tag = self
                .builder
                .build_extract_value(scrutinee.into_struct_value(), 0, "match.tag")?
                .into_int_value();
            let default = fallback.map(|index| blocks[index]).unwrap_or_else(|| {
                self.context
                    .append_basic_block(self.function, "match.unreachable")
            });
            let cases = arms
                .iter()
                .zip(&blocks)
                .filter_map(|(arm, block)| {
                    arm.variant.map(|variant| {
                        (
                            self.context.i64_type().const_int(variant as u64, false),
                            *block,
                        )
                    })
                })
                .collect::<Vec<_>>();
            self.builder.build_switch(tag, default, &cases)?;
            if fallback.is_none() {
                self.builder.position_at_end(default);
                self.builder.build_unreachable()?;
            }
        } else {
            self.builder
                .build_unconditional_branch(blocks[fallback.ok_or("empty checked match")?])?;
        }
        let mut live_arms = 0;
        let mut incoming = Vec::new();
        for (arm, start) in arms.iter().zip(blocks) {
            self.builder.position_at_end(start);
            if let Some(local) = arm.whole {
                self.builder.build_store(
                    self.locals[local].ok_or("invalid whole-pattern local")?,
                    scrutinee,
                )?;
            }
            if let Some(variant) = arm.variant {
                let Type::Data(id) = value.ty else {
                    return Err("non-enum variant pattern".into());
                };
                let checked::DataKind::Enum(variants) = &self.program.types[id].kind else {
                    return Err("non-enum variant pattern".into());
                };
                let fields = &variants[variant].1;
                let payload = self
                    .builder
                    .build_extract_value(scrutinee.into_struct_value(), 1, "match.payload")?
                    .into_array_value();
                let mut words = Vec::new();
                let count = fields
                    .iter()
                    .map(|ty| value_words(self.program, *ty))
                    .collect::<NativeResult<Vec<_>>>()?
                    .into_iter()
                    .sum::<usize>();
                for index in 0..count {
                    words.push(
                        self.builder
                            .build_extract_value(payload, index as u32, "match.word")?
                            .into_int_value(),
                    );
                }
                let mut offset = 0;
                for (ty, local) in fields.iter().zip(&arm.bindings) {
                    if let Some(local) = local {
                        let field = self.rebuild(*ty, &words, &mut offset)?;
                        self.builder.build_store(
                            self.locals[*local].ok_or("invalid pattern local")?,
                            field,
                        )?;
                    } else {
                        offset += value_words(self.program, *ty)?;
                    }
                }
            }
            let result = self.block(&arm.body)?;
            if self.live() {
                live_arms += 1;
                if let Some(result) = result {
                    incoming.push((
                        result,
                        self.builder
                            .get_insert_block()
                            .ok_or("missing match block")?,
                    ));
                }
                self.builder.build_unconditional_branch(done)?;
            }
        }
        self.join(result_type, done, live_arms, &incoming)
    }

    /// A canonical, typed payload representation avoids memory casts and does
    /// not expose padding bytes when nested records and enums are compared.
    fn flatten(
        &self,
        ty: Type,
        value: BasicValueEnum<'ctx>,
        words: &mut Vec<IntValue<'ctx>>,
    ) -> NativeResult<()> {
        match ty {
            Type::Int => words.push(value.into_int_value()),
            Type::Text | Type::Bytes | Type::List(_) => words.push(self.builder.build_ptr_to_int(
                value.into_pointer_value(),
                self.context.i64_type(),
                "pointer.word",
            )?),
            Type::Bool => words.push(self.builder.build_int_z_extend(
                value.into_int_value(),
                self.context.i64_type(),
                "bool.word",
            )?),
            Type::Unit => {}
            Type::Parameter(_) => return Err("unbound type parameter in enum payload".into()),
            Type::Data(id) => match &self.program.types[id].kind {
                checked::DataKind::Refined(base) => self.flatten(*base, value, words)?,
                checked::DataKind::Record(fields) => {
                    for (index, (_, ty)) in fields.iter().enumerate() {
                        let field = self.builder.build_extract_value(
                            value.into_struct_value(),
                            index as u32,
                            "record.word",
                        )?;
                        self.flatten(*ty, field, words)?;
                    }
                }
                checked::DataKind::Enum(_) => {
                    words.push(
                        self.builder
                            .build_extract_value(value.into_struct_value(), 0, "enum.tag")?
                            .into_int_value(),
                    );
                    let payload = self
                        .builder
                        .build_extract_value(value.into_struct_value(), 1, "enum.payload")?
                        .into_array_value();
                    for index in 0..payload.get_type().len() {
                        words.push(
                            self.builder
                                .build_extract_value(payload, index, "enum.word")?
                                .into_int_value(),
                        );
                    }
                }
            },
        }
        Ok(())
    }

    fn rebuild(
        &self,
        ty: Type,
        words: &[IntValue<'ctx>],
        offset: &mut usize,
    ) -> NativeResult<BasicValueEnum<'ctx>> {
        Ok(match ty {
            Type::Int | Type::Bool => {
                let word = words[*offset];
                *offset += 1;
                if ty == Type::Bool {
                    self.builder
                        .build_int_truncate(word, self.context.bool_type(), "word.bool")?
                        .into()
                } else {
                    word.into()
                }
            }
            Type::Text | Type::Bytes | Type::List(_) => {
                let word = words[*offset];
                *offset += 1;
                self.builder
                    .build_int_to_ptr(
                        word,
                        self.context.ptr_type(AddressSpace::default()),
                        "word.pointer",
                    )?
                    .into()
            }
            Type::Unit => self.context.struct_type(&[], false).const_zero().into(),
            Type::Parameter(_) => return Err("unbound type parameter in enum payload".into()),
            Type::Data(id) => {
                if let checked::DataKind::Refined(base) = &self.program.types[id].kind {
                    return self.rebuild(*base, words, offset);
                }
                let mut result = native_type(self.context, self.program, ty)?
                    .into_struct_type()
                    .const_zero();
                match &self.program.types[id].kind {
                    checked::DataKind::Refined(_) => {
                        unreachable!("refined layouts use their base representation")
                    }
                    checked::DataKind::Record(fields) => {
                        for (index, (_, ty)) in fields.iter().enumerate() {
                            result = self
                                .builder
                                .build_insert_value(
                                    result,
                                    self.rebuild(*ty, words, offset)?,
                                    index as u32,
                                    "payload.record",
                                )?
                                .into_struct_value();
                        }
                    }
                    checked::DataKind::Enum(_) => {
                        result = self
                            .builder
                            .build_insert_value(result, words[*offset], 0, "payload.tag")?
                            .into_struct_value();
                        *offset += 1;
                        let mut payload = self
                            .builder
                            .build_extract_value(result, 1, "payload")?
                            .into_array_value();
                        for index in 0..payload.get_type().len() {
                            payload = self
                                .builder
                                .build_insert_value(payload, words[*offset], index, "payload.word")?
                                .into_array_value();
                            *offset += 1;
                        }
                        result = self
                            .builder
                            .build_insert_value(result, payload, 1, "payload.enum")?
                            .into_struct_value();
                    }
                }
                result.into()
            }
        })
    }

    fn equal(
        &self,
        ty: Type,
        left: BasicValueEnum<'ctx>,
        right: BasicValueEnum<'ctx>,
    ) -> NativeResult<IntValue<'ctx>> {
        if ty == Type::Text {
            let value = self
                .runtime_call(
                    "text_equal",
                    Some(self.context.i32_type().into()),
                    &[left, right],
                )?
                .ok_or("missing Text comparison result")?
                .into_int_value();
            return Ok(self.builder.build_int_compare(
                IntPredicate::NE,
                value,
                self.context.i32_type().const_zero(),
                "text.equal",
            )?);
        }
        if gc::managed(self.program, ty) {
            return Err("equality is not yet supported for managed aggregate values".into());
        }
        let mut left_words = Vec::new();
        let mut right_words = Vec::new();
        self.flatten(ty, left, &mut left_words)?;
        self.flatten(ty, right, &mut right_words)?;
        let mut equal = self.context.bool_type().const_int(1, false);
        for (left, right) in left_words.into_iter().zip(right_words) {
            let word_equal =
                self.builder
                    .build_int_compare(IntPredicate::EQ, left, right, "word.equal")?;
            equal = self.builder.build_and(equal, word_equal, "value.equal")?;
        }
        Ok(equal)
    }

    fn short_circuit(
        &mut self,
        op: Binary,
        left: IntValue<'ctx>,
        right: &checked::Expr,
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let origin = self
            .builder
            .get_insert_block()
            .ok_or("missing logical block")?;
        let rhs = self.context.append_basic_block(self.function, "logic.rhs");
        let done = self.context.append_basic_block(self.function, "logic.done");
        let (yes, no) = if op == Binary::And {
            (rhs, done)
        } else {
            (done, rhs)
        };
        self.builder.build_conditional_branch(left, yes, no)?;
        self.builder.position_at_end(rhs);
        let right = self.expr(right)?;
        let right_end = self
            .builder
            .get_insert_block()
            .ok_or("missing logical right block")?;
        if self.live() {
            self.builder.build_unconditional_branch(done)?;
        }
        self.builder.position_at_end(done);
        let phi = self
            .builder
            .build_phi(self.context.bool_type(), "logic.value")?;
        phi.add_incoming(&[(&left, origin)]);
        if let Some(right) = right {
            phi.add_incoming(&[(&right, right_end)]);
        }
        Ok(Some(phi.as_basic_value()))
    }

    fn binary(
        &mut self,
        op: Binary,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
    ) -> NativeResult<IntValue<'ctx>> {
        let predicate = match op {
            Binary::Eq => Some(IntPredicate::EQ),
            Binary::Ne => Some(IntPredicate::NE),
            Binary::Lt => Some(IntPredicate::SLT),
            Binary::Le => Some(IntPredicate::SLE),
            Binary::Gt => Some(IntPredicate::SGT),
            Binary::Ge => Some(IntPredicate::SGE),
            _ => None,
        };
        if let Some(predicate) = predicate {
            return Ok(self
                .builder
                .build_int_compare(predicate, left, right, "compare")?);
        }
        Ok(match op {
            Binary::Add => self.overflow("llvm.sadd.with.overflow", left, right)?,
            Binary::Sub => self.overflow("llvm.ssub.with.overflow", left, right)?,
            Binary::Mul => self.overflow("llvm.smul.with.overflow", left, right)?,
            Binary::Div | Binary::Rem => {
                let ty = self.context.i64_type();
                let nonzero = self.builder.build_int_compare(
                    IntPredicate::NE,
                    right,
                    ty.const_zero(),
                    "nonzero",
                )?;
                self.guard(nonzero, "division by zero")?;
                let minimum = self.builder.build_int_compare(
                    IntPredicate::EQ,
                    left,
                    ty.const_int(i64::MIN as u64, true),
                    "minimum",
                )?;
                let minus_one = self.builder.build_int_compare(
                    IntPredicate::EQ,
                    right,
                    ty.const_all_ones(),
                    "minus.one",
                )?;
                let overflow = self
                    .builder
                    .build_and(minimum, minus_one, "division.overflow")?;
                let safe = self.builder.build_not(overflow, "division.safe")?;
                self.guard(safe, "integer overflow")?;
                if op == Binary::Div {
                    self.builder.build_int_signed_div(left, right, "quotient")?
                } else {
                    self.builder
                        .build_int_signed_rem(left, right, "remainder")?
                }
            }
            _ => return Err("invalid checked arithmetic operation".into()),
        })
    }

    fn overflow(
        &mut self,
        name: &str,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
    ) -> NativeResult<IntValue<'ctx>> {
        let function = Intrinsic::find(name)
            .and_then(|intrinsic| {
                intrinsic.get_declaration(self.module, &[self.context.i64_type().into()])
            })
            .ok_or_else(|| format!("LLVM intrinsic is unavailable: {name}"))?;
        let result = self
            .builder
            .build_call(function, &[left.into(), right.into()], "checked")?
            .try_as_basic_value()
            .basic()
            .ok_or("invalid intrinsic result")?
            .into_struct_value();
        let value = self
            .builder
            .build_extract_value(result, 0, "value")?
            .into_int_value();
        let overflow = self
            .builder
            .build_extract_value(result, 1, "overflow")?
            .into_int_value();
        let safe = self.builder.build_not(overflow, "safe")?;
        self.guard(safe, "integer overflow")?;
        Ok(value)
    }

    fn guard(&self, valid: IntValue<'ctx>, message: &str) -> NativeResult<()> {
        let good = self.context.append_basic_block(self.function, "guard.ok");
        let bad = self
            .context
            .append_basic_block(self.function, "guard.fault");
        self.builder.build_conditional_branch(valid, good, bad)?;
        self.builder.position_at_end(bad);
        // macOS/Linux C ABI. The host links libc; Loom needs no native support library.
        let i32_type = self.context.i32_type();
        let size_type = self.size_type;
        let pointer = self.context.ptr_type(AddressSpace::default());
        let write = self.module.get_function("write").unwrap_or_else(|| {
            self.module.add_function(
                "write",
                size_type.fn_type(&[i32_type.into(), pointer.into(), size_type.into()], false),
                None,
            )
        });
        let exit = self.module.get_function("exit").unwrap_or_else(|| {
            self.module.add_function(
                "exit",
                self.context.void_type().fn_type(&[i32_type.into()], false),
                None,
            )
        });
        let text = format!("loom: {message}\n");
        let bytes = self
            .builder
            .build_global_string_ptr(&text, "fault.message")?;
        self.builder.build_call(
            write,
            &[
                i32_type.const_int(2, false).into(),
                bytes.as_pointer_value().into(),
                size_type.const_int(text.len() as u64, false).into(),
            ],
            "",
        )?;
        self.builder
            .build_call(exit, &[i32_type.const_int(1, false).into()], "")?;
        self.builder.build_unreachable()?;
        self.builder.position_at_end(good);
        Ok(())
    }
}

fn reachable_functions(
    program: &checked::Program,
    roots: &[usize],
) -> NativeResult<BTreeSet<usize>> {
    fn expr(value: &checked::Expr, calls: &mut Vec<usize>) {
        match &value.kind {
            checked::ExprKind::Unary(_, value)
            | checked::ExprKind::Field(value, _)
            | checked::ExprKind::Coerce(value) => expr(value, calls),
            checked::ExprKind::Binary(_, left, right) => {
                expr(left, calls);
                expr(right, calls);
            }
            checked::ExprKind::Call(function, args) => {
                calls.push(*function);
                for arg in args {
                    expr(arg, calls);
                }
            }
            checked::ExprKind::Primitive(_, args) => {
                for arg in args {
                    expr(arg, calls);
                }
            }
            checked::ExprKind::Record(fields) => {
                for (_, field) in fields {
                    expr(field, calls);
                }
            }
            checked::ExprKind::Variant { fields, .. } => {
                for field in fields {
                    expr(field, calls);
                }
            }
            checked::ExprKind::Match { value, arms } => {
                expr(value, calls);
                for arm in arms {
                    block(&arm.body, calls);
                }
            }
            checked::ExprKind::Block(body) => block(body, calls),
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                expr(condition, calls);
                block(then_body, calls);
                if let Some(body) = else_body {
                    block(body, calls);
                }
            }
            _ => {}
        }
    }
    fn block(value: &checked::Block, calls: &mut Vec<usize>) {
        for statement in &value.statements {
            match &statement.kind {
                checked::StmtKind::Let { value, .. }
                | checked::StmtKind::Assign { value, .. }
                | checked::StmtKind::Assert(value)
                | checked::StmtKind::Discard(value)
                | checked::StmtKind::Expr(value)
                | checked::StmtKind::Return(Some(value)) => expr(value, calls),
                checked::StmtKind::While { condition, body } => {
                    expr(condition, calls);
                    block(body, calls);
                }
                checked::StmtKind::Return(None) => {}
            }
        }
        if let Some(tail) = &value.tail {
            expr(tail, calls);
        }
    }
    let mut reachable = BTreeSet::new();
    let mut pending = roots.to_vec();
    while let Some(id) = pending.pop() {
        if !reachable.insert(id) {
            continue;
        }
        let function = program
            .functions
            .get(id)
            .ok_or("invalid checked function ID")?;
        for requirement in &function.requires {
            expr(requirement, &mut pending);
        }
        block(&function.body, &mut pending);
    }
    Ok(reachable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_layout_uses_largest_payload_and_rejects_unbound_types() {
        let program = checked::Program {
            lists: vec![],
            types: vec![
                checked::Data {
                    name: "Pair".into(),
                    kind: checked::DataKind::Record(vec![
                        ("flag".into(), Type::Bool),
                        ("number".into(), Type::Int),
                    ]),
                },
                checked::Data {
                    name: "Choice".into(),
                    kind: checked::DataKind::Enum(vec![
                        ("Empty".into(), vec![]),
                        ("Pair".into(), vec![Type::Data(0)]),
                        ("Triple".into(), vec![Type::Int, Type::Int, Type::Int]),
                    ]),
                },
                checked::Data {
                    name: "Positive".into(),
                    kind: checked::DataKind::Refined(Type::Int),
                },
            ],
            functions: vec![],
            entry: None,
            tests: vec![],
            exports: vec![],
        };
        let context = Context::create();
        let choice = native_type(&context, &program, Type::Data(1))
            .unwrap()
            .into_struct_type();
        assert_eq!(choice.count_fields(), 2);
        assert_eq!(
            choice
                .get_field_type_at_index(1)
                .unwrap()
                .into_array_type()
                .len(),
            3
        );
        assert_eq!(value_words(&program, Type::Data(1)).unwrap(), 4);
        assert_eq!(
            native_type(&context, &program, Type::Data(2)).unwrap(),
            context.i64_type().into()
        );
        assert_eq!(value_words(&program, Type::Data(2)).unwrap(), 1);
        assert!(!gc::managed(&program, Type::Data(2)));
        assert!(native_type(&context, &program, Type::Parameter(0)).is_err());
    }
}
