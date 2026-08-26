//! Mechanical scalar LCIR to LLVM emission.
//!
//! This module intentionally has no dependency on the checked-MIR emitter,
//! native-layout planning, universal values, or runtime-requirement analysis.
//! Every source function has exactly the ABI selected by its checked LCIR
//! signature and effects.

use std::cell::Cell;
use std::path::Path;

use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::debug_info::{DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder};
use inkwell::module::{FlagBehavior, Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::FileType;
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, StructType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PhiValue, PointerValue,
};
use inkwell::{FloatPredicate as LlvmFloatPredicate, IntPredicate};
use loom_codegen_ir::{
    BlockTarget, BoolPredicate, CheckedArtifact, CheckedIntBinaryOp, Constant, Effects, FaultCode,
    FloatBinaryOp, FloatPredicate as LcirFloatPredicate, Function, InstanceId, Instruction,
    InstructionKind, IntPredicate as LcirIntPredicate, Origin, Repr, ResultTarget, ScalarRepr,
    Terminator, TerminatorKind, UnwindTarget, ValueId, ValueTypeId,
};

use crate::CodegenError;
use crate::codegen::{DebugSource, NativeObjectArtifact, NativeObjectOptions};
use crate::target::create_llvm_target_machine;

pub(crate) struct LcirEmitter;

impl LcirEmitter {
    pub(crate) fn emit_object(
        artifact: &CheckedArtifact,
        output: &Path,
        options: &NativeObjectOptions,
    ) -> Result<NativeObjectArtifact, CodegenError> {
        let target =
            create_llvm_target_machine(options.target_triple.as_deref(), options.optimization)?;
        let llvm_pointer_bits = target
            .machine
            .get_target_data()
            .get_pointer_byte_size(None)
            .saturating_mul(8);
        let lcir_pointer_bits = u32::from(artifact.representations().target().pointer_bits());
        if llvm_pointer_bits != lcir_pointer_bits {
            return Err(CodegenError::new(
                "LcirTargetLayoutMismatch",
                format!(
                    "checked LCIR uses {lcir_pointer_bits}-bit pointers but LLVM target {} uses {llvm_pointer_bits}-bit pointers",
                    target.triple
                ),
            ));
        }

        let context = Context::create();
        let backend = Backend::new(&context, artifact, options)?;
        backend.module.set_triple(&target.triple);
        backend
            .module
            .set_data_layout(&target.machine.get_target_data().get_data_layout());
        backend.compile()?;
        backend.finalize_debug();
        verify(&backend.module)?;
        backend
            .module
            .run_passes(
                options.optimization.pipeline(),
                &target.machine,
                PassBuilderOptions::create(),
            )
            .map_err(|message| CodegenError::new("LlvmOptimizationFailed", message.to_string()))?;
        verify(&backend.module)?;

        if let Some(ir_path) = &options.emit_ir {
            create_parent_directory(ir_path)?;
            backend.module.print_to_file(ir_path).map_err(|message| {
                CodegenError::new(
                    "LlvmIrWriteFailed",
                    format!("{}: {message}", ir_path.display()),
                )
            })?;
        }
        create_parent_directory(output)?;
        target
            .machine
            .write_to_file(&backend.module, FileType::Object, output)
            .map_err(|message| CodegenError::new("LlvmObjectWriteFailed", message.to_string()))?;

        Ok(NativeObjectArtifact {
            object: output.to_path_buf(),
            functions: artifact.functions().len(),
            witnesses: 0,
        })
    }
}

fn verify(module: &Module<'_>) -> Result<(), CodegenError> {
    module
        .verify()
        .map_err(|message| CodegenError::new("LlvmVerificationFailed", message.to_string()))
}

fn create_parent_directory(path: &Path) -> Result<(), CodegenError> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    std::fs::create_dir_all(parent).map_err(|error| {
        CodegenError::new(
            "ArtifactWriteFailed",
            format!("{}: {error}", parent.display()),
        )
    })
}

struct DebugState<'ctx> {
    builder: DebugInfoBuilder<'ctx>,
}

impl<'ctx> DebugState<'ctx> {
    fn new(context: &'ctx Context, module: &Module<'ctx>, sources: &[DebugSource]) -> Self {
        let primary = sources
            .first()
            .map_or("<loom-generated>.loom", |source| source.path.as_str());
        module.set_source_file_name(primary);
        module.add_basic_value_flag(
            "Debug Info Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(3, false),
        );
        module.add_basic_value_flag(
            "Dwarf Version",
            FlagBehavior::Warning,
            context.i32_type().const_int(4, false),
        );
        let (builder, unit) = module.create_debug_info_builder(
            true,
            DWARFSourceLanguage::C,
            primary,
            ".",
            concat!("loomc lcir ", env!("CARGO_PKG_VERSION")),
            true,
            "",
            0,
            "",
            DWARFEmissionKind::Full,
            0,
            false,
            false,
            "",
            "",
        );
        for (index, source) in sources.iter().enumerate() {
            if index == 0 {
                let _ = unit.get_file();
            } else {
                let _ = builder.create_file(&source.path, ".");
            }
        }
        // LCIR has exact LLVM scalar signatures but not yet an agreed source
        // debug type contract for the fallible status aggregate and hidden
        // context. Keep the compile-unit/file metadata and deliberately omit
        // DISubprogram metadata instead of publishing a false signature.
        Self { builder }
    }
}

struct Backend<'ctx, 'artifact> {
    context: &'ctx Context,
    artifact: &'artifact CheckedArtifact,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    ptr_type: inkwell::types::PointerType<'ctx>,
    unit_type: StructType<'ctx>,
    fault_context_type: StructType<'ctx>,
    functions: Vec<FunctionValue<'ctx>>,
    debug: Option<DebugState<'ctx>>,
    names: Cell<u64>,
}

impl<'ctx, 'artifact> Backend<'ctx, 'artifact> {
    fn new(
        context: &'ctx Context,
        artifact: &'artifact CheckedArtifact,
        options: &'artifact NativeObjectOptions,
    ) -> Result<Self, CodegenError> {
        let module = context.create_module("loom.lcir.program");
        let builder = context.create_builder();
        let ptr_type = context.ptr_type(AddressSpace::default());
        let unit_type = context.struct_type(&[], false);
        let fault_context_type = context.opaque_struct_type("loom.lcir.FaultContext");
        fault_context_type.set_body(&[ptr_type.into(), context.bool_type().into()], false);
        let debug = (!options.debug_sources.is_empty())
            .then(|| DebugState::new(context, &module, &options.debug_sources));
        let mut backend = Self {
            context,
            artifact,
            module,
            builder,
            ptr_type,
            unit_type,
            fault_context_type,
            functions: Vec::with_capacity(artifact.functions().len()),
            debug,
            names: Cell::new(0),
        };
        backend.declare_functions()?;
        Ok(backend)
    }

    fn declare_functions(&mut self) -> Result<(), CodegenError> {
        for source in self.artifact.functions() {
            let mut params = source
                .signature()
                .params()
                .iter()
                .copied()
                .map(|ty| self.llvm_type(ty).map(Into::into))
                .collect::<Result<Vec<BasicMetadataTypeEnum<'ctx>>, _>>()?;
            if source.effects().contains(Effects::MAY_FAULT) {
                params.push(self.ptr_type.into());
            }
            let result = self.llvm_type(source.signature().result())?;
            let function_type = if source.effects().contains(Effects::MAY_FAULT) {
                self.context
                    .struct_type(&[self.context.i32_type().into(), result], false)
                    .fn_type(&params, false)
            } else {
                result.fn_type(&params, false)
            };
            let function = self.module.add_function(
                &format!("loom.lcir.fn.{}", source.id().raw()),
                function_type,
                Some(Linkage::Internal),
            );
            self.functions.push(function);
        }
        Ok(())
    }

    fn compile(&self) -> Result<(), CodegenError> {
        for source in self.artifact.functions() {
            FunctionEmitter::new(self, source)?.compile()?;
        }
        self.emit_main()
    }

    fn finalize_debug(&self) {
        self.builder.unset_current_debug_location();
        if let Some(debug) = &self.debug {
            debug.builder.finalize();
        }
    }

    fn llvm_type(&self, ty: ValueTypeId) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
        let value_type = self
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        match self
            .artifact
            .representations()
            .repr(value_type.repr())
            .copied()
        {
            Some(Repr::Zst) => Ok(self.unit_type.into()),
            Some(Repr::Scalar(ScalarRepr::I1)) => Ok(self.context.bool_type().into()),
            Some(Repr::Scalar(ScalarRepr::I64)) => Ok(self.context.i64_type().into()),
            Some(Repr::Scalar(ScalarRepr::F64)) => Ok(self.context.f64_type().into()),
            Some(Repr::Uninhabited) => Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("uninhabited LCIR type {ty} reached an LLVM value boundary"),
            )),
            None => Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("missing representation for LCIR type {ty}"),
            )),
        }
    }

    fn zero(&self, ty: ValueTypeId) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        Ok(match self.llvm_type(ty)? {
            BasicTypeEnum::ArrayType(ty) => ty.const_zero().into(),
            BasicTypeEnum::FloatType(ty) => ty.const_zero().into(),
            BasicTypeEnum::IntType(ty) => ty.const_zero().into(),
            BasicTypeEnum::PointerType(ty) => ty.const_null().into(),
            BasicTypeEnum::StructType(ty) => ty.const_zero().into(),
            BasicTypeEnum::VectorType(ty) => ty.const_zero().into(),
            BasicTypeEnum::ScalableVectorType(ty) => ty.const_zero().into(),
        })
    }

    fn function(&self, id: InstanceId) -> Result<FunctionValue<'ctx>, CodegenError> {
        self.functions.get(id.index()).copied().ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("LCIR function {id} is missing from the closed artifact"),
            )
        })
    }

    fn unique(&self, prefix: &str) -> String {
        let index = self.names.get();
        self.names.set(index.saturating_add(1));
        format!("loom.lcir.{prefix}.{index}")
    }
}

struct FunctionEmitter<'backend, 'ctx, 'artifact> {
    backend: &'backend Backend<'ctx, 'artifact>,
    source: &'artifact Function,
    function: FunctionValue<'ctx>,
    blocks: Vec<BasicBlock<'ctx>>,
    phis: Vec<Option<PhiValue<'ctx>>>,
    values: Vec<Option<BasicValueEnum<'ctx>>>,
    fault_context: Option<PointerValue<'ctx>>,
}

impl<'backend, 'ctx, 'artifact> FunctionEmitter<'backend, 'ctx, 'artifact> {
    fn new(
        backend: &'backend Backend<'ctx, 'artifact>,
        source: &'artifact Function,
    ) -> Result<Self, CodegenError> {
        let function = backend.function(source.id())?;
        let blocks = source
            .blocks()
            .iter()
            .map(|block| {
                backend
                    .context
                    .append_basic_block(function, &format!("b{}", block.id().raw()))
            })
            .collect::<Vec<_>>();
        let mut emitter = Self {
            backend,
            source,
            function,
            blocks,
            phis: vec![None; source.values().len()],
            values: vec![None; source.values().len()],
            fault_context: None,
        };
        emitter.prepare_parameters()?;
        Ok(emitter)
    }

    fn prepare_parameters(&mut self) -> Result<(), CodegenError> {
        let entry = self.source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", self.source.id()),
            )
        })?;
        let entry_block = self.source.block(entry).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has a missing entry block", self.source.id()),
            )
        })?;
        for (index, value_id) in entry_block.params().iter().copied().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = self.function.get_nth_param(index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing LLVM parameter {index}", self.source.id()),
                )
            })?;
            self.values[value_id.index()] = Some(value);
        }
        if self.source.effects().contains(Effects::MAY_FAULT) {
            let index = u32::try_from(self.source.signature().params().len())
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            self.fault_context = Some(
                self.function
                    .get_nth_param(index)
                    .map(BasicValueEnum::into_pointer_value)
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("{} is missing its fault-context pointer", self.source.id()),
                        )
                    })?,
            );
        }

        for block in self.source.blocks() {
            if block.id() == entry {
                continue;
            }
            self.backend
                .builder
                .position_at_end(self.blocks[block.id().index()]);
            for value_id in block.params() {
                let value = self.source.value(*value_id).ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("missing block parameter {value_id}"),
                    )
                })?;
                let phi = self
                    .backend
                    .builder
                    .build_phi(
                        self.backend.llvm_type(value.ty())?,
                        &format!("v{}", value_id.raw()),
                    )
                    .map_err(builder_error)?;
                self.phis[value_id.index()] = Some(phi);
                self.values[value_id.index()] = Some(phi.as_basic_value());
            }
        }
        Ok(())
    }

    fn compile(mut self) -> Result<(), CodegenError> {
        for block in self.source.blocks() {
            self.backend
                .builder
                .position_at_end(self.blocks[block.id().index()]);
            for instruction_id in block.instructions() {
                let instruction = self.source.instruction(*instruction_id).ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("missing LCIR instruction {instruction_id}"),
                    )
                })?;
                self.emit_instruction(instruction)?;
            }
            let terminator = block.terminator().ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR block {} is unterminated", block.id()),
                )
            })?;
            self.emit_terminator(terminator)?;
        }
        self.backend.builder.unset_current_debug_location();
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn emit_instruction(&mut self, instruction: &Instruction) -> Result<(), CodegenError> {
        let value = match instruction.kind() {
            InstructionKind::Constant(constant) => self.emit_constant(*constant)?,
            InstructionKind::BoolNot { value } => self
                .backend
                .builder
                .build_not(self.int(*value)?, "bool.not")
                .map(Into::into)
                .map_err(builder_error)?,
            InstructionKind::BoolCompare {
                predicate,
                left,
                right,
            } => {
                let predicate = match predicate {
                    BoolPredicate::Equal => IntPredicate::EQ,
                    BoolPredicate::NotEqual => IntPredicate::NE,
                };
                self.backend
                    .builder
                    .build_int_compare(
                        predicate,
                        self.int(*left)?,
                        self.int(*right)?,
                        "bool.compare",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?
            }
            InstructionKind::FloatNegate { value } => self
                .backend
                .builder
                .build_float_neg(self.value(*value)?.into_float_value(), "float.negate")
                .map(Into::into)
                .map_err(builder_error)?,
            InstructionKind::FloatBinary { op, left, right } => {
                let left = self.value(*left)?.into_float_value();
                let right = self.value(*right)?.into_float_value();
                match op {
                    FloatBinaryOp::Add => {
                        self.backend
                            .builder
                            .build_float_add(left, right, "float.add")
                    }
                    FloatBinaryOp::Subtract => {
                        self.backend
                            .builder
                            .build_float_sub(left, right, "float.subtract")
                    }
                    FloatBinaryOp::Multiply => {
                        self.backend
                            .builder
                            .build_float_mul(left, right, "float.multiply")
                    }
                    FloatBinaryOp::Divide => {
                        self.backend
                            .builder
                            .build_float_div(left, right, "float.divide")
                    }
                }
                .map(Into::into)
                .map_err(builder_error)?
            }
            InstructionKind::IntCompare {
                predicate,
                left,
                right,
            } => {
                let predicate = match predicate {
                    LcirIntPredicate::Equal => IntPredicate::EQ,
                    LcirIntPredicate::NotEqual => IntPredicate::NE,
                    LcirIntPredicate::Less => IntPredicate::SLT,
                    LcirIntPredicate::LessEqual => IntPredicate::SLE,
                    LcirIntPredicate::Greater => IntPredicate::SGT,
                    LcirIntPredicate::GreaterEqual => IntPredicate::SGE,
                };
                self.backend
                    .builder
                    .build_int_compare(
                        predicate,
                        self.int(*left)?,
                        self.int(*right)?,
                        "int.compare",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?
            }
            InstructionKind::FloatCompare {
                predicate,
                left,
                right,
            } => {
                let predicate = match predicate {
                    LcirFloatPredicate::OrderedEqual => LlvmFloatPredicate::OEQ,
                    LcirFloatPredicate::UnorderedNotEqual => LlvmFloatPredicate::UNE,
                    LcirFloatPredicate::OrderedLess => LlvmFloatPredicate::OLT,
                    LcirFloatPredicate::OrderedLessEqual => LlvmFloatPredicate::OLE,
                    LcirFloatPredicate::OrderedGreater => LlvmFloatPredicate::OGT,
                    LcirFloatPredicate::OrderedGreaterEqual => LlvmFloatPredicate::OGE,
                };
                self.backend
                    .builder
                    .build_float_compare(
                        predicate,
                        self.value(*left)?.into_float_value(),
                        self.value(*right)?.into_float_value(),
                        "float.compare",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?
            }
            InstructionKind::DirectCall { callee, arguments } => {
                let arguments = self.call_arguments(arguments, false)?;
                call_basic(
                    &self.backend.builder,
                    self.backend.function(*callee)?,
                    &arguments,
                    "direct.call",
                )?
            }
        };
        let [result] = instruction.results() else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("{} does not have exactly one result", instruction.id()),
            ));
        };
        self.values[result.index()] = Some(value);
        Ok(())
    }

    fn emit_constant(&self, constant: Constant) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        Ok(match constant {
            Constant::Unit => self.backend.unit_type.const_zero().into(),
            Constant::Bool(value) => self
                .backend
                .context
                .bool_type()
                .const_int(u64::from(value), false)
                .into(),
            Constant::Int(value) => self
                .backend
                .context
                .i64_type()
                .const_int(value.cast_unsigned(), false)
                .into(),
            Constant::FloatBits(bits) => self
                .backend
                .builder
                .build_bit_cast(
                    self.backend.context.i64_type().const_int(bits, false),
                    self.backend.context.f64_type(),
                    "float.from.bits",
                )
                .map_err(builder_error)?,
        })
    }

    #[allow(clippy::too_many_lines)]
    fn emit_terminator(&mut self, terminator: &Terminator) -> Result<(), CodegenError> {
        match terminator.kind() {
            TerminatorKind::Jump(target) => self.branch(target),
            TerminatorKind::Branch {
                condition,
                then_target,
                else_target,
            } => {
                if then_target.block == else_target.block {
                    let then_edge = self
                        .backend
                        .context
                        .append_basic_block(self.function, "branch.then.edge");
                    let else_edge = self
                        .backend
                        .context
                        .append_basic_block(self.function, "branch.else.edge");
                    self.backend
                        .builder
                        .build_conditional_branch(self.int(*condition)?, then_edge, else_edge)
                        .map_err(builder_error)?;
                    self.backend.builder.position_at_end(then_edge);
                    self.branch(then_target)?;
                    self.backend.builder.position_at_end(else_edge);
                    self.branch(else_target)?;
                } else {
                    let predecessor = self.current_block()?;
                    self.add_target_incoming(then_target, predecessor)?;
                    self.add_target_incoming(else_target, predecessor)?;
                    self.backend
                        .builder
                        .build_conditional_branch(
                            self.int(*condition)?,
                            self.block(then_target.block)?,
                            self.block(else_target.block)?,
                        )
                        .map_err(builder_error)?;
                }
                Ok(())
            }
            TerminatorKind::Return(value) => self.emit_return(self.value(*value)?),
            TerminatorKind::CheckedIntNegate {
                value,
                normal,
                fault,
            } => {
                let zero = self.backend.context.i64_type().const_zero();
                let (result, overflow) =
                    self.checked_intrinsic("llvm.ssub.with.overflow", zero, self.int(*value)?)?;
                self.checked_branch(
                    result.into(),
                    overflow,
                    FaultCode::IntegerOverflow,
                    terminator.origin(),
                    normal,
                    fault,
                )
            }
            TerminatorKind::CheckedIntBinary {
                op,
                left,
                right,
                normal,
                fault,
            } => {
                let left = self.int(*left)?;
                let right = self.int(*right)?;
                match op {
                    CheckedIntBinaryOp::Divide => {
                        self.checked_divide(left, right, terminator.origin(), normal, fault)
                    }
                    CheckedIntBinaryOp::Add => self.checked_intrinsic_branch(
                        "llvm.sadd.with.overflow",
                        left,
                        right,
                        terminator.origin(),
                        normal,
                        fault,
                    ),
                    CheckedIntBinaryOp::Subtract => self.checked_intrinsic_branch(
                        "llvm.ssub.with.overflow",
                        left,
                        right,
                        terminator.origin(),
                        normal,
                        fault,
                    ),
                    CheckedIntBinaryOp::Multiply => self.checked_intrinsic_branch(
                        "llvm.smul.with.overflow",
                        left,
                        right,
                        terminator.origin(),
                        normal,
                        fault,
                    ),
                }
            }
            TerminatorKind::Invoke {
                callee,
                arguments,
                normal,
                unwind,
            } => self.emit_invoke(*callee, arguments, normal, unwind),
            TerminatorKind::Assert {
                condition,
                code,
                success,
                fault,
            } => self.emit_assert(
                self.int(*condition)?,
                *code,
                terminator.origin(),
                success,
                fault,
            ),
            TerminatorKind::Fault { code } => {
                self.emit_source_fault(*code, terminator.origin())?;
                self.emit_fault_return()
            }
            TerminatorKind::ResumeFault => self.emit_fault_return(),
        }
    }

    fn checked_intrinsic(
        &self,
        name: &str,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let intrinsic = inkwell::intrinsics::Intrinsic::find(name)
            .and_then(|intrinsic| {
                intrinsic.get_declaration(
                    &self.backend.module,
                    &[self.backend.context.i64_type().into()],
                )
            })
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing {name}")))?;
        let aggregate = call_basic(
            &self.backend.builder,
            intrinsic,
            &[left.into(), right.into()],
            "checked.int",
        )?
        .into_struct_value();
        let result = self
            .backend
            .builder
            .build_extract_value(aggregate, 0, "checked.int.result")
            .map_err(builder_error)?
            .into_int_value();
        let overflow = self
            .backend
            .builder
            .build_extract_value(aggregate, 1, "checked.int.overflow")
            .map_err(builder_error)?
            .into_int_value();
        Ok((result, overflow))
    }

    fn checked_intrinsic_branch(
        &mut self,
        intrinsic: &str,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let (result, overflow) = self.checked_intrinsic(intrinsic, left, right)?;
        self.checked_branch(
            result.into(),
            overflow,
            FaultCode::IntegerOverflow,
            origin,
            normal,
            fault,
        )
    }

    fn checked_branch(
        &mut self,
        result: BasicValueEnum<'ctx>,
        failed: IntValue<'ctx>,
        code: FaultCode,
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let predecessor = self.current_block()?;
        self.add_result_incoming(normal, result, predecessor)?;
        let report = self
            .backend
            .context
            .append_basic_block(self.function, "checked.fault");
        self.backend
            .builder
            .build_conditional_branch(failed, report, self.block(normal.block)?)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(report);
        self.emit_source_fault(code, origin)?;
        self.unwind_branch(fault)
    }

    fn checked_divide(
        &mut self,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let check_overflow = self
            .backend
            .context
            .append_basic_block(self.function, "division.check.overflow");
        let zero_fault = self
            .backend
            .context
            .append_basic_block(self.function, "division.zero.fault");
        let overflow_fault = self
            .backend
            .context
            .append_basic_block(self.function, "division.overflow.fault");
        let divide = self
            .backend
            .context
            .append_basic_block(self.function, "division.safe");
        let is_zero = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                right,
                self.backend.context.i64_type().const_zero(),
                "division.by.zero",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_zero, zero_fault, check_overflow)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(zero_fault);
        self.emit_source_fault(FaultCode::IntegerDivisionByZero, origin)?;
        self.unwind_branch(fault)?;

        self.backend.builder.position_at_end(check_overflow);
        let is_minimum = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                left,
                self.backend
                    .context
                    .i64_type()
                    .const_int(i64::MIN.cast_unsigned(), false),
                "division.is.minimum",
            )
            .map_err(builder_error)?;
        let is_minus_one = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                right,
                self.backend
                    .context
                    .i64_type()
                    .const_int((-1_i64).cast_unsigned(), false),
                "division.is.minus.one",
            )
            .map_err(builder_error)?;
        let overflows = self
            .backend
            .builder
            .build_and(is_minimum, is_minus_one, "division.overflows")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(overflows, overflow_fault, divide)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(overflow_fault);
        self.emit_source_fault(FaultCode::IntegerDivisionOverflow, origin)?;
        self.unwind_branch(fault)?;

        self.backend.builder.position_at_end(divide);
        let quotient = self
            .backend
            .builder
            .build_int_signed_div(left, right, "division.result")
            .map_err(builder_error)?;
        self.result_branch(normal, quotient.into())
    }

    fn emit_invoke(
        &mut self,
        callee: InstanceId,
        arguments: &[ValueId],
        normal: &ResultTarget,
        unwind: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let arguments = self.call_arguments(arguments, true)?;
        let aggregate = call_basic(
            &self.backend.builder,
            self.backend.function(callee)?,
            &arguments,
            "invoke.call",
        )?
        .into_struct_value();
        let status = self
            .backend
            .builder
            .build_extract_value(aggregate, 0, "invoke.status")
            .map_err(builder_error)?
            .into_int_value();
        let result = self
            .backend
            .builder
            .build_extract_value(aggregate, 1, "invoke.result")
            .map_err(builder_error)?;
        let succeeded = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_zero(),
                "invoke.succeeded",
            )
            .map_err(builder_error)?;
        let predecessor = self.current_block()?;
        self.add_result_incoming(normal, result, predecessor)?;
        self.add_unwind_incoming(unwind, predecessor)?;
        self.backend
            .builder
            .build_conditional_branch(
                succeeded,
                self.block(normal.block)?,
                self.block(unwind.block)?,
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_assert(
        &mut self,
        condition: IntValue<'ctx>,
        code: FaultCode,
        origin: Origin,
        success: &BlockTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let predecessor = self.current_block()?;
        self.add_target_incoming(success, predecessor)?;
        let report = self
            .backend
            .context
            .append_basic_block(self.function, "assert.fault");
        self.backend
            .builder
            .build_conditional_branch(condition, self.block(success.block)?, report)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(report);
        self.emit_source_fault(code, origin)?;
        self.unwind_branch(fault)
    }

    fn emit_source_fault(&self, code: FaultCode, origin: Origin) -> Result<(), CodegenError> {
        let context = self.fault_context.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "infallible {} attempted to raise a source fault",
                    self.source.id()
                ),
            )
        })?;
        let active_pointer = self
            .backend
            .builder
            .build_struct_gep(
                self.backend.fault_context_type,
                context,
                1,
                "fault.primary.active.pointer",
            )
            .map_err(builder_error)?;
        let active = self
            .backend
            .builder
            .build_load(
                self.backend.context.bool_type(),
                active_pointer,
                "fault.primary.active",
            )
            .map_err(builder_error)?
            .into_int_value();
        let function = self.function;
        let report = self
            .backend
            .context
            .append_basic_block(function, "fault.report.primary");
        let suppress = self
            .backend
            .context
            .append_basic_block(function, "fault.suppress.secondary");
        let continuation = self
            .backend
            .context
            .append_basic_block(function, "fault.reported");
        self.backend
            .builder
            .build_conditional_branch(active, suppress, report)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(report);
        self.backend
            .builder
            .build_store(
                active_pointer,
                self.backend.context.bool_type().const_int(1, false),
            )
            .map_err(builder_error)?;
        let runtime_pointer = self
            .backend
            .builder
            .build_struct_gep(
                self.backend.fault_context_type,
                context,
                0,
                "fault.runtime.pointer",
            )
            .map_err(builder_error)?;
        let runtime = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, runtime_pointer, "fault.runtime")
            .map_err(builder_error)?
            .into_pointer_value();
        self.backend.raise_fault(runtime, code, origin)?;
        self.backend
            .builder
            .build_unconditional_branch(continuation)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(suppress);
        self.backend
            .builder
            .build_unconditional_branch(continuation)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(continuation);
        Ok(())
    }

    fn emit_return(&self, value: BasicValueEnum<'ctx>) -> Result<(), CodegenError> {
        if self.source.effects().contains(Effects::MAY_FAULT) {
            self.emit_status_return(self.backend.context.i32_type().const_zero(), value)
        } else {
            self.backend
                .builder
                .build_return(Some(&value))
                .map_err(builder_error)?;
            Ok(())
        }
    }

    fn emit_fault_return(&self) -> Result<(), CodegenError> {
        let zero = self.backend.zero(self.source.signature().result())?;
        self.emit_status_return(self.backend.context.i32_type().const_int(1, false), zero)
    }

    fn emit_status_return(
        &self,
        status: IntValue<'ctx>,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), CodegenError> {
        if !self.source.effects().contains(Effects::MAY_FAULT) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("infallible {} attempted a status return", self.source.id()),
            ));
        }
        let return_type = self
            .function
            .get_type()
            .get_return_type()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "fallible function returns void"))?
            .into_struct_type();
        let aggregate = self
            .backend
            .builder
            .build_insert_value(return_type.get_undef(), status, 0, "return.status")
            .map_err(builder_error)?
            .into_struct_value();
        let aggregate = self
            .backend
            .builder
            .build_insert_value(aggregate, value, 1, "return.value")
            .map_err(builder_error)?
            .into_struct_value();
        self.backend
            .builder
            .build_return(Some(&aggregate))
            .map_err(builder_error)?;
        Ok(())
    }

    fn call_arguments(
        &self,
        arguments: &[ValueId],
        with_fault_context: bool,
    ) -> Result<Vec<BasicMetadataValueEnum<'ctx>>, CodegenError> {
        let mut values = arguments
            .iter()
            .copied()
            .map(|value| self.value(value).map(Into::into))
            .collect::<Result<Vec<_>, _>>()?;
        if with_fault_context {
            values.push(
                self.fault_context
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("{} invokes without a fault context", self.source.id()),
                        )
                    })?
                    .into(),
            );
        }
        Ok(values)
    }

    fn branch(&self, target: &BlockTarget) -> Result<(), CodegenError> {
        let predecessor = self.current_block()?;
        self.add_target_incoming(target, predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(target.block)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn result_branch(
        &self,
        target: &ResultTarget,
        result: BasicValueEnum<'ctx>,
    ) -> Result<(), CodegenError> {
        let predecessor = self.current_block()?;
        self.add_result_incoming(target, result, predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(target.block)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn unwind_branch(&self, target: &UnwindTarget) -> Result<(), CodegenError> {
        let predecessor = self.current_block()?;
        self.add_unwind_incoming(target, predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(target.block)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn add_target_incoming(
        &self,
        target: &BlockTarget,
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        self.add_incoming(target.block, &target.arguments, predecessor)
    }

    fn add_unwind_incoming(
        &self,
        target: &UnwindTarget,
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        self.add_incoming(target.block, &target.arguments, predecessor)
    }

    fn add_result_incoming(
        &self,
        target: &ResultTarget,
        result: BasicValueEnum<'ctx>,
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let mut values = Vec::with_capacity(target.arguments.len() + 1);
        values.push(result);
        for argument in &target.arguments {
            values.push(self.value(*argument)?);
        }
        self.add_basic_incoming(target.block, &values, predecessor)
    }

    fn add_incoming(
        &self,
        block: loom_codegen_ir::BlockId,
        arguments: &[ValueId],
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let values = arguments
            .iter()
            .copied()
            .map(|argument| self.value(argument))
            .collect::<Result<Vec<_>, _>>()?;
        self.add_basic_incoming(block, &values, predecessor)
    }

    fn add_basic_incoming(
        &self,
        block: loom_codegen_ir::BlockId,
        values: &[BasicValueEnum<'ctx>],
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let destination = self.source.block(block).ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", format!("missing edge destination {block}"))
        })?;
        if destination.params().len() != values.len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "edge to {block} carries {} values for {} parameters",
                    values.len(),
                    destination.params().len()
                ),
            ));
        }
        for (parameter, value) in destination.params().iter().zip(values) {
            let phi = self.phis[parameter.index()].ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("edge destination parameter {parameter} has no phi"),
                )
            })?;
            phi.add_incoming(&[(value, predecessor)]);
        }
        Ok(())
    }

    fn value(&self, id: ValueId) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        self.values
            .get(id.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR value {id} has no LLVM definition"),
                )
            })
    }

    fn int(&self, id: ValueId) -> Result<IntValue<'ctx>, CodegenError> {
        Ok(self.value(id)?.into_int_value())
    }

    fn block(&self, id: loom_codegen_ir::BlockId) -> Result<BasicBlock<'ctx>, CodegenError> {
        self.blocks.get(id.index()).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR block {id} has no LLVM block"),
            )
        })
    }

    fn current_block(&self) -> Result<BasicBlock<'ctx>, CodegenError> {
        self.backend.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new(
                "LlvmBuilderFailed",
                "LLVM builder is not positioned in a block",
            )
        })
    }
}

impl<'ctx> Backend<'ctx, '_> {
    fn emit_main(&self) -> Result<(), CodegenError> {
        let main_type = self.context.i32_type().fn_type(
            &[self.context.i32_type().into(), self.ptr_type.into()],
            false,
        );
        let main = self.module.add_function("main", main_type, None);
        let entry = self.context.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        if let Some(root) = self.artifact.run_root() {
            let source = self.artifact.function(root).ok_or_else(|| {
                CodegenError::new("InvalidFunctionReference", "LCIR run root is missing")
            })?;
            if source.effects().contains(Effects::MAY_FAULT) {
                self.emit_fallible_run(main, root)
            } else {
                self.builder
                    .build_call(self.function(root)?, &[], "run")
                    .map_err(builder_error)?;
                self.puts("Unit")?;
                self.builder
                    .build_return(Some(&self.context.i32_type().const_zero()))
                    .map_err(builder_error)?;
                Ok(())
            }
        } else {
            self.emit_tests(main)
        }
    }

    fn emit_fallible_run(
        &self,
        main: FunctionValue<'ctx>,
        root: InstanceId,
    ) -> Result<(), CodegenError> {
        let runtime = call_pointer(&self.builder, self.runtime_create(), &[], "runtime.root")?;
        let ready = self.context.append_basic_block(main, "runtime.root.ready");
        let create_failed = self
            .context
            .append_basic_block(main, "runtime.root.create.failed");
        let exists = self
            .builder
            .build_is_not_null(runtime, "runtime.root.exists")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exists, ready, create_failed)
            .map_err(builder_error)?;

        self.builder.position_at_end(create_failed);
        self.puts("RuntimeFault: runtime creation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        self.builder.position_at_end(ready);
        let activation = call_int(
            &self.builder,
            self.runtime_activate(),
            &[runtime.into()],
            "runtime.root.activate",
        )?;
        let activated = self
            .context
            .append_basic_block(main, "runtime.root.activated");
        let activation_failed = self
            .context
            .append_basic_block(main, "runtime.root.activation.failed");
        let activation_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                activation,
                self.context.i32_type().const_zero(),
                "runtime.root.activation.ok",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(activation_ok, activated, activation_failed)
            .map_err(builder_error)?;

        self.builder.position_at_end(activation_failed);
        self.builder
            .build_call(
                self.runtime_destroy(),
                &[runtime.into()],
                "runtime.root.activation.destroy",
            )
            .map_err(builder_error)?;
        self.puts("RuntimeFault: runtime activation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        self.builder.position_at_end(activated);
        let fault_context = self.initialize_fault_context(runtime)?;
        let status = self.call_fallible_root(root, fault_context, "run")?;
        self.destroy_runtime(runtime)?;
        let success = self.context.append_basic_block(main, "run.success");
        let failure = self.context.append_basic_block(main, "run.failure");
        let succeeded = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_zero(),
                "run.succeeded",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(succeeded, success, failure)
            .map_err(builder_error)?;

        self.builder.position_at_end(success);
        self.puts("Unit")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_zero()))
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        self.builder
            .build_return(Some(&status))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_tests(&self, main: FunctionValue<'ctx>) -> Result<(), CodegenError> {
        let failed = self
            .builder
            .build_alloca(self.context.i32_type(), "tests.failed")
            .map_err(builder_error)?;
        self.builder
            .build_store(failed, self.context.i32_type().const_zero())
            .map_err(builder_error)?;
        let roots = self.artifact.test_roots().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "LCIR test artifact has no ordered roots")
        })?;
        for root in roots {
            let source = self.artifact.function(*root).ok_or_else(|| {
                CodegenError::new(
                    "InvalidFunctionReference",
                    format!("LCIR test root {root} is missing"),
                )
            })?;
            if source.effects().contains(Effects::MAY_FAULT) {
                self.emit_fallible_test(main, *root, source.name(), failed)?;
            } else {
                self.builder
                    .build_call(self.function(*root)?, &[], "test")
                    .map_err(builder_error)?;
                self.puts(&format!("passed {}", source.name()))?;
            }
        }
        let status = self
            .builder
            .build_load(self.context.i32_type(), failed, "tests.status")
            .map_err(builder_error)?;
        self.builder
            .build_return(Some(&status))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_fallible_test(
        &self,
        main: FunctionValue<'ctx>,
        root: InstanceId,
        name: &str,
        failed: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let runtime = call_pointer(&self.builder, self.runtime_create(), &[], "test.runtime")?;
        let ready = self.context.append_basic_block(main, "test.runtime.ready");
        let setup_failed = self
            .context
            .append_basic_block(main, "test.runtime.setup.failed");
        let exists = self
            .builder
            .build_is_not_null(runtime, "test.runtime.exists")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exists, ready, setup_failed)
            .map_err(builder_error)?;

        self.builder.position_at_end(ready);
        let activation = call_int(
            &self.builder,
            self.runtime_activate(),
            &[runtime.into()],
            "test.runtime.activate",
        )?;
        let activated = self
            .context
            .append_basic_block(main, "test.runtime.activated");
        let activation_failed = self
            .context
            .append_basic_block(main, "test.runtime.activation.failed");
        let activation_ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                activation,
                self.context.i32_type().const_zero(),
                "test.runtime.activation.ok",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(activation_ok, activated, activation_failed)
            .map_err(builder_error)?;

        self.builder.position_at_end(activation_failed);
        self.builder
            .build_call(
                self.runtime_destroy(),
                &[runtime.into()],
                "test.runtime.activation.destroy",
            )
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(setup_failed)
            .map_err(builder_error)?;

        let next = self.context.append_basic_block(main, "test.next");
        self.builder.position_at_end(setup_failed);
        self.puts(&format!("failed {name}"))?;
        self.builder
            .build_store(failed, self.context.i32_type().const_int(1, false))
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(next)
            .map_err(builder_error)?;

        self.builder.position_at_end(activated);
        let fault_context = self.initialize_fault_context(runtime)?;
        let status = self.call_fallible_root(root, fault_context, "test")?;
        self.destroy_runtime(runtime)?;
        let pass = self.context.append_basic_block(main, "test.pass");
        let fail = self.context.append_basic_block(main, "test.fail");
        let succeeded = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_zero(),
                "test.succeeded",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(succeeded, pass, fail)
            .map_err(builder_error)?;

        self.builder.position_at_end(pass);
        self.puts(&format!("passed {name}"))?;
        self.builder
            .build_unconditional_branch(next)
            .map_err(builder_error)?;
        self.builder.position_at_end(fail);
        self.puts(&format!("failed {name}"))?;
        self.builder
            .build_store(failed, self.context.i32_type().const_int(1, false))
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(next)
            .map_err(builder_error)?;
        self.builder.position_at_end(next);
        Ok(())
    }

    fn initialize_fault_context(
        &self,
        runtime: PointerValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let context = self
            .builder
            .build_alloca(self.fault_context_type, "fault.context")
            .map_err(builder_error)?;
        self.builder
            .build_store(context, self.fault_context_type.const_zero())
            .map_err(builder_error)?;
        let runtime_pointer = self
            .builder
            .build_struct_gep(
                self.fault_context_type,
                context,
                0,
                "fault.context.runtime.pointer",
            )
            .map_err(builder_error)?;
        self.builder
            .build_store(runtime_pointer, runtime)
            .map_err(builder_error)?;
        Ok(context)
    }

    fn call_fallible_root(
        &self,
        root: InstanceId,
        context: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let aggregate = call_basic(&self.builder, self.function(root)?, &[context.into()], name)?
            .into_struct_value();
        Ok(self
            .builder
            .build_extract_value(aggregate, 0, &format!("{name}.status"))
            .map_err(builder_error)?
            .into_int_value())
    }

    fn destroy_runtime(&self, runtime: PointerValue<'ctx>) -> Result<(), CodegenError> {
        self.builder
            .build_call(
                self.runtime_deactivate(),
                &[runtime.into()],
                "runtime.root.deactivate",
            )
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.runtime_destroy(),
                &[runtime.into()],
                "runtime.root.destroy",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn puts(&self, value: &str) -> Result<(), CodegenError> {
        let string = self
            .builder
            .build_global_string_ptr(value, &self.unique("string"))
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.libc_puts(),
                &[string.as_pointer_value().into()],
                "puts",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn raise_fault(
        &self,
        runtime: PointerValue<'ctx>,
        fault: FaultCode,
        origin: Origin,
    ) -> Result<(), CodegenError> {
        let (code, message) = fault_properties(fault);
        let display = format!("{code}: {message}");
        let detail = serde_json::json!({
            "channel": "lcir",
            "code": code,
            "sourceFunction": origin.source_function.0,
            "sourceSpan": {
                "file": origin.span.file.0,
                "start": origin.span.range.start,
                "end": origin.span.range.end,
            },
        })
        .to_string();
        let code_data = self
            .builder
            .build_global_string_ptr(code, &self.unique("fault.code"))
            .map_err(builder_error)?;
        let message_data = self
            .builder
            .build_global_string_ptr(message, &self.unique("fault.message"))
            .map_err(builder_error)?;
        let display_data = self
            .builder
            .build_global_string_ptr(&display, &self.unique("fault.display"))
            .map_err(builder_error)?;
        let detail_data = self
            .builder
            .build_global_string_ptr(&detail, &self.unique("fault.detail"))
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.context_raise_fault()?,
                &[
                    runtime.into(),
                    code_data.as_pointer_value().into(),
                    self.context
                        .i64_type()
                        .const_int(code.len() as u64, false)
                        .into(),
                    message_data.as_pointer_value().into(),
                    self.context
                        .i64_type()
                        .const_int(message.len() as u64, false)
                        .into(),
                    display_data.as_pointer_value().into(),
                    self.context
                        .i64_type()
                        .const_int(display.len() as u64, false)
                        .into(),
                    detail_data.as_pointer_value().into(),
                    self.context
                        .i64_type()
                        .const_int(detail.len() as u64, false)
                        .into(),
                ],
                "fault.raise",
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn runtime_create(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_runtime_create_v1")
            .unwrap_or_else(|| {
                let function_type = self.ptr_type.fn_type(&[], false);
                self.module
                    .add_function("loom_runtime_create_v1", function_type, None)
            })
    }

    fn runtime_activate(&self) -> FunctionValue<'ctx> {
        self.runtime_status_function("loom_runtime_activate_v1")
    }

    fn runtime_deactivate(&self) -> FunctionValue<'ctx> {
        self.runtime_status_function("loom_runtime_deactivate_v1")
    }

    fn runtime_destroy(&self) -> FunctionValue<'ctx> {
        self.runtime_status_function("loom_runtime_destroy_v1")
    }

    fn runtime_status_function(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into()], false);
            self.module.add_function(name, function_type, None)
        })
    }

    fn context_raise_fault(&self) -> Result<FunctionValue<'ctx>, CodegenError> {
        if let Some(function) = self.module.get_function("loom_context_raise_fault_v1") {
            return Ok(function);
        }
        let function_type = self.context.i32_type().fn_type(
            &[
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
            ],
            false,
        );
        let function = self
            .module
            .add_function("loom_context_raise_fault_v1", function_type, None);
        mark_cold_noinline(self.context, function)?;
        Ok(function)
    }

    fn libc_puts(&self) -> FunctionValue<'ctx> {
        self.module.get_function("puts").unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into()], false);
            self.module.add_function("puts", function_type, None)
        })
    }
}

fn fault_properties(code: FaultCode) -> (&'static str, &'static str) {
    match code {
        FaultCode::IntegerOverflow => ("IntegerOverflow", "integer arithmetic overflowed"),
        FaultCode::IntegerDivisionByZero => ("IntegerDivisionByZero", "integer division by zero"),
        FaultCode::IntegerDivisionOverflow => {
            ("IntegerDivisionOverflow", "integer division overflowed")
        }
        FaultCode::AssertionFailed => ("AssertionFailed", "assertion failed"),
        // LCIR does not yet carry the legacy contract category, blame span, or
        // contract span. The stable generic code remains faithfully emit-able.
        FaultCode::ContractFailed => ("ContractFailed", "contract failed"),
    }
}

fn mark_cold_noinline(context: &Context, function: FunctionValue<'_>) -> Result<(), CodegenError> {
    for name in ["cold", "noinline"] {
        let kind = Attribute::get_named_enum_kind_id(name);
        if kind == 0 {
            return Err(CodegenError::new(
                "LlvmAttributeUnavailable",
                format!("LLVM does not provide the `{name}` function attribute"),
            ));
        }
        function.add_attribute(
            AttributeLoc::Function,
            context.create_enum_attribute(kind, 0),
        );
    }
    Ok(())
}

fn call_basic<'ctx>(
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    builder
        .build_call(function, arguments, name)
        .map_err(builder_error)?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "LLVM call returned no value"))
}

fn call_int<'ctx>(
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<IntValue<'ctx>, CodegenError> {
    Ok(call_basic(builder, function, arguments, name)?.into_int_value())
}

fn call_pointer<'ctx>(
    builder: &Builder<'ctx>,
    function: FunctionValue<'ctx>,
    arguments: &[BasicMetadataValueEnum<'ctx>],
    name: &str,
) -> Result<PointerValue<'ctx>, CodegenError> {
    Ok(call_basic(builder, function, arguments, name)?.into_pointer_value())
}

#[allow(clippy::needless_pass_by_value)]
fn builder_error(error: inkwell::builder::BuilderError) -> CodegenError {
    CodegenError::new("LlvmBuilderFailed", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn bare_artifact_paths_do_not_attempt_to_create_an_empty_parent() {
        super::create_parent_directory(Path::new("artifact.o"))
            .expect("a bare artifact path has no directory to create");
    }
}
