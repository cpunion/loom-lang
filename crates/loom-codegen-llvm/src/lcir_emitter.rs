//! Mechanical typed LCIR to LLVM emission.
//!
//! This module intentionally has no dependency on the checked-MIR emitter,
//! native-layout planning, universal values, or runtime-requirement analysis.
//! Every source function has exactly the ABI selected by its checked LCIR
//! signature and effects.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::path::Path;

use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::debug_info::{
    AsDIScope, DIFile, DIFlags, DIFlagsConstants, DILocalVariable, DILocation, DIType,
    DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{ByteOrdering, FileType, TargetData};
use inkwell::types::{
    AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, IntType, StructType,
};
use inkwell::values::{
    AsValueRef, BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PhiValue,
    PointerValue, UnnamedAddress,
};
use inkwell::{FloatPredicate as LlvmFloatPredicate, IntPredicate};
use llvm_sys::debuginfo::LLVMDIBuilderInsertDbgValueRecordBefore;
use loom_codegen_ir::{
    BlockId, BlockTarget, BoolPredicate, CheckedArtifact, CheckedIntBinaryOp, Constant,
    ContractFaultMetadata, Effects, FaultCode, FaultMetadata, FloatBinaryOp,
    FloatPredicate as LcirFloatPredicate, Function, InstanceId, Instruction, InstructionKind,
    IntPredicate as LcirIntPredicate, Origin, Repr, ResultTarget, ScalarRepr, SumRepr, SumTagRepr,
    Terminator, TerminatorKind, TestOutcomePlan, UnwindTarget, ValueId, ValueTypeId,
};
use loom_core::runtime_fault::{INTEGER_OVERFLOW_FAULT_CODE, INTEGER_OVERFLOW_FAULT_MESSAGE};
use loom_runtime_abi::{
    TEXT_CONTAINS_SYMBOL, TEXT_LAYOUT_SYMBOL, TEXT_OBJECT_ALIGNMENT, TEXT_OBJECT_FIELD_BYTE_LENGTH,
    TEXT_OBJECT_FIELD_BYTES, TEXT_OBJECT_FIELD_SCALAR_LENGTH, TEXT_OBJECT_HEADER_SIZE,
};

use crate::CodegenError;
use crate::codegen::{DebugSource, NativeObjectArtifact, NativeObjectOptions};
use crate::target::{
    NativeTargetMachine, configure_debug_module_flags, create_llvm_target_machine,
};

pub(crate) struct LcirEmitter;

impl LcirEmitter {
    pub(crate) fn emit_object(
        artifact: &CheckedArtifact,
        output: &Path,
        options: &NativeObjectOptions,
    ) -> Result<NativeObjectArtifact, CodegenError> {
        let target =
            create_llvm_target_machine(options.target_triple.as_deref(), options.optimization)?;
        Self::emit_object_with_target(artifact, output, options, &target)
    }

    pub(crate) fn emit_object_with_target(
        artifact: &CheckedArtifact,
        output: &Path,
        options: &NativeObjectOptions,
        target: &NativeTargetMachine,
    ) -> Result<NativeObjectArtifact, CodegenError> {
        let llvm_pointer_bits = target.pointer_bits()?;
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
        let backend = Backend::new(&context, artifact, options, target)?;
        backend.compile()?;
        backend.finalize_debug();
        verify(&backend.module)?;
        backend
            .module
            .run_passes(
                target.optimization().pipeline(),
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
    files: BTreeMap<u32, DIFile<'ctx>>,
    sources: BTreeMap<u32, DebugSource>,
    type_file: DIFile<'ctx>,
    unit_type: DIType<'ctx>,
    bool_type: DIType<'ctx>,
    int_type: DIType<'ctx>,
    float_type: DIType<'ctx>,
    text_type: DIType<'ctx>,
    status_type: DIType<'ctx>,
    fault_context_pointer_type: DIType<'ctx>,
    fallible_unit_type: DIType<'ctx>,
    fallible_bool_type: DIType<'ctx>,
    fallible_int_type: DIType<'ctx>,
    fallible_float_type: DIType<'ctx>,
    product_types: RefCell<BTreeMap<u32, DIType<'ctx>>>,
    sum_types: RefCell<BTreeMap<u32, DIType<'ctx>>>,
    optimized: bool,
}

impl<'ctx> DebugState<'ctx> {
    #[expect(
        clippy::too_many_lines,
        reason = "one constructor must publish the mutually referential compile unit, exact target ABI types, and source-id tables before any function metadata"
    )]
    fn new(
        context: &'ctx Context,
        module: &Module<'ctx>,
        sources: &[DebugSource],
        optimized: bool,
        target_data: &TargetData,
        ptr_type: inkwell::types::PointerType<'ctx>,
        fault_context_type: StructType<'ctx>,
    ) -> Result<Self, CodegenError> {
        let primary = sources
            .first()
            .map_or("<loom-generated>.loom", |source| source.path.as_str());
        module.set_source_file_name(primary);
        let target_triple = module.get_triple();
        configure_debug_module_flags(context, module, &target_triple.as_str().to_string_lossy());
        let (builder, unit) = module.create_debug_info_builder(
            true,
            DWARFSourceLanguage::C,
            primary,
            ".",
            concat!("loomc lcir ", env!("CARGO_PKG_VERSION")),
            optimized,
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
        let mut files = BTreeMap::new();
        let mut source_map = BTreeMap::new();
        for (index, source) in sources.iter().enumerate() {
            if files.contains_key(&source.file) {
                return Err(CodegenError::new(
                    "LlvmDebugInfoFailed",
                    format!("duplicate debug source file id #{}", source.file),
                ));
            }
            let file = if index == 0 {
                unit.get_file()
            } else {
                builder.create_file(&source.path, ".")
            };
            files.insert(source.file, file);
            source_map.insert(source.file, source.clone());
        }
        let file = unit.get_file();
        let unit_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "Unit",
                file,
                0,
                0,
                8,
                DIFlags::PUBLIC,
                None,
                &[],
                0,
                None,
                "loom.Unit",
            )
            .as_type();
        let bool_type = builder
            .create_basic_type("Bool", 1, 0x02, DIFlags::PUBLIC)
            .map_err(|error| debug_info_error(&error))?
            .as_type();
        let int_type = builder
            .create_basic_type("Int", 64, 0x05, DIFlags::PUBLIC)
            .map_err(|error| debug_info_error(&error))?
            .as_type();
        let float_type = builder
            .create_basic_type("Float", 64, 0x04, DIFlags::PUBLIC)
            .map_err(|error| debug_info_error(&error))?
            .as_type();
        let text_alignment_bits = u32::try_from(TEXT_OBJECT_ALIGNMENT.saturating_mul(8))
            .map_err(|_| CodegenError::new("LlvmDebugInfoFailed", "Text alignment is too wide"))?;
        let text_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "TextObject",
                file,
                0,
                TEXT_OBJECT_HEADER_SIZE.saturating_mul(8),
                text_alignment_bits,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.TextObject",
            )
            .as_type();
        let text_type = builder
            .create_pointer_type(
                "Text",
                text_object_type,
                target_data.get_bit_size(&ptr_type),
                abi_alignment_bits(target_data, &ptr_type)?,
                AddressSpace::default(),
            )
            .as_type();
        let status_type = builder
            .create_basic_type("LoomStatus", 32, 0x05, DIFlags::ARTIFICIAL)
            .map_err(|error| debug_info_error(&error))?
            .as_type();
        let fault_context = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "LoomFaultContext",
                file,
                0,
                target_data.get_bit_size(&fault_context_type),
                abi_alignment_bits(target_data, &fault_context_type)?,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.LoomFaultContext",
            )
            .as_type();
        let fault_context_pointer_type = builder
            .create_pointer_type(
                "LoomFaultContext*",
                fault_context,
                target_data.get_bit_size(&ptr_type),
                abi_alignment_bits(target_data, &ptr_type)?,
                AddressSpace::default(),
            )
            .as_type();
        let fallible_unit_type = create_fallible_debug_type(
            context,
            &builder,
            file,
            target_data,
            "Unit",
            unit_type,
            context.struct_type(&[], false).into(),
            status_type,
        )?;
        let fallible_bool_type = create_fallible_debug_type(
            context,
            &builder,
            file,
            target_data,
            "Bool",
            bool_type,
            context.bool_type().into(),
            status_type,
        )?;
        let fallible_int_type = create_fallible_debug_type(
            context,
            &builder,
            file,
            target_data,
            "Int",
            int_type,
            context.i64_type().into(),
            status_type,
        )?;
        let fallible_float_type = create_fallible_debug_type(
            context,
            &builder,
            file,
            target_data,
            "Float",
            float_type,
            context.f64_type().into(),
            status_type,
        )?;
        Ok(Self {
            builder,
            files,
            sources: source_map,
            type_file: file,
            unit_type,
            bool_type,
            int_type,
            float_type,
            text_type,
            status_type,
            fault_context_pointer_type,
            fallible_unit_type,
            fallible_bool_type,
            fallible_int_type,
            fallible_float_type,
            product_types: RefCell::new(BTreeMap::new()),
            sum_types: RefCell::new(BTreeMap::new()),
            optimized,
        })
    }

    fn file(&self, id: u32) -> Result<DIFile<'ctx>, CodegenError> {
        self.files.get(&id).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("debug source table does not contain file id #{id}"),
            )
        })
    }

    fn line_column(&self, file: u32, offset: u32) -> Result<(u32, u32), CodegenError> {
        let source = self.sources.get(&file).ok_or_else(|| {
            CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("debug source table does not contain file id #{file}"),
            )
        })?;
        let line = source
            .line_starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1);
        let start = source.line_starts.get(line).copied().unwrap_or(0);
        Ok((
            u32::try_from(line).unwrap_or(u32::MAX).saturating_add(1),
            offset.saturating_sub(start).saturating_add(1),
        ))
    }

    fn value_type(
        &self,
        backend: &Backend<'ctx, '_>,
        ty: ValueTypeId,
    ) -> Result<DIType<'ctx>, CodegenError> {
        self.value_type_with_stack(backend, ty, &mut BTreeSet::new())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the exhaustive debug-type mapping keeps every physical LCIR representation and recursive guard in one audited boundary"
    )]
    fn value_type_with_stack(
        &self,
        backend: &Backend<'ctx, '_>,
        ty: ValueTypeId,
        visiting: &mut BTreeSet<u32>,
    ) -> Result<DIType<'ctx>, CodegenError> {
        let value_type = backend
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| {
                CodegenError::new("LlvmDebugInfoFailed", format!("missing LCIR type {ty}"))
            })?;
        match backend
            .artifact
            .representations()
            .repr(value_type.repr())
            .copied()
        {
            Some(Repr::Zst) => Ok(self.unit_type),
            Some(Repr::Scalar(ScalarRepr::I1)) => Ok(self.bool_type),
            Some(Repr::Scalar(ScalarRepr::I64)) => Ok(self.int_type),
            Some(Repr::Scalar(ScalarRepr::F64)) => Ok(self.float_type),
            Some(Repr::ImmortalText) => Ok(self.text_type),
            Some(Repr::Product(product)) => {
                if let Some(existing) = self.product_types.borrow().get(&ty.raw()).copied() {
                    return Ok(existing);
                }
                if !visiting.insert(ty.raw()) {
                    return Err(CodegenError::new(
                        "LlvmDebugInfoFailed",
                        format!("cyclic LCIR product type {ty} reached debug emission"),
                    ));
                }
                let fields = backend
                    .artifact
                    .representations()
                    .product(product)
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmDebugInfoFailed",
                            format!("missing LCIR product representation {product}"),
                        )
                    })?
                    .fields()
                    .to_vec();
                let physical_type = backend.llvm_type(ty)?.into_struct_type();
                let mut members = Vec::with_capacity(fields.len());
                for (index, field) in fields.into_iter().enumerate() {
                    members.push(DebugAggregateField {
                        name: format!("field{index}"),
                        debug_type: self.value_type_with_stack(backend, field, visiting)?,
                        llvm_type: backend.llvm_type(field)?,
                        flags: DIFlags::ZERO,
                    });
                }
                visiting.remove(&ty.raw());
                let name = format!("LoomProduct<t{}>", ty.raw());
                let identifier = format!("loom.compiler.LoomProduct.t{}", ty.raw());
                let debug_type = create_aggregate_debug_type(
                    &self.builder,
                    self.type_file,
                    &backend.target_data,
                    &name,
                    &identifier,
                    physical_type,
                    &members,
                    DIFlags::ARTIFICIAL | DIFlags::TYPE_PASS_BY_VALUE,
                )?;
                self.product_types.borrow_mut().insert(ty.raw(), debug_type);
                Ok(debug_type)
            }
            Some(Repr::Sum(_)) => {
                if let Some(existing) = self.sum_types.borrow().get(&ty.raw()).copied() {
                    return Ok(existing);
                }
                if !visiting.insert(ty.raw()) {
                    return Err(CodegenError::new(
                        "LlvmDebugInfoFailed",
                        format!("cyclic LCIR sum type {ty} reached debug emission"),
                    ));
                }
                let sum = backend.sum_repr(ty)?;
                let layout = backend.sum_layout(ty)?;
                let name = format!("LoomSum<t{}>", ty.raw());
                let identifier = format!("loom.compiler.LoomSum.t{}", ty.raw());
                let debug_type = match layout.tag {
                    SumTagRepr::Tagless => {
                        let variant = sum.variants().first().ok_or_else(|| {
                            CodegenError::new(
                                "LlvmDebugInfoFailed",
                                format!("tagless LCIR sum type {ty} has no variant"),
                            )
                        })?;
                        let mut members = Vec::with_capacity(variant.fields().len());
                        for (index, field) in variant.fields().iter().copied().enumerate() {
                            members.push(DebugAggregateField {
                                name: format!("variant0.field{index}"),
                                debug_type: self.value_type_with_stack(backend, field, visiting)?,
                                llvm_type: backend.llvm_type(field)?,
                                flags: DIFlags::ZERO,
                            });
                        }
                        create_aggregate_debug_type(
                            &self.builder,
                            self.type_file,
                            &backend.target_data,
                            &name,
                            &identifier,
                            layout.physical.into_struct_type(),
                            &members,
                            DIFlags::ARTIFICIAL | DIFlags::TYPE_PASS_BY_VALUE,
                        )?
                    }
                    SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 if sum.is_tag_only() => self
                        .builder
                        .create_basic_type(
                            &name,
                            backend.target_data.get_bit_size(&layout.physical),
                            0x07,
                            DIFlags::ARTIFICIAL | DIFlags::TYPE_PASS_BY_VALUE,
                        )
                        .map_err(|error| debug_info_error(&error))?
                        .as_type(),
                    SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                        let tag_type = backend.sum_tag_type(layout.tag).ok_or_else(|| {
                            CodegenError::new(
                                "LlvmDebugInfoFailed",
                                format!("LCIR sum type {ty} has no tag type"),
                            )
                        })?;
                        let tag_debug_type = self
                            .builder
                            .create_basic_type(
                                &format!("{name}.tag"),
                                backend.target_data.get_bit_size(&tag_type),
                                0x07,
                                DIFlags::ARTIFICIAL,
                            )
                            .map_err(|error| debug_info_error(&error))?
                            .as_type();
                        let carrier = layout.carrier.ok_or_else(|| {
                            CodegenError::new(
                                "LlvmDebugInfoFailed",
                                format!("tagged LCIR sum type {ty} has no carrier"),
                            )
                        })?;
                        let carrier_debug_type = self
                            .builder
                            .create_struct_type(
                                self.type_file.as_debug_info_scope(),
                                &format!("{name}.carrier"),
                                self.type_file,
                                0,
                                backend.target_data.get_bit_size(&carrier),
                                abi_alignment_bits(&backend.target_data, &carrier)?,
                                DIFlags::ARTIFICIAL,
                                None,
                                &[],
                                0,
                                None,
                                &format!("{identifier}.carrier"),
                            )
                            .as_type();
                        let members = [
                            DebugAggregateField {
                                name: "tag".into(),
                                debug_type: tag_debug_type,
                                llvm_type: tag_type.into(),
                                flags: DIFlags::ARTIFICIAL,
                            },
                            DebugAggregateField {
                                name: "carrier".into(),
                                debug_type: carrier_debug_type,
                                llvm_type: carrier.into(),
                                flags: DIFlags::ARTIFICIAL,
                            },
                        ];
                        create_aggregate_debug_type(
                            &self.builder,
                            self.type_file,
                            &backend.target_data,
                            &name,
                            &identifier,
                            layout.physical.into_struct_type(),
                            &members,
                            DIFlags::ARTIFICIAL | DIFlags::TYPE_PASS_BY_VALUE,
                        )?
                    }
                };
                visiting.remove(&ty.raw());
                self.sum_types.borrow_mut().insert(ty.raw(), debug_type);
                Ok(debug_type)
            }
            Some(Repr::Uninhabited) => Err(CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("uninhabited LCIR type {ty} reached a debug signature"),
            )),
            None => Err(CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("missing representation for LCIR type {ty}"),
            )),
        }
    }

    fn fallible_type(
        &self,
        backend: &Backend<'ctx, '_>,
        ty: ValueTypeId,
    ) -> Result<DIType<'ctx>, CodegenError> {
        let value_type = backend
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| {
                CodegenError::new("LlvmDebugInfoFailed", format!("missing LCIR type {ty}"))
            })?;
        match backend
            .artifact
            .representations()
            .repr(value_type.repr())
            .copied()
        {
            Some(Repr::Zst) => Ok(self.fallible_unit_type),
            Some(Repr::Scalar(ScalarRepr::I1)) => Ok(self.fallible_bool_type),
            Some(Repr::Scalar(ScalarRepr::I64)) => Ok(self.fallible_int_type),
            Some(Repr::Scalar(ScalarRepr::F64)) => Ok(self.fallible_float_type),
            Some(Repr::ImmortalText) => create_fallible_debug_type(
                backend.context,
                &self.builder,
                self.type_file,
                &backend.target_data,
                "Text",
                self.text_type,
                backend.ptr_type.into(),
                self.status_type,
            ),
            Some(Repr::Product(_)) => create_fallible_debug_type(
                backend.context,
                &self.builder,
                self.type_file,
                &backend.target_data,
                &format!("LoomProduct<t{}>", ty.raw()),
                self.value_type(backend, ty)?,
                backend.llvm_type(ty)?,
                self.status_type,
            ),
            Some(Repr::Sum(_)) => create_fallible_debug_type(
                backend.context,
                &self.builder,
                self.type_file,
                &backend.target_data,
                &format!("LoomSum<t{}>", ty.raw()),
                self.value_type(backend, ty)?,
                backend.llvm_type(ty)?,
                self.status_type,
            ),
            Some(Repr::Uninhabited) => Err(CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("uninhabited LCIR type {ty} reached a fallible debug signature"),
            )),
            None => Err(CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("missing representation for LCIR type {ty}"),
            )),
        }
    }

    fn attach_function(
        &self,
        backend: &Backend<'ctx, '_>,
        source: &Function,
        function: FunctionValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let origin = source.origin();
        let file_id = origin.span.file.0;
        let file = self.file(file_id)?;
        let (line, _) = self.line_column(file_id, origin.span.range.start)?;
        let result = self.function_result_type(backend, source)?;
        let mut parameters = source
            .signature()
            .params()
            .iter()
            .copied()
            .map(|ty| self.value_type(backend, ty))
            .collect::<Result<Vec<_>, _>>()?;
        if source.effects().contains(Effects::MAY_FAULT) {
            parameters.push(self.fault_context_pointer_type);
        }
        // This describes the exact callable ABI: direct results stay direct,
        // inout writebacks extend the physical return aggregate, and fallible
        // returns prepend status. Hidden status/writeback fields and the
        // fault-context parameter are marked artificial.
        let signature =
            self.builder
                .create_subroutine_type(file, Some(result), &parameters, DIFlags::PUBLIC);
        let linkage = function.get_name().to_string_lossy();
        let subprogram = self.builder.create_function(
            file.as_debug_info_scope(),
            source.name(),
            Some(linkage.as_ref()),
            file,
            line,
            signature,
            true,
            true,
            line,
            DIFlags::PUBLIC,
            self.optimized,
        );
        function.set_subprogram(subprogram);
        Ok(())
    }

    fn function_result_type(
        &self,
        backend: &Backend<'ctx, '_>,
        source: &Function,
    ) -> Result<DIType<'ctx>, CodegenError> {
        let writebacks = Backend::signature_writeback_types(source)?;
        if writebacks.is_empty() {
            return if source.effects().contains(Effects::MAY_FAULT) {
                self.fallible_type(backend, source.signature().result())
            } else {
                self.value_type(backend, source.signature().result())
            };
        }

        let mut logical_types = Vec::with_capacity(1 + writebacks.len());
        logical_types.push(source.signature().result());
        logical_types.extend(writebacks.iter().copied());
        let fallible = source.effects().contains(Effects::MAY_FAULT);
        let mut fields = Vec::with_capacity(logical_types.len() + usize::from(fallible));
        if fallible {
            fields.push(DebugAggregateField {
                name: "status".into(),
                debug_type: self.status_type,
                llvm_type: backend.context.i32_type().into(),
                flags: DIFlags::ARTIFICIAL,
            });
        }
        for (index, ty) in logical_types.iter().copied().enumerate() {
            fields.push(DebugAggregateField {
                name: if index == 0 {
                    "value".into()
                } else {
                    format!("writeback{}", index - 1)
                },
                debug_type: self.value_type(backend, ty)?,
                llvm_type: backend.llvm_type(ty)?,
                flags: if index == 0 {
                    DIFlags::ZERO
                } else {
                    DIFlags::ARTIFICIAL
                },
            });
        }
        let physical_fields = fields
            .iter()
            .map(|field| field.llvm_type)
            .collect::<Vec<_>>();
        let physical_type = backend.context.struct_type(&physical_fields, false);
        let result = source.signature().result().raw();
        let writeback_count = writebacks.len();
        let writeback_names = writebacks
            .iter()
            .map(|ty| format!("t{}", ty.raw()))
            .collect::<Vec<_>>()
            .join(",");
        let writeback_identity = writebacks
            .iter()
            .map(|ty| ty.raw().to_string())
            .collect::<Vec<_>>()
            .join(".t");
        let name = if fallible {
            format!("LoomFallibleInOut<t{result};writebacks=[{writeback_names}]>")
        } else {
            format!("LoomInOut<t{result};writebacks=[{writeback_names}]>")
        };
        let identifier = format!(
            "loom.compiler.LoomReturn.{}.result.t{result}.writebacks.{writeback_count}.t{writeback_identity}",
            if fallible { "fallible" } else { "inout" }
        );
        create_aggregate_debug_type(
            &self.builder,
            self.type_file,
            &backend.target_data,
            &name,
            &identifier,
            physical_type,
            &fields,
            DIFlags::ARTIFICIAL | DIFlags::TYPE_PASS_BY_VALUE,
        )
    }

    fn attach_parameter_values(
        &self,
        backend: &Backend<'ctx, '_>,
        source: &Function,
        function: FunctionValue<'ctx>,
        entry: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let scope = function.get_subprogram().ok_or_else(|| {
            CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("{} has no debug subprogram", source.id()),
            )
        })?;
        // A checked LCIR entry always has a terminator, so even an entry with
        // no ordinary instructions has a stable LLVM insertion point.
        let first = entry.get_first_instruction().ok_or_else(|| {
            CodegenError::new(
                "LlvmDebugInfoFailed",
                format!("{} has an empty LLVM entry block", source.id()),
            )
        })?;
        let origin = source.origin();
        let file_id = origin.span.file.0;
        let file = self.file(file_id)?;
        let (line, column) = self.line_column(file_id, origin.span.range.start)?;
        let location = self.builder.create_debug_location(
            backend.context,
            line,
            column,
            scope.as_debug_info_scope(),
            None,
        );

        for (index, ty) in source.signature().params().iter().copied().enumerate() {
            let llvm_index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let argument_number = llvm_index
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = function.get_nth_param(llvm_index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing LLVM parameter {llvm_index}", source.id()),
                )
            })?;
            let variable = self.builder.create_parameter_variable(
                scope.as_debug_info_scope(),
                &format!("arg{index}"),
                argument_number,
                file,
                line,
                self.value_type(backend, ty)?,
                true,
                DIFlags::ZERO,
            );
            insert_dbg_value_before(&self.builder, value, variable, location, first);
        }

        if source.effects().contains(Effects::MAY_FAULT) {
            let llvm_index = u32::try_from(source.signature().params().len())
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let argument_number = llvm_index
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = function.get_nth_param(llvm_index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing its fault-context pointer", source.id()),
                )
            })?;
            let variable = self.builder.create_parameter_variable(
                scope.as_debug_info_scope(),
                "__loom_fault_context",
                argument_number,
                file,
                line,
                self.fault_context_pointer_type,
                true,
                DIFlags::ARTIFICIAL,
            );
            insert_dbg_value_before(&self.builder, value, variable, location, first);
        }
        Ok(())
    }

    fn set_location(
        &self,
        context: &'ctx Context,
        ir_builder: &Builder<'ctx>,
        function: FunctionValue<'ctx>,
        origin: Origin,
    ) -> Result<(), CodegenError> {
        let scope = function.get_subprogram().ok_or_else(|| {
            CodegenError::new(
                "LlvmDebugInfoFailed",
                "LLVM function has no debug subprogram",
            )
        })?;
        let file = origin.span.file.0;
        self.file(file)?;
        let (line, column) = self.line_column(file, origin.span.range.start)?;
        let location = self.builder.create_debug_location(
            context,
            line,
            column,
            scope.as_debug_info_scope(),
            None,
        );
        ir_builder.set_current_debug_location(location);
        Ok(())
    }
}

struct DebugAggregateField<'ctx> {
    name: String,
    debug_type: DIType<'ctx>,
    llvm_type: BasicTypeEnum<'ctx>,
    flags: DIFlags,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the complete physical LLVM aggregate and its DWARF identity are explicit inputs to target-data-checked metadata construction"
)]
fn create_aggregate_debug_type<'ctx>(
    builder: &DebugInfoBuilder<'ctx>,
    file: DIFile<'ctx>,
    target_data: &TargetData,
    name: &str,
    identifier: &str,
    physical_type: StructType<'ctx>,
    fields: &[DebugAggregateField<'ctx>],
    flags: DIFlags,
) -> Result<DIType<'ctx>, CodegenError> {
    if physical_type.count_fields() as usize != fields.len() {
        return Err(CodegenError::new(
            "LlvmDebugInfoFailed",
            format!(
                "debug aggregate {name} has {} metadata field(s) but {} LLVM field(s)",
                fields.len(),
                physical_type.count_fields()
            ),
        ));
    }
    let mut members = Vec::with_capacity(fields.len());
    for (index, field) in fields.iter().enumerate() {
        let index = u32::try_from(index)
            .map_err(|_| CodegenError::new("ProgramTooLarge", "too many debug aggregate fields"))?;
        let offset = target_data
            .offset_of_element(&physical_type, index)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmDebugInfoFailed",
                    format!("missing physical offset for {name}.{}", field.name),
                )
            })?;
        members.push(
            builder
                .create_member_type(
                    file.as_debug_info_scope(),
                    &field.name,
                    file,
                    0,
                    target_data.get_bit_size(&field.llvm_type),
                    abi_alignment_bits(target_data, &field.llvm_type)?,
                    byte_offset_bits(offset)?,
                    field.flags,
                    field.debug_type,
                )
                .as_type(),
        );
    }
    Ok(builder
        .create_struct_type(
            file.as_debug_info_scope(),
            name,
            file,
            0,
            target_data.get_bit_size(&physical_type),
            abi_alignment_bits(target_data, &physical_type)?,
            flags,
            None,
            &members,
            0,
            None,
            identifier,
        )
        .as_type())
}

#[expect(
    clippy::too_many_arguments,
    reason = "all LLVM and DI type handles are explicit so target layout cannot be inferred or taken from ambient state"
)]
fn create_fallible_debug_type<'ctx>(
    context: &'ctx Context,
    builder: &DebugInfoBuilder<'ctx>,
    file: DIFile<'ctx>,
    target_data: &TargetData,
    result_name: &str,
    result_debug_type: DIType<'ctx>,
    result_llvm_type: BasicTypeEnum<'ctx>,
    status_debug_type: DIType<'ctx>,
) -> Result<DIType<'ctx>, CodegenError> {
    let status_llvm_type = context.i32_type();
    let physical_type = context.struct_type(&[status_llvm_type.into(), result_llvm_type], false);
    let status_offset = target_data
        .offset_of_element(&physical_type, 0)
        .ok_or_else(|| CodegenError::new("LlvmDebugInfoFailed", "missing fallible status field"))?;
    let value_offset = target_data
        .offset_of_element(&physical_type, 1)
        .ok_or_else(|| CodegenError::new("LlvmDebugInfoFailed", "missing fallible value field"))?;
    let status_member = builder
        .create_member_type(
            file.as_debug_info_scope(),
            "status",
            file,
            0,
            target_data.get_bit_size(&status_llvm_type),
            abi_alignment_bits(target_data, &status_llvm_type)?,
            byte_offset_bits(status_offset)?,
            DIFlags::ARTIFICIAL,
            status_debug_type,
        )
        .as_type();
    let value_member = builder
        .create_member_type(
            file.as_debug_info_scope(),
            "value",
            file,
            0,
            target_data.get_bit_size(&result_llvm_type),
            abi_alignment_bits(target_data, &result_llvm_type)?,
            byte_offset_bits(value_offset)?,
            DIFlags::ZERO,
            result_debug_type,
        )
        .as_type();
    let name = format!("LoomFallible<{result_name}>");
    Ok(builder
        .create_struct_type(
            file.as_debug_info_scope(),
            &name,
            file,
            0,
            target_data.get_bit_size(&physical_type),
            abi_alignment_bits(target_data, &physical_type)?,
            DIFlags::ARTIFICIAL | DIFlags::TYPE_PASS_BY_VALUE,
            None,
            &[status_member, value_member],
            0,
            None,
            &format!("loom.compiler.LoomFallible.{result_name}"),
        )
        .as_type())
}

fn abi_alignment_bits(target_data: &TargetData, ty: &dyn AnyType) -> Result<u32, CodegenError> {
    target_data
        .get_abi_alignment(ty)
        .checked_mul(8)
        .ok_or_else(|| CodegenError::new("LlvmDebugInfoFailed", "debug alignment exceeds u32"))
}

fn byte_offset_bits(offset: u64) -> Result<u64, CodegenError> {
    offset
        .checked_mul(8)
        .ok_or_else(|| CodegenError::new("LlvmDebugInfoFailed", "debug field offset exceeds u64"))
}

#[expect(
    unsafe_code,
    reason = "Inkwell 0.10 wraps LLVM 19 DbgRecord pointers as InstructionValue and panics in debug builds; this calls the same typed LLVM C API and discards its opaque record"
)]
fn insert_dbg_value_before<'ctx>(
    builder: &DebugInfoBuilder<'ctx>,
    value: BasicValueEnum<'ctx>,
    variable: DILocalVariable<'ctx>,
    location: DILocation<'ctx>,
    instruction: inkwell::values::InstructionValue<'ctx>,
) {
    let expression = builder.create_expression(Vec::new());
    // SAFETY: every handle belongs to the same live LLVM context and DIBuilder.
    // `instruction` is in the emitted function's entry block, `value` is one
    // of that function's physical parameters, and LLVM owns the returned
    // DbgRecord.
    unsafe {
        LLVMDIBuilderInsertDbgValueRecordBefore(
            builder.as_mut_ptr(),
            value.as_value_ref(),
            variable.as_mut_ptr(),
            expression.as_mut_ptr(),
            location.as_mut_ptr(),
            instruction.as_value_ref(),
        );
    }
}

fn debug_info_error(error: &inkwell::error::Error) -> CodegenError {
    CodegenError::new("LlvmDebugInfoFailed", error.to_string())
}

#[derive(Clone)]
struct SumLayout<'ctx> {
    tag: SumTagRepr,
    payloads: Vec<StructType<'ctx>>,
    carrier: Option<StructType<'ctx>>,
    physical: BasicTypeEnum<'ctx>,
}

struct Backend<'ctx, 'artifact> {
    context: &'ctx Context,
    artifact: &'artifact CheckedArtifact,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    ptr_type: inkwell::types::PointerType<'ctx>,
    unit_type: StructType<'ctx>,
    fault_context_type: StructType<'ctx>,
    text_object_type: StructType<'ctx>,
    target_data: TargetData,
    functions: Vec<FunctionValue<'ctx>>,
    debug: Option<DebugState<'ctx>>,
    names: Cell<u64>,
}

impl<'ctx, 'artifact> Backend<'ctx, 'artifact> {
    fn new(
        context: &'ctx Context,
        artifact: &'artifact CheckedArtifact,
        options: &'artifact NativeObjectOptions,
        target: &NativeTargetMachine,
    ) -> Result<Self, CodegenError> {
        let module = context.create_module("loom.lcir.program");
        let target_data = target.machine.get_target_data();
        module.set_triple(&target.triple);
        module.set_data_layout(&target_data.get_data_layout());
        let builder = context.create_builder();
        let ptr_type = context.ptr_type(AddressSpace::default());
        let unit_type = context.struct_type(&[], false);
        let fault_context_type = context.opaque_struct_type("loom.lcir.FaultContext");
        fault_context_type.set_body(&[ptr_type.into(), context.bool_type().into()], false);
        let text_object_type = context.struct_type(
            &[
                ptr_type.into(),
                context.i64_type().into(),
                context.i64_type().into(),
                context.i64_type().into(),
                context.i8_type().array_type(0).into(),
            ],
            false,
        );
        if artifact
            .representations()
            .reprs()
            .contains(&Repr::ImmortalText)
        {
            let actual_size = target_data.get_abi_size(&text_object_type);
            let actual_alignment = u64::from(target_data.get_abi_alignment(&text_object_type));
            if actual_size != TEXT_OBJECT_HEADER_SIZE || actual_alignment != TEXT_OBJECT_ALIGNMENT {
                return Err(CodegenError::new(
                    "LcirTextAbiMismatch",
                    format!(
                        "LLVM target {} gives the runtime Text header size/alignment {actual_size}/{actual_alignment}, expected {TEXT_OBJECT_HEADER_SIZE}/{TEXT_OBJECT_ALIGNMENT}",
                        target.triple
                    ),
                ));
            }
        }
        let debug = (!options.debug_sources.is_empty())
            .then(|| {
                DebugState::new(
                    context,
                    &module,
                    &options.debug_sources,
                    options.optimization == crate::OptimizationProfile::Release,
                    &target_data,
                    ptr_type,
                    fault_context_type,
                )
            })
            .transpose()?;
        let mut backend = Self {
            context,
            artifact,
            module,
            builder,
            ptr_type,
            unit_type,
            fault_context_type,
            text_object_type,
            target_data,
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
            let results = self.logical_result_types(source)?;
            let function_type = if source.effects().contains(Effects::MAY_FAULT) {
                let mut fields = Vec::with_capacity(1 + results.len());
                fields.push(self.context.i32_type().into());
                fields.extend(results);
                self.context
                    .struct_type(&fields, false)
                    .fn_type(&params, false)
            } else if let [result] = results.as_slice() {
                result.fn_type(&params, false)
            } else {
                self.context
                    .struct_type(&results, false)
                    .fn_type(&params, false)
            };
            let function = self.module.add_function(
                &format!("loom.lcir.fn.{}", source.id().raw()),
                function_type,
                Some(Linkage::Internal),
            );
            if let Some(debug) = &self.debug {
                debug.attach_function(self, source, function)?;
            }
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
            Some(Repr::ImmortalText) => Ok(self.ptr_type.into()),
            Some(Repr::Product(product)) => {
                let fields = self
                    .artifact
                    .representations()
                    .product(product)
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("missing LCIR product representation {product}"),
                        )
                    })?
                    .fields()
                    .iter()
                    .copied()
                    .map(|field| self.llvm_type(field))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.context.struct_type(&fields, false).into())
            }
            Some(Repr::Sum(_)) => Ok(self.sum_layout(ty)?.physical),
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

    fn sum_repr(&self, ty: ValueTypeId) -> Result<&SumRepr, CodegenError> {
        let value_type = self
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        let Repr::Sum(sum) = self
            .artifact
            .representations()
            .repr(value_type.repr())
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("missing representation for LCIR type {ty}"),
                )
            })?
        else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR type {ty} is not a sum"),
            ));
        };
        self.artifact.representations().sum(sum).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("missing LCIR sum representation {sum}"),
            )
        })
    }

    fn sum_tag_type(&self, tag: SumTagRepr) -> Option<IntType<'ctx>> {
        match tag {
            SumTagRepr::Tagless => None,
            SumTagRepr::I8 => Some(self.context.i8_type()),
            SumTagRepr::I16 => Some(self.context.i16_type()),
            SumTagRepr::I32 => Some(self.context.i32_type()),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "sum layout selection and its target-data size/alignment proof are intentionally one atomic computation"
    )]
    fn sum_layout(&self, ty: ValueTypeId) -> Result<SumLayout<'ctx>, CodegenError> {
        let sum = self.sum_repr(ty)?;
        let payloads = sum
            .variants()
            .iter()
            .map(|variant| {
                let fields = variant
                    .fields()
                    .iter()
                    .copied()
                    .map(|field| self.llvm_type(field))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(self.context.struct_type(&fields, false))
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        if sum.tag() == SumTagRepr::Tagless {
            let physical = payloads.first().copied().ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR sum type {ty} has no variants"),
                )
            })?;
            return Ok(SumLayout {
                tag: sum.tag(),
                payloads,
                carrier: None,
                physical: physical.into(),
            });
        }
        let tag_type = self.sum_tag_type(sum.tag()).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR sum type {ty} has no tag type"),
            )
        })?;
        if sum.is_tag_only() {
            return Ok(SumLayout {
                tag: sum.tag(),
                payloads,
                carrier: None,
                physical: tag_type.into(),
            });
        }

        let mut maximum_size = 0_u64;
        let mut anchor = None;
        let mut maximum_alignment = 0_u32;
        for payload in &payloads {
            let size = self.target_data.get_abi_size(payload);
            let alignment = self.target_data.get_abi_alignment(payload);
            maximum_size = maximum_size.max(size);
            if alignment > maximum_alignment {
                maximum_alignment = alignment;
                anchor = Some(*payload);
            }
        }
        let anchor = anchor.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR sum type {ty} has no payload ABI"),
            )
        })?;
        let carrier_bytes = u32::try_from(maximum_size).map_err(|_| {
            CodegenError::new(
                "ProgramTooLarge",
                format!("LCIR sum type {ty} carrier exceeds LLVM array limits"),
            )
        })?;
        let carrier = self.context.struct_type(
            &[
                anchor.array_type(0).into(),
                self.context.i8_type().array_type(carrier_bytes).into(),
            ],
            false,
        );
        let expected_size = maximum_size
            .checked_add(u64::from(maximum_alignment.saturating_sub(1)))
            .map(|size| size / u64::from(maximum_alignment) * u64::from(maximum_alignment))
            .ok_or_else(|| {
                CodegenError::new(
                    "ProgramTooLarge",
                    format!("LCIR sum type {ty} is too large"),
                )
            })?;
        let actual_size = self.target_data.get_abi_size(&carrier);
        let actual_alignment = self.target_data.get_abi_alignment(&carrier);
        if actual_size != expected_size || actual_alignment != maximum_alignment {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "LCIR sum type {ty} carrier has ABI size/alignment {actual_size}/{actual_alignment}, expected {expected_size}/{maximum_alignment}"
                ),
            ));
        }
        let physical = self
            .context
            .struct_type(&[tag_type.into(), carrier.into()], false)
            .into();
        Ok(SumLayout {
            tag: sum.tag(),
            payloads,
            carrier: Some(carrier),
            physical,
        })
    }

    fn signature_writeback_types(source: &Function) -> Result<Vec<ValueTypeId>, CodegenError> {
        source
            .signature()
            .inout_params()
            .iter()
            .map(|parameter| {
                usize::try_from(*parameter)
                    .ok()
                    .and_then(|index| source.signature().params().get(index))
                    .copied()
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!(
                                "{} has invalid inout parameter position {parameter}",
                                source.id()
                            ),
                        )
                    })
            })
            .collect()
    }

    fn logical_result_types(
        &self,
        source: &Function,
    ) -> Result<Vec<BasicTypeEnum<'ctx>>, CodegenError> {
        let mut results = Vec::with_capacity(1 + source.signature().inout_params().len());
        results.push(self.llvm_type(source.signature().result())?);
        for writeback in Self::signature_writeback_types(source)? {
            results.push(self.llvm_type(writeback)?);
        }
        Ok(results)
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

    fn text_layout(&self) -> inkwell::values::GlobalValue<'ctx> {
        self.module
            .get_global(TEXT_LAYOUT_SYMBOL)
            .unwrap_or_else(|| {
                let descriptor = self.context.struct_type(
                    &[
                        self.context.i32_type().into(),
                        self.context.i32_type().into(),
                        self.context.i64_type().into(),
                        self.context.i64_type().into(),
                        self.context.i64_type().into(),
                        self.context.i64_type().into(),
                        self.context.i32_type().into(),
                        self.context.i32_type().into(),
                    ],
                    false,
                );
                let layout = self.module.add_global(descriptor, None, TEXT_LAYOUT_SYMBOL);
                layout.set_linkage(Linkage::External);
                layout
            })
    }

    fn emit_text_literal(&self, utf8: &str) -> Result<PointerValue<'ctx>, CodegenError> {
        if utf8.len() > loom_codegen_ir::TEXT_LITERAL_MAX_BYTES {
            return Err(CodegenError::new(
                "TextLiteralTooLarge",
                "checked LCIR Text literal exceeds its byte budget",
            ));
        }
        let byte_length = u64::try_from(utf8.len()).map_err(|_| {
            CodegenError::new("TextLiteralTooLarge", "Text literal length exceeds u64")
        })?;
        let array_length = u32::try_from(utf8.len()).map_err(|_| {
            CodegenError::new(
                "TextLiteralTooLarge",
                "Text literal exceeds LLVM's constant array limit",
            )
        })?;
        let allocation_size = TEXT_OBJECT_HEADER_SIZE
            .checked_add(byte_length)
            .ok_or_else(|| {
                CodegenError::new(
                    "TextLiteralTooLarge",
                    "Text literal allocation size overflowed",
                )
            })?;
        let scalar_length = u64::try_from(utf8.chars().count()).map_err(|_| {
            CodegenError::new("TextLiteralTooLarge", "Text scalar count exceeds u64")
        })?;
        let bytes_type = self.context.i8_type().array_type(array_length);
        let literal_type = self.context.struct_type(
            &[
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                bytes_type.into(),
            ],
            false,
        );
        let bytes = utf8
            .as_bytes()
            .iter()
            .map(|byte| self.context.i8_type().const_int(u64::from(*byte), false))
            .collect::<Vec<_>>();
        let initializer = literal_type.const_named_struct(&[
            self.text_layout().as_pointer_value().into(),
            self.context
                .i64_type()
                .const_int(allocation_size, false)
                .into(),
            self.context.i64_type().const_int(byte_length, false).into(),
            self.context
                .i64_type()
                .const_int(scalar_length, false)
                .into(),
            self.context.i8_type().const_array(&bytes).into(),
        ]);
        let literal = self
            .module
            .add_global(literal_type, None, &self.unique("text.literal"));
        literal.set_initializer(&initializer);
        literal.set_constant(true);
        literal.set_linkage(Linkage::Private);
        literal.set_unnamed_address(UnnamedAddress::Global);
        let alignment = u32::try_from(TEXT_OBJECT_ALIGNMENT).map_err(|_| {
            CodegenError::new(
                "LcirTextAbiMismatch",
                "runtime Text object alignment exceeds LLVM's alignment domain",
            )
        })?;
        literal.set_alignment(alignment);
        Ok(literal.as_pointer_value())
    }

    fn text_field(
        &self,
        object: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.builder
            .build_struct_gep(self.text_object_type, object, field, name)
            .map_err(builder_error)
    }

    fn text_parts(
        &self,
        object: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let length_pointer = self.text_field(
            object,
            TEXT_OBJECT_FIELD_BYTE_LENGTH,
            &format!("{name}.length.pointer"),
        )?;
        let length = self
            .builder
            .build_load(
                self.context.i64_type(),
                length_pointer,
                &format!("{name}.length"),
            )
            .map_err(builder_error)?
            .into_int_value();
        let data = self.text_field(object, TEXT_OBJECT_FIELD_BYTES, &format!("{name}.bytes"))?;
        Ok((data, length))
    }

    fn text_scalar_length(
        &self,
        object: PointerValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let pointer = self.text_field(
            object,
            TEXT_OBJECT_FIELD_SCALAR_LENGTH,
            "text.scalar_length.pointer",
        )?;
        self.builder
            .build_load(self.context.i64_type(), pointer, "text.scalar_length")
            .map_err(builder_error)
            .map(BasicValueEnum::into_int_value)
    }

    fn runtime_text_contains(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TEXT_CONTAINS_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TEXT_CONTAINS_SYMBOL, function_type, None)
            })
    }
}

#[derive(Clone, Copy)]
enum FaultEmission<'metadata> {
    Runtime { code: FaultCode, origin: Origin },
    Contract(&'metadata ContractFaultMetadata),
}

struct FunctionEmitter<'backend, 'ctx, 'artifact> {
    backend: &'backend Backend<'ctx, 'artifact>,
    source: &'artifact Function,
    function: FunctionValue<'ctx>,
    blocks: Vec<BasicBlock<'ctx>>,
    emission_order: Vec<BlockId>,
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
        let emission_order = Self::compute_emission_order(source)?;
        let mut blocks = vec![None; source.blocks().len()];
        for block_id in emission_order.iter().copied() {
            blocks[block_id.index()] = Some(
                backend
                    .context
                    .append_basic_block(function, &format!("b{}", block_id.raw())),
            );
        }
        let blocks = blocks
            .into_iter()
            .enumerate()
            .map(|(index, block)| {
                block.ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("{} is missing LLVM block {index}", source.id()),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut emitter = Self {
            backend,
            source,
            function,
            blocks,
            emission_order,
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
        for (parameter_index, value_id) in entry_block.params().iter().copied().enumerate() {
            let index = u32::try_from(parameter_index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = self.function.get_nth_param(index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing LLVM parameter {index}", self.source.id()),
                )
            })?;
            if self.backend.debug.is_some() {
                value.set_name(&format!("arg{parameter_index}"));
            }
            self.values[value_id.index()] = Some(value);
        }
        if self.source.effects().contains(Effects::MAY_FAULT) {
            let index = u32::try_from(self.source.signature().params().len())
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = self.function.get_nth_param(index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing its fault-context pointer", self.source.id()),
                )
            })?;
            if self.backend.debug.is_some() {
                value.set_name("__loom_fault_context");
            }
            self.fault_context = Some(value.into_pointer_value());
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
        for index in 0..self.emission_order.len() {
            let block_id = self.emission_order[index];
            let block = self.source.block(block_id).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR emission order contains missing block {block_id}"),
                )
            })?;
            self.backend
                .builder
                .position_at_end(self.blocks[block_id.index()]);
            for instruction_id in block.instructions() {
                let instruction = self.source.instruction(*instruction_id).ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("missing LCIR instruction {instruction_id}"),
                    )
                })?;
                self.set_debug_location(instruction.origin())?;
                self.emit_instruction(instruction)?;
            }
            let terminator = block.terminator().ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR block {} is unterminated", block.id()),
                )
            })?;
            self.set_debug_location(terminator.origin())?;
            self.emit_terminator(terminator)?;
        }
        if let Some(debug) = &self.backend.debug {
            let entry = self.source.entry().ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} has no entry block", self.source.id()),
                )
            })?;
            debug.attach_parameter_values(
                self.backend,
                self.source,
                self.function,
                self.blocks[entry.index()],
            )?;
        }
        self.backend.builder.unset_current_debug_location();
        Ok(())
    }

    fn set_debug_location(&self, origin: Origin) -> Result<(), CodegenError> {
        if let Some(debug) = &self.backend.debug {
            debug.set_location(
                self.backend.context,
                &self.backend.builder,
                self.function,
                origin,
            )?;
        }
        Ok(())
    }

    /// Returns a deterministic CFG preorder rooted at the function entry.
    ///
    /// Checked LCIR constrains SSA uses by dominance, not by block-table
    /// insertion order. Every dominator is encountered before the blocks it
    /// dominates in a preorder rooted at entry, so all non-phi operands have an
    /// LLVM definition before they are consumed. The explicit stack also keeps
    /// large generated CFGs off the Rust call stack.
    fn compute_emission_order(source: &Function) -> Result<Vec<BlockId>, CodegenError> {
        let entry = source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", source.id()),
            )
        })?;
        let mut order = Vec::with_capacity(source.blocks().len());
        let mut seen = vec![false; source.blocks().len()];
        let mut pending = vec![entry];
        while let Some(block_id) = pending.pop() {
            let visited = seen.get_mut(block_id.index()).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} references missing block {block_id}", source.id()),
                )
            })?;
            if *visited {
                continue;
            }
            *visited = true;
            order.push(block_id);

            let block = source.block(block_id).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} references missing block {block_id}", source.id()),
                )
            })?;
            let terminator = block.terminator().ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR block {block_id} is unterminated"),
                )
            })?;
            // Push the secondary edge first so the primary/normal edge is
            // visited first when popped from the LIFO worklist.
            match terminator.kind() {
                TerminatorKind::Jump(target) => pending.push(target.block),
                TerminatorKind::Branch {
                    then_target,
                    else_target,
                    ..
                } => {
                    pending.push(else_target.block);
                    pending.push(then_target.block);
                }
                TerminatorKind::SumSwitch { cases, .. } => {
                    for case in cases.iter().rev() {
                        pending.push(case.block);
                    }
                }
                TerminatorKind::CheckedIntNegate { normal, fault, .. }
                | TerminatorKind::CheckedIntBinary { normal, fault, .. } => {
                    pending.push(fault.block);
                    pending.push(normal.block);
                }
                TerminatorKind::Invoke { normal, unwind, .. } => {
                    pending.push(unwind.block);
                    pending.push(normal.block);
                }
                TerminatorKind::Assert { success, fault, .. } => {
                    pending.push(fault.block);
                    pending.push(success.block);
                }
                TerminatorKind::Return(_)
                | TerminatorKind::Fault { .. }
                | TerminatorKind::ResumeFault => {}
            }
        }
        if order.len() != source.blocks().len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "{} has {} block(s) outside the entry CFG",
                    source.id(),
                    source.blocks().len() - order.len()
                ),
            ));
        }
        Ok(order)
    }

    #[allow(clippy::too_many_lines)]
    fn emit_instruction(&mut self, instruction: &Instruction) -> Result<(), CodegenError> {
        let one = |value: BasicValueEnum<'ctx>| vec![value];
        let values = match instruction.kind() {
            InstructionKind::Constant(constant) => one(self.emit_constant(*constant)?),
            InstructionKind::TextLiteral { utf8 } => {
                one(self.backend.emit_text_literal(utf8)?.into())
            }
            InstructionKind::TextLength { text } => one(self
                .backend
                .text_scalar_length(self.value(*text)?.into_pointer_value())?
                .into()),
            InstructionKind::TextContains { text, needle } => {
                let (data, length) = self.backend.text_parts(
                    self.value(*text)?.into_pointer_value(),
                    "text.contains.value",
                )?;
                let (needle_data, needle_length) = self.backend.text_parts(
                    self.value(*needle)?.into_pointer_value(),
                    "text.contains.needle",
                )?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_text_contains(),
                    &[
                        data.into(),
                        length.into(),
                        needle_data.into(),
                        needle_length.into(),
                    ],
                    "text.contains.status",
                )?;
                one(self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        status,
                        self.backend.context.i32_type().const_int(1, false),
                        "text.contains",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?)
            }
            InstructionKind::TextCompare {
                predicate,
                left,
                right,
            } => {
                let (left_data, left_length) = self
                    .backend
                    .text_parts(self.value(*left)?.into_pointer_value(), "text.compare.left")?;
                let (right_data, right_length) = self.backend.text_parts(
                    self.value(*right)?.into_pointer_value(),
                    "text.compare.right",
                )?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_text_contains(),
                    &[
                        left_data.into(),
                        left_length.into(),
                        right_data.into(),
                        right_length.into(),
                    ],
                    "text.compare.contains.status",
                )?;
                let contained = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        status,
                        self.backend.context.i32_type().const_int(1, false),
                        "text.compare.contains",
                    )
                    .map_err(builder_error)?;
                let same_length = self
                    .backend
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        left_length,
                        right_length,
                        "text.compare.same_length",
                    )
                    .map_err(builder_error)?;
                let equal = self
                    .backend
                    .builder
                    .build_and(contained, same_length, "text.compare.equal")
                    .map_err(builder_error)?;
                let compared = match predicate {
                    BoolPredicate::Equal => equal,
                    BoolPredicate::NotEqual => self
                        .backend
                        .builder
                        .build_not(equal, "text.compare.not_equal")
                        .map_err(builder_error)?,
                };
                one(compared.into())
            }
            InstructionKind::ProductConstruct { fields }
            | InstructionKind::InvariantRecordProven { fields } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("{} product construction has no result", instruction.id()),
                    )
                })?;
                let ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", format!("missing result {result}"))
                    })?
                    .ty();
                let mut aggregate = self.backend.llvm_type(ty)?.into_struct_type().get_undef();
                for (index, field) in fields.iter().copied().enumerate() {
                    let index = u32::try_from(index).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "too many LCIR product fields")
                    })?;
                    aggregate = self
                        .backend
                        .builder
                        .build_insert_value(
                            aggregate,
                            self.value(field)?,
                            index,
                            "product.construct",
                        )
                        .map_err(builder_error)?
                        .into_struct_value();
                }
                one(aggregate.into())
            }
            InstructionKind::ProductExtract { aggregate, field } => one(self
                .backend
                .builder
                .build_extract_value(
                    self.value(*aggregate)?.into_struct_value(),
                    *field,
                    "product.extract",
                )
                .map_err(builder_error)?),
            InstructionKind::ProductInsert {
                aggregate,
                field,
                value,
            } => one(self
                .backend
                .builder
                .build_insert_value(
                    self.value(*aggregate)?.into_struct_value(),
                    self.value(*value)?,
                    *field,
                    "product.insert",
                )
                .map_err(builder_error)?
                .into_struct_value()
                .into()),
            InstructionKind::RefineProven { value } | InstructionKind::Unrefine { value } => {
                // Checked LCIR requires both semantic types to select the exact
                // same physical representation. Preserve the SSA value
                // directly; the instruction exists to retain the nominal
                // proof boundary in LCIR and artifact identity.
                one(self.value(*value)?)
            }
            InstructionKind::SumConstruct { variant, payload } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("{} sum construction has no result", instruction.id()),
                    )
                })?;
                let ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", format!("missing result {result}"))
                    })?
                    .ty();
                one(self.emit_sum_construct(ty, *variant, payload)?)
            }
            InstructionKind::BoolNot { value } => one(self
                .backend
                .builder
                .build_not(self.int(*value)?, "bool.not")
                .map(Into::into)
                .map_err(builder_error)?),
            InstructionKind::BoolCompare {
                predicate,
                left,
                right,
            } => {
                let predicate = match predicate {
                    BoolPredicate::Equal => IntPredicate::EQ,
                    BoolPredicate::NotEqual => IntPredicate::NE,
                };
                one(self
                    .backend
                    .builder
                    .build_int_compare(
                        predicate,
                        self.int(*left)?,
                        self.int(*right)?,
                        "bool.compare",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?)
            }
            InstructionKind::FloatNegate { value } => one(self
                .backend
                .builder
                .build_float_neg(self.value(*value)?.into_float_value(), "float.negate")
                .map(Into::into)
                .map_err(builder_error)?),
            InstructionKind::FloatBinary { op, left, right } => {
                let left = self.value(*left)?.into_float_value();
                let right = self.value(*right)?.into_float_value();
                one(match op {
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
                .map_err(builder_error)?)
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
                one(self
                    .backend
                    .builder
                    .build_int_compare(
                        predicate,
                        self.int(*left)?,
                        self.int(*right)?,
                        "int.compare",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?)
            }
            InstructionKind::IntSuccessorBelow { value, .. } => {
                // The CheckedArtifact boundary has already proved the exact
                // dominating true-edge fact carried by the other operands.
                // They are evidence, not runtime values for this operation.
                one(self
                    .backend
                    .builder
                    .build_int_nsw_add(
                        self.int(*value)?,
                        self.backend.context.i64_type().const_int(1, false),
                        "int.successor",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?)
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
                one(self
                    .backend
                    .builder
                    .build_float_compare(
                        predicate,
                        self.value(*left)?.into_float_value(),
                        self.value(*right)?.into_float_value(),
                        "float.compare",
                    )
                    .map(Into::into)
                    .map_err(builder_error)?)
            }
            InstructionKind::DirectCall { callee, arguments } => {
                let arguments = self.call_arguments(arguments, false)?;
                let returned = call_basic(
                    &self.backend.builder,
                    self.backend.function(*callee)?,
                    &arguments,
                    "direct.call",
                )?;
                let callee = self.backend.artifact.function(*callee).ok_or_else(|| {
                    CodegenError::new(
                        "InvalidFunctionReference",
                        "direct LCIR callee is missing from its artifact",
                    )
                })?;
                if callee.signature().inout_params().is_empty() {
                    one(returned)
                } else {
                    let returned = returned.into_struct_value();
                    (0..=callee.signature().inout_params().len())
                        .map(|index| {
                            let index = u32::try_from(index).map_err(|_| {
                                CodegenError::new(
                                    "ProgramTooLarge",
                                    "too many direct-call writebacks",
                                )
                            })?;
                            self.backend
                                .builder
                                .build_extract_value(returned, index, "direct.call.result")
                                .map_err(builder_error)
                        })
                        .collect::<Result<Vec<_>, _>>()?
                }
            }
        };
        if instruction.results().len() != values.len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "{} has {} checked results but LLVM emission produced {}",
                    instruction.id(),
                    instruction.results().len(),
                    values.len()
                ),
            ));
        }
        for (result, value) in instruction.results().iter().zip(values) {
            self.values[result.index()] = Some(value);
        }
        Ok(())
    }

    fn emit_sum_construct(
        &self,
        ty: ValueTypeId,
        variant: u32,
        payload: &[ValueId],
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let layout = self.backend.sum_layout(ty)?;
        let variant_index = usize::try_from(variant).map_err(|_| {
            CodegenError::new("LlvmAbiDefect", format!("invalid sum variant {variant}"))
        })?;
        let payload_type = layout.payloads.get(variant_index).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("sum type {ty} has no variant {variant}"),
            )
        })?;
        if usize::try_from(payload_type.count_fields()).ok() != Some(payload.len()) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "sum type {ty} variant {variant} has {} LLVM payload fields but {} LCIR values",
                    payload_type.count_fields(),
                    payload.len()
                ),
            ));
        }
        let mut payload_value = payload_type.get_undef();
        for (index, value) in payload.iter().copied().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many sum payload fields"))?;
            payload_value = self
                .backend
                .builder
                .build_insert_value(payload_value, self.value(value)?, index, "sum.payload")
                .map_err(builder_error)?
                .into_struct_value();
        }
        match layout.tag {
            SumTagRepr::Tagless => Ok(payload_value.into()),
            SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                let tag = self
                    .backend
                    .sum_tag_type(layout.tag)
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sum tag type is missing"))?
                    .const_int(u64::from(variant), false);
                let Some(carrier_type) = layout.carrier else {
                    if !payload.is_empty() {
                        return Err(CodegenError::new(
                            "LlvmAbiDefect",
                            format!("tag-only sum type {ty} carried a payload"),
                        ));
                    }
                    return Ok(tag.into());
                };
                let carrier = self.pack_sum_carrier(payload_value, payload_type, carrier_type)?;
                let physical = layout.physical.into_struct_type();
                let tagged = self
                    .backend
                    .builder
                    .build_insert_value(physical.get_undef(), tag, 0, "sum.construct.tag")
                    .map_err(builder_error)?
                    .into_struct_value();
                Ok(self
                    .backend
                    .builder
                    .build_insert_value(tagged, carrier, 1, "sum.construct.value")
                    .map_err(builder_error)?
                    .into_struct_value()
                    .into())
            }
        }
    }

    fn pack_sum_carrier(
        &self,
        payload: inkwell::values::StructValue<'ctx>,
        payload_type: StructType<'ctx>,
        carrier_type: StructType<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        if Self::carrier_byte_len(carrier_type)? == 0 {
            return Ok(carrier_type.const_zero().into());
        }
        let (bytes, wide_type) = self.carrier_bits_type(carrier_type)?;
        let bits = self.pack_value_bits(
            wide_type.const_zero(),
            wide_type,
            payload.into(),
            payload_type.into(),
            0,
        )?;
        let byte_array_type = carrier_type
            .get_field_type_at_index(1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sum carrier has no byte array"))?
            .into_array_type();
        let mut byte_array = byte_array_type.const_zero();
        for index in 0..bytes {
            let shift = wide_type.const_int(Self::sum_carrier_shift(u64::from(index))?, false);
            let shifted = if index == 0 {
                bits
            } else {
                self.backend
                    .builder
                    .build_right_shift(bits, shift, false, "sum.pack.byte.shift")
                    .map_err(builder_error)?
            };
            let byte = self
                .backend
                .builder
                .build_int_truncate(shifted, self.backend.context.i8_type(), "sum.pack.byte")
                .map_err(builder_error)?;
            byte_array = self
                .backend
                .builder
                .build_insert_value(byte_array, byte, index, "sum.pack.carrier.byte")
                .map_err(builder_error)?
                .into_array_value();
        }
        Ok(self
            .backend
            .builder
            .build_insert_value(carrier_type.const_zero(), byte_array, 1, "sum.pack.carrier")
            .map_err(builder_error)?
            .into_struct_value()
            .into())
    }

    fn unpack_sum_carrier(
        &self,
        carrier: BasicValueEnum<'ctx>,
        carrier_type: StructType<'ctx>,
        payload_type: StructType<'ctx>,
    ) -> Result<inkwell::values::StructValue<'ctx>, CodegenError> {
        if Self::carrier_byte_len(carrier_type)? == 0 {
            return Ok(payload_type.const_zero());
        }
        let (bytes, wide_type) = self.carrier_bits_type(carrier_type)?;
        let byte_array = self
            .backend
            .builder
            .build_extract_value(carrier.into_struct_value(), 1, "sum.unpack.carrier.bytes")
            .map_err(builder_error)?
            .into_array_value();
        let mut bits = wide_type.const_zero();
        for index in 0..bytes {
            let byte = self
                .backend
                .builder
                .build_extract_value(byte_array, index, "sum.unpack.carrier.byte")
                .map_err(builder_error)?
                .into_int_value();
            let extended = self
                .backend
                .builder
                .build_int_z_extend(byte, wide_type, "sum.unpack.byte.extend")
                .map_err(builder_error)?;
            let shifted = if index == 0 {
                extended
            } else {
                self.backend
                    .builder
                    .build_left_shift(
                        extended,
                        wide_type.const_int(Self::sum_carrier_shift(u64::from(index))?, false),
                        "sum.unpack.byte.shift",
                    )
                    .map_err(builder_error)?
            };
            bits = self
                .backend
                .builder
                .build_or(bits, shifted, "sum.unpack.byte.merge")
                .map_err(builder_error)?;
        }
        Ok(self
            .unpack_value_bits(bits, wide_type, payload_type.into(), 0)?
            .into_struct_value())
    }

    fn carrier_bits_type(
        &self,
        carrier_type: StructType<'ctx>,
    ) -> Result<(u32, IntType<'ctx>), CodegenError> {
        if self.backend.target_data.get_byte_ordering() != ByteOrdering::LittleEndian {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "direct LCIR sum carriers currently require a little-endian target",
            ));
        }
        let bytes = Self::carrier_byte_len(carrier_type)?;
        let width = bytes
            .checked_mul(8)
            .and_then(NonZeroU32::new)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "sum carrier has no addressable payload bits",
                )
            })?;
        let wide_type = self
            .backend
            .context
            .custom_width_int_type(width)
            .map_err(|message| CodegenError::new("ProgramTooLarge", message))?;
        Ok((bytes, wide_type))
    }

    fn carrier_byte_len(carrier_type: StructType<'ctx>) -> Result<u32, CodegenError> {
        Ok(carrier_type
            .get_field_type_at_index(1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sum carrier has no byte array"))?
            .into_array_type()
            .len())
    }

    fn sum_carrier_shift(byte_offset: u64) -> Result<u64, CodegenError> {
        byte_offset.checked_mul(8).ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "sum carrier bit offset overflowed")
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "all recursively supported LLVM aggregate kinds remain visible in one bounded carrier packing routine"
    )]
    fn pack_value_bits(
        &self,
        accumulator: IntValue<'ctx>,
        wide_type: IntType<'ctx>,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        byte_offset: u64,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        match ty {
            BasicTypeEnum::IntType(ty) => {
                let value = value.into_int_value();
                let value = match ty.get_bit_width().cmp(&wide_type.get_bit_width()) {
                    std::cmp::Ordering::Less => self
                        .backend
                        .builder
                        .build_int_z_extend(value, wide_type, "sum.pack.extend")
                        .map_err(builder_error)?,
                    std::cmp::Ordering::Equal => value,
                    std::cmp::Ordering::Greater => {
                        return Err(CodegenError::new(
                            "LlvmAbiDefect",
                            "sum payload scalar is wider than its carrier",
                        ));
                    }
                };
                let shifted = if byte_offset == 0 {
                    value
                } else {
                    self.backend
                        .builder
                        .build_left_shift(
                            value,
                            wide_type.const_int(Self::sum_carrier_shift(byte_offset)?, false),
                            "sum.pack.shift",
                        )
                        .map_err(builder_error)?
                };
                self.backend
                    .builder
                    .build_or(accumulator, shifted, "sum.pack.merge")
                    .map_err(builder_error)
            }
            BasicTypeEnum::FloatType(ty) => {
                let width =
                    u32::try_from(self.backend.target_data.get_bit_size(&ty)).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "sum float payload is too wide")
                    })?;
                let int_type = self
                    .backend
                    .context
                    .custom_width_int_type(NonZeroU32::new(width).ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "sum float payload has zero width")
                    })?)
                    .map_err(|message| CodegenError::new("ProgramTooLarge", message))?;
                let bits = self
                    .backend
                    .builder
                    .build_bit_cast(value.into_float_value(), int_type, "sum.pack.float")
                    .map_err(builder_error)?;
                self.pack_value_bits(accumulator, wide_type, bits, int_type.into(), byte_offset)
            }
            BasicTypeEnum::StructType(ty) => {
                let value = value.into_struct_value();
                let mut packed = accumulator;
                for (index, field_type) in ty.get_field_types().into_iter().enumerate() {
                    let index = u32::try_from(index).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "too many nested sum fields")
                    })?;
                    let field = self
                        .backend
                        .builder
                        .build_extract_value(value, index, "sum.pack.field")
                        .map_err(builder_error)?;
                    let offset = self
                        .backend
                        .target_data
                        .offset_of_element(&ty, index)
                        .and_then(|offset| byte_offset.checked_add(offset))
                        .ok_or_else(|| {
                            CodegenError::new("LlvmAbiDefect", "missing nested sum field offset")
                        })?;
                    packed = self.pack_value_bits(packed, wide_type, field, field_type, offset)?;
                }
                Ok(packed)
            }
            BasicTypeEnum::ArrayType(ty) => {
                let value = value.into_array_value();
                let element_type = ty.get_element_type();
                let stride = self.backend.target_data.get_abi_size(&element_type);
                let mut packed = accumulator;
                for index in 0..ty.len() {
                    let element = self
                        .backend
                        .builder
                        .build_extract_value(value, index, "sum.pack.element")
                        .map_err(builder_error)?;
                    let offset = u64::from(index)
                        .checked_mul(stride)
                        .and_then(|offset| byte_offset.checked_add(offset))
                        .ok_or_else(|| {
                            CodegenError::new("ProgramTooLarge", "sum array offset overflowed")
                        })?;
                    packed =
                        self.pack_value_bits(packed, wide_type, element, element_type, offset)?;
                }
                Ok(packed)
            }
            BasicTypeEnum::PointerType(_)
            | BasicTypeEnum::VectorType(_)
            | BasicTypeEnum::ScalableVectorType(_) => Err(CodegenError::new(
                "LlvmAbiDefect",
                "unsupported physical value in a direct LCIR sum carrier",
            )),
        }
    }

    fn unpack_value_bits(
        &self,
        bits: IntValue<'ctx>,
        wide_type: IntType<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        byte_offset: u64,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match ty {
            BasicTypeEnum::IntType(ty) => {
                let shifted = if byte_offset == 0 {
                    bits
                } else {
                    self.backend
                        .builder
                        .build_right_shift(
                            bits,
                            wide_type.const_int(Self::sum_carrier_shift(byte_offset)?, false),
                            false,
                            "sum.unpack.shift",
                        )
                        .map_err(builder_error)?
                };
                Ok(if ty.get_bit_width() == wide_type.get_bit_width() {
                    shifted.into()
                } else {
                    self.backend
                        .builder
                        .build_int_truncate(shifted, ty, "sum.unpack.int")
                        .map(Into::into)
                        .map_err(builder_error)?
                })
            }
            BasicTypeEnum::FloatType(ty) => {
                let width =
                    u32::try_from(self.backend.target_data.get_bit_size(&ty)).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "sum float payload is too wide")
                    })?;
                let int_type = self
                    .backend
                    .context
                    .custom_width_int_type(NonZeroU32::new(width).ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "sum float payload has zero width")
                    })?)
                    .map_err(|message| CodegenError::new("ProgramTooLarge", message))?;
                let value = self
                    .unpack_value_bits(bits, wide_type, int_type.into(), byte_offset)?
                    .into_int_value();
                self.backend
                    .builder
                    .build_bit_cast(value, ty, "sum.unpack.float")
                    .map_err(builder_error)
            }
            BasicTypeEnum::StructType(ty) => {
                let mut value = ty.get_undef();
                for (index, field_type) in ty.get_field_types().into_iter().enumerate() {
                    let index = u32::try_from(index).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "too many nested sum fields")
                    })?;
                    let offset = self
                        .backend
                        .target_data
                        .offset_of_element(&ty, index)
                        .and_then(|offset| byte_offset.checked_add(offset))
                        .ok_or_else(|| {
                            CodegenError::new("LlvmAbiDefect", "missing nested sum field offset")
                        })?;
                    let field = self.unpack_value_bits(bits, wide_type, field_type, offset)?;
                    value = self
                        .backend
                        .builder
                        .build_insert_value(value, field, index, "sum.unpack.field")
                        .map_err(builder_error)?
                        .into_struct_value();
                }
                Ok(value.into())
            }
            BasicTypeEnum::ArrayType(ty) => {
                let element_type = ty.get_element_type();
                let stride = self.backend.target_data.get_abi_size(&element_type);
                let mut value = ty.const_zero();
                for index in 0..ty.len() {
                    let offset = u64::from(index)
                        .checked_mul(stride)
                        .and_then(|offset| byte_offset.checked_add(offset))
                        .ok_or_else(|| {
                            CodegenError::new("ProgramTooLarge", "sum array offset overflowed")
                        })?;
                    let element = self.unpack_value_bits(bits, wide_type, element_type, offset)?;
                    value = self
                        .backend
                        .builder
                        .build_insert_value(value, element, index, "sum.unpack.element")
                        .map_err(builder_error)?
                        .into_array_value();
                }
                Ok(value.into())
            }
            BasicTypeEnum::PointerType(_)
            | BasicTypeEnum::VectorType(_)
            | BasicTypeEnum::ScalableVectorType(_) => Err(CodegenError::new(
                "LlvmAbiDefect",
                "unsupported physical value in a direct LCIR sum carrier",
            )),
        }
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
                if then_target == else_target {
                    self.branch(then_target)?;
                } else if then_target.block == else_target.block {
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
            TerminatorKind::SumSwitch { scrutinee, cases } => {
                self.emit_sum_switch(*scrutinee, cases)
            }
            TerminatorKind::Return(value) => {
                self.emit_return(self.value(*value)?, terminator.writebacks())
            }
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
                metadata,
                success,
                fault,
            } => self.emit_assert(self.int(*condition)?, metadata, success, fault),
            TerminatorKind::Fault { metadata } => {
                match metadata {
                    FaultMetadata::Runtime(code) => {
                        self.emit_source_fault(*code, terminator.origin())?;
                    }
                    FaultMetadata::Contract(metadata) => {
                        self.emit_contract_fault(metadata)?;
                    }
                }
                self.emit_fault_return(terminator.writebacks())
            }
            TerminatorKind::ResumeFault => self.emit_fault_return(terminator.writebacks()),
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive sum dispatch keeps tag extraction, per-case payload decoding, and phi-edge construction together"
    )]
    fn emit_sum_switch(
        &self,
        scrutinee: ValueId,
        cases: &[loom_codegen_ir::SumCase],
    ) -> Result<(), CodegenError> {
        let ty = self
            .source
            .value(scrutinee)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", format!("missing scrutinee {scrutinee}"))
            })?
            .ty();
        let layout = self.backend.sum_layout(ty)?;
        let value = self.value(scrutinee)?;
        let edges = cases
            .iter()
            .map(|case| {
                self.backend
                    .context
                    .append_basic_block(self.function, &format!("sum.case.{}", case.variant))
            })
            .collect::<Vec<_>>();

        let carrier = match layout.tag {
            SumTagRepr::Tagless => {
                let [edge] = edges.as_slice() else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        format!("tagless sum switch for {ty} does not have one case"),
                    ));
                };
                self.backend
                    .builder
                    .build_unconditional_branch(*edge)
                    .map_err(builder_error)?;
                None
            }
            SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                let (tag, carrier) = if layout.carrier.is_some() {
                    let aggregate = value.into_struct_value();
                    let tag = self
                        .backend
                        .builder
                        .build_extract_value(aggregate, 0, "sum.switch.tag")
                        .map_err(builder_error)?
                        .into_int_value();
                    let carrier = self
                        .backend
                        .builder
                        .build_extract_value(aggregate, 1, "sum.switch.carrier")
                        .map_err(builder_error)?;
                    (tag, Some(carrier))
                } else {
                    (value.into_int_value(), None)
                };
                let default = self
                    .backend
                    .context
                    .append_basic_block(self.function, "sum.switch.invalid");
                let llvm_cases = cases
                    .iter()
                    .zip(&edges)
                    .map(|(case, edge)| {
                        (
                            tag.get_type().const_int(u64::from(case.variant), false),
                            *edge,
                        )
                    })
                    .collect::<Vec<_>>();
                self.backend
                    .builder
                    .build_switch(tag, default, &llvm_cases)
                    .map_err(builder_error)?;
                self.backend.builder.position_at_end(default);
                self.backend
                    .builder
                    .build_unreachable()
                    .map_err(builder_error)?;
                carrier
            }
        };

        for ((case, edge), payload_type) in cases.iter().zip(edges).zip(&layout.payloads) {
            self.backend.builder.position_at_end(edge);
            let payload = match layout.tag {
                SumTagRepr::Tagless => value.into_struct_value(),
                SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                    if let Some(carrier_type) = layout.carrier {
                        self.unpack_sum_carrier(
                            carrier.ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    "tagged sum switch has no carrier value",
                                )
                            })?,
                            carrier_type,
                            *payload_type,
                        )?
                    } else {
                        payload_type.const_zero()
                    }
                }
            };
            let implicit = (0..payload_type.count_fields())
                .map(|field| {
                    self.backend
                        .builder
                        .build_extract_value(payload, field, "sum.switch.field")
                        .map_err(builder_error)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let predecessor = self.current_block()?;
            self.add_implicit_incoming(case.block, &implicit, &case.arguments, predecessor)?;
            self.backend
                .builder
                .build_unconditional_branch(self.block(case.block)?)
                .map_err(builder_error)?;
        }
        Ok(())
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
        self.add_result_incoming(normal, &[result], predecessor)?;
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
        let callee_source = self.backend.artifact.function(callee).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                "invoked LCIR callee is missing from its artifact",
            )
        })?;
        let mut logical_results =
            Vec::with_capacity(1 + callee_source.signature().inout_params().len());
        for index in 0..=callee_source.signature().inout_params().len() {
            let field = u32::try_from(index.saturating_add(1))
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many invoke writebacks"))?;
            logical_results.push(
                self.backend
                    .builder
                    .build_extract_value(aggregate, field, "invoke.result")
                    .map_err(builder_error)?,
            );
        }
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
        self.add_result_incoming(normal, &logical_results, predecessor)?;
        self.add_unwind_incoming(unwind, &logical_results[1..], predecessor)?;
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
        metadata: &ContractFaultMetadata,
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
        self.emit_contract_fault(metadata)?;
        self.unwind_branch(fault)
    }

    fn emit_source_fault(&self, code: FaultCode, origin: Origin) -> Result<(), CodegenError> {
        self.emit_fault(FaultEmission::Runtime { code, origin })
    }

    fn emit_contract_fault(&self, metadata: &ContractFaultMetadata) -> Result<(), CodegenError> {
        self.emit_fault(FaultEmission::Contract(metadata))
    }

    fn emit_fault(&self, fault: FaultEmission<'_>) -> Result<(), CodegenError> {
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
        match fault {
            FaultEmission::Runtime { code, origin } => {
                self.backend.raise_fault(runtime, code, origin)?;
            }
            FaultEmission::Contract(metadata) => {
                self.backend.raise_contract_fault(runtime, metadata)?;
            }
        }
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

    fn emit_return(
        &self,
        value: BasicValueEnum<'ctx>,
        writebacks: &[ValueId],
    ) -> Result<(), CodegenError> {
        let mut values = Vec::with_capacity(1 + writebacks.len());
        values.push(value);
        for writeback in writebacks {
            values.push(self.value(*writeback)?);
        }
        if self.source.effects().contains(Effects::MAY_FAULT) {
            self.emit_status_return(self.backend.context.i32_type().const_zero(), &values)
        } else if let [value] = values.as_slice() {
            self.backend
                .builder
                .build_return(Some(value))
                .map_err(builder_error)?;
            Ok(())
        } else {
            let return_type = self
                .function
                .get_type()
                .get_return_type()
                .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "function returns void"))?
                .into_struct_type();
            let aggregate = self.build_return_aggregate(return_type, &values, 0)?;
            self.backend
                .builder
                .build_return(Some(&aggregate))
                .map_err(builder_error)?;
            Ok(())
        }
    }

    fn emit_fault_return(&self, writebacks: &[ValueId]) -> Result<(), CodegenError> {
        let zero = self.backend.zero(self.source.signature().result())?;
        let mut values = Vec::with_capacity(1 + writebacks.len());
        values.push(zero);
        for writeback in writebacks {
            values.push(self.value(*writeback)?);
        }
        self.emit_status_return(self.backend.context.i32_type().const_int(1, false), &values)
    }

    fn emit_status_return(
        &self,
        status: IntValue<'ctx>,
        values: &[BasicValueEnum<'ctx>],
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
        let mut aggregate = self
            .backend
            .builder
            .build_insert_value(return_type.get_undef(), status, 0, "return.status")
            .map_err(builder_error)?
            .into_struct_value();
        for (index, value) in values.iter().enumerate() {
            let index = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "too many LCIR return writebacks")
                })?;
            aggregate = self
                .backend
                .builder
                .build_insert_value(aggregate, *value, index, "return.value")
                .map_err(builder_error)?
                .into_struct_value();
        }
        self.backend
            .builder
            .build_return(Some(&aggregate))
            .map_err(builder_error)?;
        Ok(())
    }

    fn build_return_aggregate(
        &self,
        return_type: StructType<'ctx>,
        values: &[BasicValueEnum<'ctx>],
        offset: u32,
    ) -> Result<inkwell::values::StructValue<'ctx>, CodegenError> {
        let mut aggregate = return_type.get_undef();
        for (index, value) in values.iter().enumerate() {
            let index = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(offset))
                .ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "too many LCIR return writebacks")
                })?;
            aggregate = self
                .backend
                .builder
                .build_insert_value(aggregate, *value, index, "return.value")
                .map_err(builder_error)?
                .into_struct_value();
        }
        Ok(aggregate)
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
        self.add_result_incoming(target, &[result], predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(target.block)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn unwind_branch(&self, target: &UnwindTarget) -> Result<(), CodegenError> {
        let predecessor = self.current_block()?;
        self.add_unwind_incoming(target, &[], predecessor)?;
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
        implicit: &[BasicValueEnum<'ctx>],
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        self.add_implicit_incoming(target.block, implicit, &target.arguments, predecessor)
    }

    fn add_result_incoming(
        &self,
        target: &ResultTarget,
        implicit: &[BasicValueEnum<'ctx>],
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        self.add_implicit_incoming(target.block, implicit, &target.arguments, predecessor)
    }

    fn add_implicit_incoming(
        &self,
        block: BlockId,
        implicit: &[BasicValueEnum<'ctx>],
        arguments: &[ValueId],
        predecessor: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let mut values = Vec::with_capacity(implicit.len() + arguments.len());
        values.extend_from_slice(implicit);
        for argument in arguments {
            values.push(self.value(*argument)?);
        }
        self.add_basic_incoming(block, &values, predecessor)
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
        let (status, _) = self.call_fallible_root(root, fault_context, "run")?;
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
        let outcomes = self.artifact.test_outcomes().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "LCIR test artifact has no outcome plans")
        })?;
        if roots.len() != outcomes.len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "LCIR test roots and outcome plans have different lengths",
            ));
        }
        for (root, outcome) in roots.iter().zip(outcomes) {
            let source = self.artifact.function(*root).ok_or_else(|| {
                CodegenError::new(
                    "InvalidFunctionReference",
                    format!("LCIR test root {root} is missing"),
                )
            })?;
            if source.effects().contains(Effects::MAY_FAULT) {
                self.emit_fallible_test(main, *root, source.name(), *outcome, failed)?;
            } else {
                let returned = call_basic(&self.builder, self.function(*root)?, &[], "test")?;
                let succeeded =
                    self.test_outcome_succeeded(returned, source.signature().result(), *outcome)?;
                self.emit_test_completion(main, source.name(), failed, succeeded)?;
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
        outcome: TestOutcomePlan,
        failed: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let runtime = call_pointer(&self.builder, self.runtime_create(), &[], "test.runtime")?;
        let ready = self.context.append_basic_block(main, "test.runtime.ready");
        let create_failed = self
            .context
            .append_basic_block(main, "test.runtime.create.failed");
        let exists = self
            .builder
            .build_is_not_null(runtime, "test.runtime.exists")
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
        self.puts("RuntimeFault: runtime activation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        let next = self.context.append_basic_block(main, "test.next");
        self.builder.position_at_end(activated);
        let fault_context = self.initialize_fault_context(runtime)?;
        let (status, returned) = self.call_fallible_root(root, fault_context, "test")?;
        self.destroy_runtime(runtime)?;
        let runtime_succeeded = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_zero(),
                "test.succeeded",
            )
            .map_err(builder_error)?;
        let source = self.artifact.function(root).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("LCIR test root {root} is missing"),
            )
        })?;
        let outcome_succeeded =
            self.test_outcome_succeeded(returned, source.signature().result(), outcome)?;
        let succeeded = self
            .builder
            .build_and(
                runtime_succeeded,
                outcome_succeeded,
                "test.outcome.succeeded",
            )
            .map_err(builder_error)?;
        self.emit_test_completion_to(main, name, failed, succeeded, next)
    }

    fn emit_test_completion(
        &self,
        main: FunctionValue<'ctx>,
        name: &str,
        failed: PointerValue<'ctx>,
        succeeded: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let next = self.context.append_basic_block(main, "test.next");
        self.emit_test_completion_to(main, name, failed, succeeded, next)
    }

    fn emit_test_completion_to(
        &self,
        main: FunctionValue<'ctx>,
        name: &str,
        failed: PointerValue<'ctx>,
        succeeded: IntValue<'ctx>,
        next: BasicBlock<'ctx>,
    ) -> Result<(), CodegenError> {
        let pass = self.context.append_basic_block(main, "test.pass");
        let fail = self.context.append_basic_block(main, "test.fail");
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

    fn test_outcome_succeeded(
        &self,
        value: BasicValueEnum<'ctx>,
        ty: ValueTypeId,
        outcome: TestOutcomePlan,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        match outcome {
            TestOutcomePlan::Unit => Ok(self.context.bool_type().const_int(1, false)),
            TestOutcomePlan::Result {
                success_variant,
                failure_variant: _,
            } => {
                let layout = self.sum_layout(ty)?;
                let tag = match layout.tag {
                    SumTagRepr::Tagless => {
                        return Err(CodegenError::new(
                            "LlvmAbiDefect",
                            format!("Result test type {ty} unexpectedly has no physical tag"),
                        ));
                    }
                    SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                        if layout.carrier.is_some() {
                            self.builder
                                .build_extract_value(
                                    value.into_struct_value(),
                                    0,
                                    "test.result.tag",
                                )
                                .map_err(builder_error)?
                                .into_int_value()
                        } else {
                            value.into_int_value()
                        }
                    }
                };
                self.builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        tag.get_type().const_int(u64::from(success_variant), false),
                        "test.result.succeeded",
                    )
                    .map_err(builder_error)
            }
        }
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
    ) -> Result<(IntValue<'ctx>, BasicValueEnum<'ctx>), CodegenError> {
        let aggregate = call_basic(&self.builder, self.function(root)?, &[context.into()], name)?
            .into_struct_value();
        let status = self
            .builder
            .build_extract_value(aggregate, 0, &format!("{name}.status"))
            .map_err(builder_error)?
            .into_int_value();
        let value = self
            .builder
            .build_extract_value(aggregate, 1, &format!("{name}.value"))
            .map_err(builder_error)?;
        Ok((status, value))
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
        // Integer overflow is a shared language fault, so expose the same
        // stable RuntimeFault payload as the interpreter and legacy emitter.
        // Other LCIR fault families retain their existing private detail until
        // their language-level diagnostic contracts are specified separately.
        let detail = if fault == FaultCode::IntegerOverflow {
            serde_json::json!({
                "channel": "runtime",
                "fault": {
                    "code": code,
                    "message": message,
                    "span": origin.span,
                },
            })
        } else {
            serde_json::json!({
                "channel": "lcir",
                "code": code,
                "sourceFunction": origin.source_function.0,
                "sourceSpan": {
                    "file": origin.span.file.0,
                    "start": origin.span.range.start,
                    "end": origin.span.range.end,
                },
            })
        }
        .to_string();
        self.raise_fault_payload(runtime, code, message, &display, &detail)
    }

    fn raise_contract_fault(
        &self,
        runtime: PointerValue<'ctx>,
        metadata: &ContractFaultMetadata,
    ) -> Result<(), CodegenError> {
        let code = metadata.kind().fault_code();
        let message = metadata.message();
        let display = metadata.user_code().map_or_else(
            || code.to_owned(),
            |user_code| format!("{code}: {user_code}"),
        );
        let detail = serde_json::to_string(&serde_json::json!({
            "channel": "contract",
            "fault": {
                "code": code,
                "category": metadata.kind().category(),
                "message": message,
                "contractSpan": metadata.contract_span(),
                "blameSpan": metadata.blame_span(),
            },
        }))
        .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
        self.raise_fault_payload(runtime, code, message, &display, &detail)
    }

    fn raise_fault_payload(
        &self,
        runtime: PointerValue<'ctx>,
        code: &str,
        message: &str,
        display: &str,
        detail: &str,
    ) -> Result<(), CodegenError> {
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
            .build_global_string_ptr(display, &self.unique("fault.display"))
            .map_err(builder_error)?;
        let detail_data = self
            .builder
            .build_global_string_ptr(detail, &self.unique("fault.detail"))
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
        FaultCode::IntegerOverflow => (INTEGER_OVERFLOW_FAULT_CODE, INTEGER_OVERFLOW_FAULT_MESSAGE),
        FaultCode::IntegerDivisionByZero => ("IntegerDivisionByZero", "integer division by zero"),
        FaultCode::IntegerDivisionOverflow => {
            ("IntegerDivisionOverflow", "integer division overflowed")
        }
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
