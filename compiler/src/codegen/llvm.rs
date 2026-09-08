//! Direct LLVM lowering of checked Loom programs. No source-language frontend.

use super::{Backend, EmissionResult, EmitOptions, Optimization};
use crate::model::{Binary, Primitive, Type, Unary, checked};
use inkwell::{
    AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel,
    attributes::{Attribute, AttributeLoc},
    basic_block::BasicBlock,
    builder::Builder,
    context::Context,
    intrinsics::Intrinsic,
    module::{Linkage, Module},
    passes::PassBuilderOptions,
    targets::{FileType, TargetMachine},
    types::{BasicType, BasicTypeEnum, FunctionType, IntType},
    values::{BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue},
};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::Once,
};

#[path = "llvm_cleanup.rs"]
mod cleanup;
#[path = "llvm_dyn.rs"]
mod dynamic;
#[path = "llvm_gc.rs"]
mod gc;
#[path = "llvm_gc_lower.rs"]
mod gc_lower;
#[path = "llvm_memory.rs"]
mod memory;
#[path = "llvm_target.rs"]
mod native_target;
#[path = "llvm_tasks.rs"]
mod tasks;

use native_target::NativeTarget;

type NativeResult<T> = Result<T, Box<dyn std::error::Error>>;

pub struct Llvm;

impl Backend for Llvm {
    fn cache_identity(
        &self,
        optimization: Optimization,
        test_mode: bool,
    ) -> Result<Option<String>, String> {
        let target = NativeTarget::new(optimization)?;
        Ok(target.cache_identity(test_mode))
    }

    fn emit(
        &self,
        program: &checked::Program,
        options: EmitOptions<'_>,
    ) -> Result<EmissionResult, String> {
        trace_phase("configure");
        configure_codegen();
        let uses_runtime = emit_checked(
            program,
            options.test_mode,
            options.object,
            options.ir,
            options.optimization,
        )
        .map_err(|error| error.to_string())?;
        Ok(EmissionResult {
            library: !options.test_mode && program.entry.is_none(),
            uses_runtime,
        })
    }
}

fn trace_phase(phase: &str) {
    if std::env::var_os("LOOM_NATIVE_TIMINGS").is_some() {
        eprintln!("llvm phase: {phase}");
    }
}

fn configure_codegen() {
    static CONFIGURE: Once = Once::new();
    CONFIGURE.call_once(|| {
        // Bound candidate pressure analysis in large blocks. LLVM 22's default
        // of 256 regresses self-build latency; 32 retains scheduling dependencies.
        let arguments = [c"loom-native".as_ptr(), c"--misched-limit=32".as_ptr()];
        // SAFETY: fixed NUL-terminated literals live for the process; argv has
        // exactly two entries and lives through this synchronous call. Once
        // configures LLVM before any caller enters native emission. Inkwell
        // exposes this binding but has no safe wrapper for the process options.
        #[allow(unsafe_code)]
        unsafe {
            inkwell::llvm_sys::support::LLVMParseCommandLineOptions(
                2,
                arguments.as_ptr(),
                c"".as_ptr(),
            );
        }
    });
}

fn unwind_tables<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    runtime_fault: bool,
) -> NativeResult<()> {
    if !runtime_fault {
        return Ok(());
    }
    // LLVM's UWTableKind::Sync is 1. Synthesized functions inherit this module
    // default; Max (7) is LLVM's merge policy, absent from LLVM-C/Inkwell's enum.
    if module.get_flag("uwtable").is_none() {
        module.add_global_metadata(
            "llvm.module.flags",
            &context.metadata_node(&[
                context.i32_type().const_int(7, false).into(),
                context.metadata_string("uwtable").into(),
                context.i32_type().const_int(1, false).into(),
            ]),
        )?;
    }
    let attribute = context.create_enum_attribute(Attribute::get_named_enum_kind_id("uwtable"), 1);
    for function in module.get_functions() {
        if function.count_basic_blocks() != 0 {
            // Loom passes private runtime faults through to the Rust boundary;
            // this does not promise nounwind or introduce a landing pad.
            function.add_attribute(AttributeLoc::Function, attribute);
        }
    }
    Ok(())
}

fn emit_checked(
    program: &checked::Program,
    test_mode: bool,
    object: &Path,
    llvm_ir: Option<&Path>,
    optimization: Optimization,
) -> NativeResult<bool> {
    if !cfg!(any(unix, all(windows, target_env = "msvc"))) {
        return Err("native emission requires a Unix or Windows MSVC host".into());
    }
    let roots = if test_mode {
        program.tests.clone()
    } else if let Some(entry) = program.entry {
        vec![entry]
    } else {
        program.exports.clone()
    };
    let library = !test_mode && program.entry.is_none();
    trace_phase("reachability");
    let Reachable {
        functions: reachable,
        witnesses: live_witnesses,
        slots: live_slots,
    } = reachable_functions(program, &roots, library)?;
    let allocating = gc::allocating_functions(program, &reachable);
    let cleanup_plans = reachable
        .iter()
        .map(|id| cleanup::plans(&program.functions[*id]).map(|plans| (*id, plans)))
        .collect::<NativeResult<HashMap<_, _>>>()?;
    let runtime_fault = test_mode
        || library
        || cleanup_plans.values().any(|plans| !plans.is_empty())
        || tasks::present(program, &reachable);
    if !program.test_names.is_empty() && program.test_names.len() != program.tests.len() {
        return Err("checked test-name count mismatch".into());
    }
    trace_phase("target");
    let native = NativeTarget::new(optimization)?;
    let machine = &native.machine;
    let optimization = native.optimization;
    let context = Context::create();
    let module = context.create_module("loom");
    module.set_triple(&machine.get_triple());
    module.set_data_layout(&machine.get_target_data().get_data_layout());
    let builder = context.create_builder();
    let mut tracers = HashMap::new();
    let mut functions = vec![None; program.functions.len()];
    trace_phase("declarations");
    for &id in &reachable {
        let source = &program.functions[id];
        let ty = native_signature(&context, program, &source.params, source.result)?;
        let linkage = if library && program.exports.contains(&id) {
            Linkage::External
        } else {
            Linkage::Internal
        };
        functions[id] = Some(module.add_function(&format!("loom.fn.{id}"), ty, Some(linkage)));
    }
    let witnesses = dynamic::emit_witnesses(
        &context,
        &module,
        program,
        &functions,
        &live_witnesses,
        &live_slots,
    )?;
    trace_phase("lower");
    for &id in &reachable {
        let function = functions[id].ok_or("missing checked function")?;
        let entry = context.append_basic_block(function, "entry");
        builder.position_at_end(entry);
        let source = &program.functions[id];
        let plans = &cleanup_plans[&id];
        let callback_locals = plans
            .iter()
            .flat_map(|plan| &plan.locals)
            .copied()
            .collect::<BTreeSet<_>>();
        let locals = source
            .locals
            .iter()
            .enumerate()
            .map(|(id, ty)| {
                if *ty == Type::Unit || callback_locals.contains(&id) {
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
        let roots = if allocating.contains(&id)
            || plans.iter().any(|plan| {
                plan.captures
                    .iter()
                    .any(|id| gc::managed(program, source.locals[*id]))
            }) {
            gc::root_function(
                &context,
                &module,
                &builder,
                program,
                &allocating,
                source,
                None,
                &locals,
                &HashMap::new(),
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
            witnesses: &witnesses,
            function,
            locals,
            local_types: &source.locals,
            allocating: &allocating,
            program,
            size_type,
            roots,
            tracers: &mut tracers,
            loop_targets: Vec::new(),
            cleanups: HashMap::new(),
            runtime_fault,
        };
        emitter.prepare_cleanups(id, plans)?;
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
        emitter.emit_cleanups(source, plans)?;
    }
    if !library {
        let pointer = context.ptr_type(AddressSpace::default());
        let main = module.add_function(
            "main",
            context
                .i32_type()
                .fn_type(&[context.i32_type().into(), pointer.into()], false),
            None,
        );
        builder.position_at_end(context.append_basic_block(main, "entry"));
        if module.get_function("loom_rt_process_arg_count").is_some()
            || module.get_function("loom_rt_process_arg_text").is_some()
        {
            let init = gc::runtime_function(
                &context,
                &module,
                "process_init",
                None,
                &[context.i32_type().into(), pointer.into()],
            );
            builder.build_call(
                init,
                &[
                    main.get_nth_param(0).unwrap().into(),
                    main.get_nth_param(1).unwrap().into(),
                ],
                "",
            )?;
        }
        for (index, root) in roots.into_iter().enumerate() {
            let function = &program.functions[root];
            if !function.params.is_empty() || function.result != Type::Unit {
                return Err("entry points must take no arguments and return no value".into());
            }
            if test_mode {
                let name = program
                    .test_names
                    .get(index)
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&function.name);
                let name_pointer = builder.build_global_string_ptr(name, "test.name")?;
                let size_type = context.ptr_sized_int_type(&machine.get_target_data(), None);
                let enter = gc::runtime_function(
                    &context,
                    &module,
                    "test_enter",
                    None,
                    &[pointer.into(), size_type.into()],
                );
                builder.build_call(
                    enter,
                    &[
                        name_pointer.as_pointer_value().into(),
                        size_type.const_int(name.len() as u64, false).into(),
                    ],
                    "",
                )?;
            }
            builder.build_call(functions[root].ok_or("missing entry point")?, &[], "")?;
            if test_mode {
                let leave = gc::runtime_function(&context, &module, "test_leave", None, &[]);
                builder.build_call(leave, &[], "")?;
            }
        }
        builder.build_return(Some(&context.i32_type().const_zero()))?;
    }
    trace_phase("optimize");
    unwind_tables(&context, &module, runtime_fault)?;
    module.verify().map_err(|error| error.to_string())?;
    // Inline source abstractions before committing to physical root frames.
    // These opaque markers preserve managed snapshots through the early pass.
    if optimization != OptimizationLevel::None {
        module
            .run_passes(
                "function(sroa,early-cse),cgscc(inline)",
                machine,
                PassBuilderOptions::create(),
            )
            .map_err(|error| error.to_string())?;
    }
    gc_lower::lower(&context, &module, machine)?;
    let pipeline = match optimization {
        OptimizationLevel::None => "default<O0>",
        OptimizationLevel::Less => "default<O1>",
        OptimizationLevel::Default => "default<O2>",
        OptimizationLevel::Aggressive => "default<O3>",
    };
    module
        .run_passes(pipeline, machine, PassBuilderOptions::create())
        .map_err(|error| error.to_string())?;
    // Also cover surviving definitions synthesized without LLVM's default-attr
    // constructor. The module flag covers later target-generated helpers.
    unwind_tables(&context, &module, runtime_fault)?;
    module.verify().map_err(|error| error.to_string())?;
    if let Some(path) = llvm_ir {
        module
            .print_to_file(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    trace_phase("object");
    machine
        .write_to_file(&module, FileType::Object, object)
        .map_err(|error| format!("{}: {error}", object.display()))?;
    Ok(module
        .get_functions()
        .any(|function| function.get_name().to_bytes().starts_with(b"loom_rt_")))
}

fn native_signature<'ctx>(
    context: &'ctx Context,
    program: &checked::Program,
    params: &[Type],
    result: Type,
) -> NativeResult<FunctionType<'ctx>> {
    let params = params
        .iter()
        .map(|ty| native_type(context, program, *ty).map(Into::into))
        .collect::<NativeResult<Vec<_>>>()?;
    Ok(if result == Type::Unit {
        context.void_type().fn_type(&params, false)
    } else {
        native_type(context, program, result)?.fn_type(&params, false)
    })
}

fn native_type<'ctx>(
    context: &'ctx Context,
    program: &checked::Program,
    ty: Type,
) -> NativeResult<BasicTypeEnum<'ctx>> {
    Ok(match ty {
        Type::Bool => context.bool_type().into(),
        Type::Int => context.i64_type().into(),
        Type::Float => context.f64_type().into(),
        Type::Dyn(_) => {
            let pointer = context.ptr_type(AddressSpace::default());
            context
                .struct_type(&[pointer.into(), pointer.into()], false)
                .into()
        }
        Type::Text | Type::Bytes | Type::List(_) | Type::Function(_) => {
            context.ptr_type(AddressSpace::default()).into()
        }
        Type::Unit => context.struct_type(&[], false).into(),
        Type::Parameter(_) => return Err("unbound type parameter reached native emission".into()),
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Task(_) => context.i64_type().into(),
            checked::DataKind::Frame(_) => context.ptr_type(AddressSpace::default()).into(),
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
        Type::Int
        | Type::Float
        | Type::Bool
        | Type::Text
        | Type::Bytes
        | Type::List(_)
        | Type::Function(_) => Ok(1),
        Type::Dyn(_) => Ok(2),
        Type::Unit => Ok(0),
        Type::Parameter(_) => Err("unbound type parameter reached native layout".into()),
        Type::Data(id) => match &program.types[id].kind {
            checked::DataKind::Task(_) | checked::DataKind::Frame(_) => Ok(1),
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
    witnesses: &'a [Option<PointerValue<'ctx>>],
    function: FunctionValue<'ctx>,
    locals: Vec<Option<PointerValue<'ctx>>>,
    local_types: &'a [Type],
    allocating: &'a BTreeSet<usize>,
    program: &'a checked::Program,
    size_type: IntType<'ctx>,
    roots: gc::RootFrame<'ctx>,
    tracers: &'a mut HashMap<Type, FunctionValue<'ctx>>,
    /// Nearest loop body first via `last()`: (continue target, break target).
    loop_targets: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
    cleanups: HashMap<usize, cleanup::Site<'ctx>>,
    runtime_fault: bool,
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
        for slot in &self.roots.slots {
            gc_lower::end(self.context, self.module, self.builder, *slot)?;
        }
        Ok(())
    }

    fn restore_locals(&self) -> NativeResult<()> {
        if self.roots.locals.is_empty() {
            return Ok(());
        }
        // Source-local order keeps generated IR deterministic. Ordinary locals
        // remain nonescaping; LLVM removes restores not used before a write.
        for (index, ty) in self.local_types.iter().enumerate() {
            if let Some(root) = self.roots.locals.get(&index) {
                let value = self.builder.build_load(
                    native_type(self.context, self.program, *ty)?,
                    *root,
                    "gc.restored",
                )?;
                self.builder
                    .build_store(self.locals[index].ok_or("missing managed local")?, value)?;
            }
        }
        Ok(())
    }

    fn reload(
        &self,
        expression: &checked::Expr,
        value: BasicValueEnum<'ctx>,
    ) -> NativeResult<BasicValueEnum<'ctx>> {
        match self
            .roots
            .expressions
            .get(&(expression as *const checked::Expr))
        {
            Some(root) => Ok(self
                .builder
                .build_load(value.get_type(), *root, "gc.snapshot")?),
            None => Ok(value),
        }
    }

    fn operands<'source>(
        &mut self,
        expressions: impl IntoIterator<Item = &'source checked::Expr>,
    ) -> NativeResult<Option<Vec<BasicValueEnum<'ctx>>>> {
        let mut pending = Vec::new();
        for expression in expressions {
            let Some(value) = self.expr(expression)? else {
                return Ok(None);
            };
            pending.push((expression, value));
        }
        // An earlier operand may have moved while a later operand allocated.
        // Its root holds that evaluation's snapshot, not a reassigned local.
        Ok(Some(
            pending
                .into_iter()
                .map(|(expression, value)| self.reload(expression, value))
                .collect::<NativeResult<_>>()?,
        ))
    }

    fn primitive(
        &mut self,
        result: Type,
        operation: Primitive,
        args: &[checked::Expr],
    ) -> NativeResult<Option<BasicValueEnum<'ctx>>> {
        let Some(values) = self.operands(args)? else {
            return Ok(None);
        };
        let pointer = self.context.ptr_type(AddressSpace::default());
        let i64_type = self.context.i64_type();
        let (name, result_type) = match operation {
            Primitive::TaskCreate
            | Primitive::TaskAdopt
            | Primitive::TaskReturn
            | Primitive::TaskCleanupPush
            | Primitive::TaskCleanupPop
            | Primitive::TaskAwait
            | Primitive::TaskResult
            | Primitive::TaskRelease
            | Primitive::TaskRun
            | Primitive::TaskWaitTimer => {
                return self.task_primitive(result, operation, &values);
            }
            Primitive::FloatFromInt => {
                return Ok(Some(
                    self.builder
                        .build_signed_int_to_float(
                            values[0].into_int_value(),
                            self.context.f64_type(),
                            "float.from.int",
                        )?
                        .into(),
                ));
            }
            Primitive::FloatToInt => {
                // Total even outside the public std conversion's checked range:
                // ordinary fptosi would introduce LLVM poison for NaN/overflow.
                let function = Intrinsic::find("llvm.fptosi.sat")
                    .ok_or("missing saturating Float conversion intrinsic")?
                    .get_declaration(
                        self.module,
                        &[i64_type.into(), self.context.f64_type().into()],
                    )
                    .ok_or("missing saturating Float conversion declaration")?;
                return Ok(self
                    .builder
                    .build_call(function, &[values[0].into()], "float.to.int")?
                    .try_as_basic_value()
                    .basic());
            }
            Primitive::FloatParse => ("float_parse", Some(self.context.f64_type().into())),
            Primitive::FloatFormat => ("float_format", Some(pointer.into())),
            Primitive::TextLen | Primitive::BytesLen | Primitive::ListLen => {
                return Ok(Some(
                    self.memory_len(values[0].into_pointer_value())?.into(),
                ));
            }
            Primitive::TextByte => {
                return Ok(Some(
                    self.text_byte(values[0].into_pointer_value(), values[1].into_int_value())?
                        .into(),
                ));
            }
            Primitive::TextConcat => ("text_concat", Some(pointer.into())),
            Primitive::TextEqual => ("text_equal", Some(self.context.i32_type().into())),
            Primitive::TextSlice => ("text_slice", Some(pointer.into())),
            Primitive::UnicodeAlphabetic => {
                ("unicode_alphabetic", Some(self.context.i32_type().into()))
            }
            Primitive::UnicodeAlphanumeric => {
                ("unicode_alphanumeric", Some(self.context.i32_type().into()))
            }
            Primitive::UnicodeWhitespace => {
                ("unicode_whitespace", Some(self.context.i32_type().into()))
            }
            Primitive::ArgCount => ("process_arg_count", Some(i64_type.into())),
            Primitive::MonotonicNs => ("monotonic_ns", Some(i64_type.into())),
            Primitive::ArgText => ("process_arg_text", Some(pointer.into())),
            Primitive::ProcessRun => ("process_run", Some(i64_type.into())),
            Primitive::ProcessRunInput => ("process_run_input", Some(i64_type.into())),
            Primitive::ProcessCaptureConfigured => {
                ("process_capture_configured", Some(i64_type.into()))
            }
            Primitive::ProcessCaptureInputConfigured => {
                ("process_capture_input_configured", Some(i64_type.into()))
            }
            Primitive::EnvGet => ("env_get", Some(i64_type.into())),
            Primitive::Exit => ("process_exit", None),
            Primitive::BytesNew => ("bytes_new", Some(pointer.into())),
            Primitive::BytesGet | Primitive::BytesSet => {
                let slot =
                    self.bytes_slot(values[0].into_pointer_value(), values[1].into_int_value())?;
                if operation == Primitive::BytesGet {
                    let byte = self
                        .builder
                        .build_load(self.context.i8_type(), slot, "bytes.element")?
                        .into_int_value();
                    return Ok(Some(
                        self.builder
                            .build_int_z_extend(byte, i64_type, "bytes.element.int")?
                            .into(),
                    ));
                }
                let byte = self.checked_byte(values[2].into_int_value())?;
                self.builder.build_store(slot, byte)?;
                return Ok(None);
            }
            Primitive::BytesPush | Primitive::ListPush => {
                self.buffer_push(args, &values, operation == Primitive::BytesPush)?;
                return Ok(None);
            }
            Primitive::BytesUtf8 => ("bytes_utf8", Some(self.context.i32_type().into())),
            Primitive::BytesTextCopy => ("bytes_text_copy", Some(pointer.into())),
            Primitive::ListNew => {
                return Ok(Some(self.list_new(result, 0)?.into()));
            }
            Primitive::ListGet | Primitive::ListSet => {
                let element = if operation == Primitive::ListGet {
                    native_type(self.context, self.program, result)?
                } else {
                    values[2].get_type()
                };
                let slot = self.list_slot(
                    values[0].into_pointer_value(),
                    values[1].into_int_value(),
                    element,
                )?;
                if operation == Primitive::ListGet {
                    return Ok(Some(self.builder.build_load(
                        element,
                        slot,
                        "list.element",
                    )?));
                }
                self.builder.build_store(slot, values[2])?;
                return Ok(None);
            }
            Primitive::Open => ("file_open", Some(i64_type.into())),
            Primitive::Create => ("file_create", Some(i64_type.into())),
            Primitive::Read => ("file_read", Some(i64_type.into())),
            Primitive::Write => ("file_write", Some(i64_type.into())),
            Primitive::WriteBytes => ("file_write_bytes", Some(i64_type.into())),
            Primitive::Close => ("file_close", Some(i64_type.into())),
            Primitive::DirectoryRead => ("directory_read", Some(i64_type.into())),
            Primitive::PathKind => ("path_kind", Some(i64_type.into())),
            Primitive::PathCanonical => ("path_canonical", Some(i64_type.into())),
            Primitive::DirectoryCreate => ("directory_create", Some(i64_type.into())),
            Primitive::PathRename => ("path_rename", Some(i64_type.into())),
            Primitive::FileRemove => ("file_remove", Some(i64_type.into())),
            Primitive::DirectoryRemove => ("directory_remove", Some(i64_type.into())),
            Primitive::PathEntryKind => ("path_entry_kind", Some(i64_type.into())),
        };
        let value = self.runtime_call(name, result_type, &values)?;
        if gc::allocates(operation) {
            self.restore_locals()?;
        }
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

    fn store_local(&self, local: usize, value: BasicValueEnum<'ctx>) -> NativeResult<()> {
        self.builder
            .build_store(self.locals[local].ok_or("invalid checked local")?, value)?;
        if let Some(slot) = self.roots.locals.get(&local) {
            self.builder.build_store(*slot, value)?;
        }
        Ok(())
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
                        self.store_local(*local, value)?;
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
                checked::StmtKind::Assert { condition, message } => {
                    if let Some(value) = self.expr(condition)? {
                        self.guard(
                            value.into_int_value(),
                            if message.is_empty() {
                                "assertion failed"
                            } else {
                                message
                            },
                        )?;
                    }
                }
                checked::StmtKind::Discard(value) | checked::StmtKind::Expr(value) => {
                    self.expr(value)?;
                }
                checked::StmtKind::Defer { id, .. } => self.register_cleanup(*id)?,
                checked::StmtKind::Cleanup { id, .. } => self.run_cleanup(*id)?,
                checked::StmtKind::Break | checked::StmtKind::Continue => {
                    let &(test, done) = self
                        .loop_targets
                        .last()
                        .ok_or("checked loop control requires an enclosing loop body")?;
                    let target = if matches!(statement.kind, checked::StmtKind::Break) {
                        done
                    } else {
                        test
                    };
                    self.builder.build_unconditional_branch(target)?;
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
                        self.loop_targets.push((test, done));
                        self.block(body)?;
                        self.loop_targets.pop();
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
            checked::ExprKind::Float(value) => self.context.f64_type().const_float(*value).into(),
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
            checked::ExprKind::DynBox { witness, value } => return self.dyn_box(*witness, value),
            checked::ExprKind::DynCall {
                receiver,
                slot,
                arguments,
            } => {
                return self.dyn_call(receiver, *slot, arguments);
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
                let operand_type = value.ty;
                let Some(value) = self.expr(value)? else {
                    return Ok(None);
                };
                if operand_type == Type::Float {
                    if *op != Unary::Neg {
                        return Err("invalid checked Float unary operation".into());
                    }
                    return Ok(Some(
                        self.builder
                            .build_float_neg(value.into_float_value(), "float.neg")?
                            .into(),
                    ));
                }
                let value = value.into_int_value();
                match op {
                    Unary::Not | Unary::BitNot => self.builder.build_not(value, "not")?,
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
                let left_expression = left.as_ref();
                let Some(left) = self.expr(left)? else {
                    return Ok(None);
                };
                if matches!(op, Binary::And | Binary::Or) {
                    return self.short_circuit(*op, left.into_int_value(), right);
                }
                let Some(right) = self.expr(right)? else {
                    return Ok(None);
                };
                let left = self.reload(left_expression, left)?;
                if operand_type == Type::Float {
                    self.float_binary(*op, left.into_float_value(), right.into_float_value())?
                } else if matches!(op, Binary::Eq | Binary::Ne) {
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
                let Some(values) = self.operands(args)? else {
                    return Ok(None);
                };
                let call = self.builder.build_call(
                    self.functions[*function].ok_or("unresolved checked call")?,
                    &values
                        .iter()
                        .map(|value| (*value).into())
                        .collect::<Vec<_>>(),
                    if expr.ty == Type::Unit { "" } else { "call" },
                )?;
                if self.allocating.contains(function) {
                    self.restore_locals()?;
                }
                return Ok(call.try_as_basic_value().basic());
            }
            checked::ExprKind::FunctionRef(function) => self.functions[*function]
                .ok_or("unresolved checked function reference")?
                .as_global_value()
                .as_pointer_value()
                .into(),
            checked::ExprKind::IndirectCall { callee, arguments } => {
                let Type::Function(signature) = callee.ty else {
                    return Err("invalid checked callable type".into());
                };
                let Some(callee) = self.expr(callee)? else {
                    return Ok(None);
                };
                let Some(values) = self.operands(arguments)? else {
                    return Ok(None);
                };
                let signature = &self.program.function_types[signature];
                let call = self.builder.build_indirect_call(
                    native_signature(
                        self.context,
                        self.program,
                        &signature.params,
                        signature.result,
                    )?,
                    callee.into_pointer_value(),
                    &values
                        .iter()
                        .map(|value| (*value).into())
                        .collect::<Vec<_>>(),
                    if signature.result == Type::Unit {
                        ""
                    } else {
                        "indirect.call"
                    },
                )?;
                self.restore_locals()?;
                return Ok(call.try_as_basic_value().basic());
            }
            checked::ExprKind::FrameNew(fields) => return self.frame_new(expr.ty, fields),
            checked::ExprKind::FrameStore {
                frame,
                field,
                value,
            } => {
                return self.frame_store(frame, *field, value);
            }
            checked::ExprKind::Record(fields) => {
                let Some(values) = self.operands(fields.iter().map(|(_, field)| field))? else {
                    return Ok(None);
                };
                let mut result = native_type(self.context, self.program, expr.ty)?
                    .into_struct_type()
                    .const_zero();
                // The checked field indices preserve source evaluation order even
                // when construction names fields in a different declaration order.
                for ((index, _), value) in fields.iter().zip(values) {
                    result = self
                        .builder
                        .build_insert_value(result, value, *index as u32, "record.field")?
                        .into_struct_value();
                }
                result.into()
            }
            checked::ExprKind::List(elements) => return self.list_literal(expr.ty, elements),
            checked::ExprKind::Field(value, index) => {
                let frame_type = match value.ty {
                    Type::Data(id)
                        if matches!(self.program.types[id].kind, checked::DataKind::Frame(_)) =>
                    {
                        Some(value.ty)
                    }
                    _ => None,
                };
                let Some(value) = self.expr(value)? else {
                    return Ok(None);
                };
                if let Some(ty) = frame_type {
                    let address = self.builder.build_struct_gep(
                        tasks::frame_layout(self.context, self.program, ty)?,
                        value.into_pointer_value(),
                        *index as u32,
                        "frame.field",
                    )?;
                    self.builder.build_load(
                        native_type(self.context, self.program, expr.ty)?,
                        address,
                        "frame.load",
                    )?
                } else {
                    self.builder.build_extract_value(
                        value.into_struct_value(),
                        *index as u32,
                        "field",
                    )?
                }
            }
            checked::ExprKind::Variant { variant, fields } => {
                let Some(values) = self.operands(fields)? else {
                    return Ok(None);
                };
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
                for (field, value) in fields.iter().zip(values) {
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
                self.store_local(local, scrutinee)?;
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
                        self.store_local(*local, field)?;
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
            Type::Dyn(_) => {
                for index in 0..2 {
                    let pointer = self
                        .builder
                        .build_extract_value(value.into_struct_value(), index, "dyn.word")?
                        .into_pointer_value();
                    words.push(self.builder.build_ptr_to_int(
                        pointer,
                        self.context.i64_type(),
                        "pointer.word",
                    )?);
                }
            }
            Type::Int => words.push(value.into_int_value()),
            Type::Float => words.push(
                self.builder
                    .build_bit_cast(value, self.context.i64_type(), "float.word")?
                    .into_int_value(),
            ),
            Type::Text | Type::Bytes | Type::List(_) | Type::Function(_) => {
                words.push(self.builder.build_ptr_to_int(
                    value.into_pointer_value(),
                    self.context.i64_type(),
                    "pointer.word",
                )?)
            }
            Type::Bool => words.push(self.builder.build_int_z_extend(
                value.into_int_value(),
                self.context.i64_type(),
                "bool.word",
            )?),
            Type::Unit => {}
            Type::Parameter(_) => return Err("unbound type parameter in enum payload".into()),
            Type::Data(id) => match &self.program.types[id].kind {
                checked::DataKind::Task(_) => self.flatten(Type::Int, value, words)?,
                checked::DataKind::Frame(_) => self.flatten(Type::Text, value, words)?,
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
            Type::Dyn(_) => {
                let mut value = native_type(self.context, self.program, ty)?
                    .into_struct_type()
                    .const_zero();
                for index in 0..2 {
                    let pointer = self.builder.build_int_to_ptr(
                        words[*offset],
                        self.context.ptr_type(AddressSpace::default()),
                        "word.pointer",
                    )?;
                    *offset += 1;
                    value = self
                        .builder
                        .build_insert_value(value, pointer, index, "payload.dyn")?
                        .into_struct_value();
                }
                value.into()
            }
            Type::Float => {
                let word = words[*offset];
                *offset += 1;
                self.builder
                    .build_bit_cast(word, self.context.f64_type(), "word.float")?
            }
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
            Type::Text | Type::Bytes | Type::List(_) | Type::Function(_) => {
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
                match self.program.types[id].kind {
                    checked::DataKind::Task(_) => return self.rebuild(Type::Int, words, offset),
                    checked::DataKind::Frame(_) => return self.rebuild(Type::Text, words, offset),
                    _ => {}
                }
                if let checked::DataKind::Refined(base) = &self.program.types[id].kind {
                    return self.rebuild(*base, words, offset);
                }
                let mut result = native_type(self.context, self.program, ty)?
                    .into_struct_type()
                    .const_zero();
                match &self.program.types[id].kind {
                    checked::DataKind::Refined(_)
                    | checked::DataKind::Task(_)
                    | checked::DataKind::Frame(_) => {
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
        if matches!(ty, Type::Function(_)) {
            return Err("function values do not support equality".into());
        }
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

    fn float_binary(
        &self,
        op: Binary,
        left: FloatValue<'ctx>,
        right: FloatValue<'ctx>,
    ) -> NativeResult<BasicValueEnum<'ctx>> {
        let predicate = match op {
            Binary::Eq => Some(FloatPredicate::OEQ),
            Binary::Ne => Some(FloatPredicate::UNE),
            Binary::Lt => Some(FloatPredicate::OLT),
            Binary::Le => Some(FloatPredicate::OLE),
            Binary::Gt => Some(FloatPredicate::OGT),
            Binary::Ge => Some(FloatPredicate::OGE),
            _ => None,
        };
        if let Some(predicate) = predicate {
            return Ok(self
                .builder
                .build_float_compare(predicate, left, right, "float.compare")?
                .into());
        }
        Ok(match op {
            Binary::Add => self.builder.build_float_add(left, right, "float.add")?,
            Binary::Sub => self.builder.build_float_sub(left, right, "float.sub")?,
            Binary::Mul => self.builder.build_float_mul(left, right, "float.mul")?,
            Binary::Div => self.builder.build_float_div(left, right, "float.div")?,
            Binary::Rem => self.builder.build_float_rem(left, right, "float.rem")?,
            _ => return Err("invalid checked Float binary operation".into()),
        }
        .into())
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
            Binary::BitAnd => self.builder.build_and(left, right, "bit.and")?,
            Binary::BitOr => self.builder.build_or(left, right, "bit.or")?,
            Binary::BitXor => self.builder.build_xor(left, right, "bit.xor")?,
            Binary::Shl | Binary::Shr => {
                // Unsigned comparison rejects negative counts as well. Guard
                // first: an unchecked LLVM shift by >= 64 produces poison.
                let valid = self.builder.build_int_compare(
                    IntPredicate::ULT,
                    right,
                    self.context.i64_type().const_int(64, false),
                    "shift.valid",
                )?;
                self.guard(valid, "shift count must be between 0 and 63")?;
                if op == Binary::Shl {
                    self.builder.build_left_shift(left, right, "shift.left")?
                } else {
                    self.builder
                        .build_right_shift(left, right, true, "shift.right")?
                }
            }
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
        if self.runtime_fault {
            let bytes = self
                .builder
                .build_global_string_ptr(message, "fault.message")?;
            self.runtime_call(
                "fault",
                None,
                &[
                    bytes.as_pointer_value().into(),
                    self.size_type.const_int(message.len() as u64, false).into(),
                ],
            )?;
            self.builder.build_unreachable()?;
            self.builder.position_at_end(good);
            return Ok(());
        }
        // Fault-only programs use the host CRT, without the managed runtime.
        let i32_type = self.context.i32_type();
        let pointer = self.context.ptr_type(AddressSpace::default());
        let (write_name, write_result, write_count) =
            fault_write_abi(self.context, self.size_type, cfg!(windows));
        let write = self.module.get_function(write_name).unwrap_or_else(|| {
            self.module.add_function(
                write_name,
                write_result.fn_type(
                    &[i32_type.into(), pointer.into(), write_count.into()],
                    false,
                ),
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
                write_count.const_int(text.len() as u64, false).into(),
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

fn fault_write_abi<'ctx>(
    context: &'ctx Context,
    size_type: IntType<'ctx>,
    windows: bool,
) -> (&'static str, IntType<'ctx>, IntType<'ctx>) {
    if windows {
        ("_write", context.i32_type(), context.i32_type())
    } else {
        ("write", size_type, size_type)
    }
}

#[derive(Debug)]
struct Reachable {
    functions: BTreeSet<usize>,
    witnesses: BTreeSet<usize>,
    slots: BTreeSet<(Type, usize)>,
}

fn reachable_functions(
    program: &checked::Program,
    roots: &[usize],
    library: bool,
) -> NativeResult<Reachable> {
    fn expr(
        value: &checked::Expr,
        calls: &mut Vec<usize>,
        witnesses: &mut Vec<usize>,
        slots: &mut BTreeSet<(Type, usize)>,
    ) {
        match &value.kind {
            checked::ExprKind::Unary(_, value)
            | checked::ExprKind::Field(value, _)
            | checked::ExprKind::Coerce(value) => expr(value, calls, witnesses, slots),
            checked::ExprKind::DynBox { witness, value } => {
                witnesses.push(*witness);
                expr(value, calls, witnesses, slots);
            }
            checked::ExprKind::DynCall {
                receiver,
                slot,
                arguments,
            } => {
                slots.insert((receiver.ty, *slot));
                expr(receiver, calls, witnesses, slots);
                for argument in arguments {
                    expr(argument, calls, witnesses, slots);
                }
            }
            checked::ExprKind::Binary(_, left, right) => {
                expr(left, calls, witnesses, slots);
                expr(right, calls, witnesses, slots);
            }
            checked::ExprKind::Call(function, args) => {
                calls.push(*function);
                for arg in args {
                    expr(arg, calls, witnesses, slots);
                }
            }
            checked::ExprKind::FunctionRef(function) => calls.push(*function),
            checked::ExprKind::IndirectCall { callee, arguments } => {
                expr(callee, calls, witnesses, slots);
                for argument in arguments {
                    expr(argument, calls, witnesses, slots);
                }
            }
            checked::ExprKind::Primitive(_, args) | checked::ExprKind::List(args) => {
                for arg in args {
                    expr(arg, calls, witnesses, slots);
                }
            }
            checked::ExprKind::Record(fields) | checked::ExprKind::FrameNew(fields) => {
                for (_, field) in fields {
                    expr(field, calls, witnesses, slots);
                }
            }
            checked::ExprKind::FrameStore { frame, value, .. } => {
                expr(frame, calls, witnesses, slots);
                expr(value, calls, witnesses, slots);
            }
            checked::ExprKind::Variant { fields, .. } => {
                for field in fields {
                    expr(field, calls, witnesses, slots);
                }
            }
            checked::ExprKind::Match { value, arms } => {
                expr(value, calls, witnesses, slots);
                for arm in arms {
                    block(&arm.body, calls, witnesses, slots);
                }
            }
            checked::ExprKind::Block(body) => block(body, calls, witnesses, slots),
            checked::ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                expr(condition, calls, witnesses, slots);
                block(then_body, calls, witnesses, slots);
                if let Some(body) = else_body {
                    block(body, calls, witnesses, slots);
                }
            }
            _ => {}
        }
    }
    fn block(
        value: &checked::Block,
        calls: &mut Vec<usize>,
        witnesses: &mut Vec<usize>,
        slots: &mut BTreeSet<(Type, usize)>,
    ) {
        for statement in &value.statements {
            match &statement.kind {
                checked::StmtKind::Let { value, .. }
                | checked::StmtKind::Assign { value, .. }
                | checked::StmtKind::Assert {
                    condition: value, ..
                }
                | checked::StmtKind::Discard(value)
                | checked::StmtKind::Expr(value)
                | checked::StmtKind::Return(Some(value)) => expr(value, calls, witnesses, slots),
                checked::StmtKind::While { condition, body } => {
                    expr(condition, calls, witnesses, slots);
                    block(body, calls, witnesses, slots);
                }
                checked::StmtKind::Defer { body, .. } | checked::StmtKind::Cleanup { body, .. } => {
                    block(body, calls, witnesses, slots);
                }
                checked::StmtKind::Return(None)
                | checked::StmtKind::Break
                | checked::StmtKind::Continue => {}
            }
        }
        if let Some(tail) = &value.tail {
            expr(tail, calls, witnesses, slots);
        }
    }
    let mut reachable = BTreeSet::new();
    let mut pending = roots.to_vec();
    let mut witnesses = BTreeSet::new();
    let mut pending_witnesses = Vec::new();
    let mut slots = BTreeSet::new();
    loop {
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let function = program
                .functions
                .get(id)
                .ok_or("invalid checked function ID")?;
            for requirement in &function.requires {
                expr(
                    requirement,
                    &mut pending,
                    &mut pending_witnesses,
                    &mut slots,
                );
            }
            block(
                &function.body,
                &mut pending,
                &mut pending_witnesses,
                &mut slots,
            );
        }
        while let Some(id) = pending_witnesses.pop() {
            if witnesses.insert(id) {
                let witness = program
                    .witnesses
                    .get(id)
                    .ok_or("invalid checked witness ID")?;
                if library && witness.methods.iter().any(Option::is_none) {
                    return Err("exported witness tables must retain every method".into());
                }
                if library {
                    slots.extend(
                        (0..witness.methods.len()).map(|slot| (Type::Dyn(witness.interface), slot)),
                    );
                }
            }
        }
        // Only actual call slots activate a live witness's methods. Those
        // methods can construct further witnesses or call further slots.
        for &(ty, slot) in &slots {
            let Type::Dyn(interface) = ty else {
                return Err("invalid dyn receiver type".into());
            };
            program
                .interfaces
                .get(interface)
                .and_then(|value| value.methods.get(slot))
                .ok_or("invalid checked dyn method slot")?;
            for id in &witnesses {
                let witness = &program.witnesses[*id];
                if witness.interface == interface {
                    let target = witness
                        .methods
                        .get(slot)
                        .copied()
                        .flatten()
                        .ok_or("reachable dyn call refers to an absent witness method")?;
                    if !reachable.contains(&target) {
                        pending.push(target);
                    }
                }
            }
        }
        if pending.is_empty() {
            break;
        }
    }
    Ok(Reachable {
        functions: reachable,
        witnesses,
        slots,
    })
}

#[cfg(test)]
#[path = "llvm_float_tests.rs"]
mod float_tests;

#[cfg(test)]
#[path = "llvm_function_tests.rs"]
mod function_tests;

#[cfg(test)]
#[path = "llvm_memory_tests.rs"]
mod memory_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_fault_definitions_and_late_helpers_get_synchronous_unwind_tables() {
        let context = Context::create();
        let builder = context.create_builder();
        let kind = Attribute::get_named_enum_kind_id("uwtable");
        for enabled in [false, true] {
            let module = context.create_module("unwind");
            let ty = context.void_type().fn_type(&[], false);
            let declaration = module.add_function("loom_rt_fault", ty, None);
            for name in [
                "loom.fn.0",
                "loom.witness.0",
                "loom.cleanup.0",
                "loom.trace.0",
            ] {
                let function = module.add_function(name, ty, Some(Linkage::Internal));
                builder.position_at_end(context.append_basic_block(function, "entry"));
                builder.build_return(None).unwrap();
            }
            unwind_tables(&context, &module, enabled).unwrap();
            let late = module.add_function("late.helper", ty, Some(Linkage::Internal));
            builder.position_at_end(context.append_basic_block(late, "entry"));
            builder.build_return(None).unwrap();
            unwind_tables(&context, &module, enabled).unwrap();
            module.verify().unwrap();
            for function in module.get_functions().filter(|value| *value != declaration) {
                let attribute = function.get_enum_attribute(AttributeLoc::Function, kind);
                assert_eq!(
                    attribute.map(Attribute::get_enum_value),
                    enabled.then_some(1)
                );
                assert!(
                    function
                        .get_enum_attribute(
                            AttributeLoc::Function,
                            Attribute::get_named_enum_kind_id("nounwind"),
                        )
                        .is_none()
                );
            }
            assert!(
                declaration
                    .get_enum_attribute(AttributeLoc::Function, kind)
                    .is_none()
            );
            let text = module.print_to_string().to_string();
            assert_eq!(text.contains("uwtable(sync)"), enabled);
            assert_eq!(
                text.matches("!{i32 7, !\"uwtable\", i32 1}").count(),
                usize::from(enabled)
            );
        }
    }

    #[test]
    fn fault_writes_follow_host_crt_integer_widths() {
        let context = Context::create();
        for (windows, name, width) in [(false, "write", 64), (true, "_write", 32)] {
            let (symbol, result, count) = fault_write_abi(&context, context.i64_type(), windows);
            assert_eq!(symbol, name);
            assert_eq!(result.get_bit_width(), width);
            assert_eq!(count.get_bit_width(), width);
        }
    }

    #[test]
    fn enum_layout_uses_largest_payload_and_rejects_unbound_types() {
        let program = checked::Program {
            function_types: vec![],
            interfaces: vec![],
            witnesses: vec![],
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
            test_names: vec![],
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
