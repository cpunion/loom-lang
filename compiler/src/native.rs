//! Direct native lowering for the checked seed language. No universal values or executor.

use crate::model::{Binary, Type, Unary, checked};
use inkwell::{
    AddressSpace, IntPredicate, OptimizationLevel,
    builder::Builder,
    context::Context,
    intrinsics::Intrinsic,
    module::{Linkage, Module},
    passes::PassBuilderOptions,
    targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine},
    types::IntType,
    values::{FunctionValue, IntValue, PointerValue},
};
use std::{collections::BTreeSet, path::Path};

type NativeResult<T> = Result<T, Box<dyn std::error::Error>>;

pub fn emit(
    program: &checked::Program,
    test_mode: bool,
    object: &Path,
    llvm_ir: Option<&Path>,
) -> Result<(), String> {
    emit_checked(program, test_mode, object, llvm_ir).map_err(|error| error.to_string())
}

fn emit_checked(
    program: &checked::Program,
    test_mode: bool,
    object: &Path,
    llvm_ir: Option<&Path>,
) -> NativeResult<()> {
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
    let mut functions = vec![None; program.functions.len()];
    for &id in &reachable {
        let source = &program.functions[id];
        let params = source
            .params
            .iter()
            .map(|ty| scalar_type(&context, *ty).into())
            .collect::<Vec<_>>();
        let ty = match source.result {
            Type::Unit => context.void_type().fn_type(&params, false),
            ty => scalar_type(&context, ty).fn_type(&params, false),
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
                    builder
                        .build_alloca(scalar_type(&context, *ty), "local")
                        .map(Some)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (index, value) in function.get_param_iter().enumerate() {
            builder.build_store(locals[index].ok_or("invalid checked parameter")?, value)?;
        }
        let mut emitter = FunctionEmitter {
            context: &context,
            module: &module,
            builder: &builder,
            functions: &functions,
            function,
            locals,
            size_type: context.ptr_sized_int_type(&machine.get_target_data(), None),
        };
        for requirement in &source.requires {
            let condition = emitter
                .expr(requirement)?
                .ok_or("invalid checked precondition")?;
            emitter.guard(condition, "precondition failed")?;
        }
        let result = emitter.block(&source.body)?;
        if emitter.live() {
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
    Ok(())
}

fn scalar_type(context: &Context, ty: Type) -> IntType<'_> {
    match ty {
        Type::Bool => context.bool_type(),
        Type::Int | Type::Unit => context.i64_type(),
    }
}

struct FunctionEmitter<'a, 'ctx> {
    context: &'ctx Context,
    module: &'a Module<'ctx>,
    builder: &'a Builder<'ctx>,
    functions: &'a [Option<FunctionValue<'ctx>>],
    function: FunctionValue<'ctx>,
    locals: Vec<Option<PointerValue<'ctx>>>,
    size_type: IntType<'ctx>,
}

impl<'ctx> FunctionEmitter<'_, 'ctx> {
    fn live(&self) -> bool {
        self.builder
            .get_insert_block()
            .is_some_and(|block| block.get_terminator().is_none())
    }

    fn block(&mut self, block: &checked::Block) -> NativeResult<Option<IntValue<'ctx>>> {
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
                        self.guard(value, "assertion failed")?;
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
                        self.builder
                            .build_conditional_branch(condition, run, done)?;
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

    fn expr(&mut self, expr: &checked::Expr) -> NativeResult<Option<IntValue<'ctx>>> {
        let value = match &expr.kind {
            checked::ExprKind::Int(value) => self.context.i64_type().const_int(*value as u64, true),
            checked::ExprKind::Bool(value) => {
                self.context.bool_type().const_int(u64::from(*value), false)
            }
            checked::ExprKind::Local(local) => {
                let Some(pointer) = self.locals[*local] else {
                    return Ok(None);
                };
                self.builder
                    .build_load(scalar_type(self.context, expr.ty), pointer, "load")?
                    .into_int_value()
            }
            checked::ExprKind::Unary(op, value) => {
                let Some(value) = self.expr(value)? else {
                    return Ok(None);
                };
                match op {
                    Unary::Not => self.builder.build_not(value, "not")?,
                    Unary::Neg => self.overflow(
                        "llvm.ssub.with.overflow",
                        self.context.i64_type().const_zero(),
                        value,
                    )?,
                }
            }
            checked::ExprKind::Binary(op, left, right) => {
                let Some(left) = self.expr(left)? else {
                    return Ok(None);
                };
                if matches!(op, Binary::And | Binary::Or) {
                    return self.short_circuit(*op, left, right);
                }
                let Some(right) = self.expr(right)? else {
                    return Ok(None);
                };
                self.binary(*op, left, right)?
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
                return Ok(call
                    .try_as_basic_value()
                    .basic()
                    .map(|value| value.into_int_value()));
            }
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
                self.builder
                    .build_conditional_branch(condition, then_block, else_block)?;
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
                self.builder.position_at_end(done);
                if live_arms == 0 {
                    self.builder.build_unreachable()?;
                    return Ok(None);
                }
                if expr.ty == Type::Unit {
                    return Ok(None);
                }
                if incoming.len() != live_arms {
                    return Err("missing checked branch value".into());
                }
                let phi = self
                    .builder
                    .build_phi(scalar_type(self.context, expr.ty), "if.value")?;
                for (value, predecessor) in &incoming {
                    phi.add_incoming(&[(value, *predecessor)]);
                }
                phi.as_basic_value().into_int_value()
            }
        };
        Ok(Some(value))
    }

    fn short_circuit(
        &mut self,
        op: Binary,
        left: IntValue<'ctx>,
        right: &checked::Expr,
    ) -> NativeResult<Option<IntValue<'ctx>>> {
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
        Ok(Some(phi.as_basic_value().into_int_value()))
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
            checked::ExprKind::Unary(_, value) => expr(value, calls),
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
