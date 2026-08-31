//! Mechanical typed LCIR to LLVM emission.
//!
//! This module emits checked, target-typed SSA without native-layout planning,
//! universal values, erased call nodes, or runtime-requirement analysis.
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
    AsDIScope, DIFile, DIFlags, DIFlagsConstants, DILocalVariable, DILocation, DISubprogram,
    DIType, DWARFEmissionKind, DWARFSourceLanguage, DebugInfoBuilder,
};
use inkwell::module::{Linkage, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{ByteOrdering, FileType, TargetData};
use inkwell::types::{
    AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, IntType, StructType,
};
use inkwell::values::{
    ArrayValue, AsValueRef, BasicMetadataValueEnum, BasicValueEnum, FunctionValue,
    InstructionValue, IntValue, PhiValue, PointerValue, StructValue, UnnamedAddress,
};
use inkwell::{FloatPredicate as LlvmFloatPredicate, IntPredicate};
use llvm_sys::debuginfo::LLVMDIBuilderInsertDbgValueRecordBefore;
use loom_codegen_ir::{
    AwaitMode, BlockId, BlockTarget, BoolPredicate, CheckedArtifact, CheckedIntBinaryOp, Constant,
    ContractFaultBlame, ContractFaultMetadata, CoroutinePlan, CoroutineSuspension, Effects,
    FaultCode, FaultMetadata, FloatBinaryOp, FloatPredicate as LcirFloatPredicate, Function,
    InstanceId, Instruction, InstructionId, InstructionKind, IntPredicate as LcirIntPredicate,
    IoTaskErrorMode, IoTaskOperation, MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE, ManagedRootPlan,
    ManagedRootProjection, ManagedRootSlot, ManagedSafepoint, Origin, Repr, ResourceKind,
    ResultTarget, ScalarRepr, SumRepr, SumTagRepr, TASK_OUTCOME_CANCELLED_VARIANT,
    TASK_OUTCOME_COMPLETED_VARIANT, TASK_OUTCOME_FAULTED_VARIANT, Terminator, TerminatorKind,
    TestOutcomePlan, UnwindTarget, ValueDefinition, ValueId, ValueTypeId, ValueTypeKind,
    plan_managed_roots,
};
use loom_core::runtime_fault::{
    ARTIFACT_PROOF_REJECTED_FAULT_CODE, ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE,
    EMPTY_TASK_JOIN_FAULT_CODE, EMPTY_TASK_JOIN_FAULT_MESSAGE, INTEGER_OVERFLOW_FAULT_CODE,
    INTEGER_OVERFLOW_FAULT_MESSAGE, INVALID_DURATION_FAULT_CODE, INVALID_DURATION_FAULT_MESSAGE,
};
use loom_core::runtime_fault::{
    INVALID_SLEEP_DURATION_FAULT_CODE, INVALID_SLEEP_DURATION_FAULT_MESSAGE, LOG_WRITE_FAULT_CODE,
    LOG_WRITE_FAULT_MESSAGE, SLEEP_DURATION_OVERFLOW_FAULT_CODE,
    SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE, STDOUT_WRITE_FAULT_CODE, STDOUT_WRITE_FAULT_MESSAGE,
    TASK_ANY_FAILED_FAULT_CODE, TASK_ANY_FAILED_FAULT_MESSAGE,
};
use loom_mir::Type;
use loom_runtime_abi::{
    BYTES_APPEND_TYPED_SYMBOL, BYTES_DECODE_UTF8_TYPED_INVALID_UTF8,
    BYTES_DECODE_UTF8_TYPED_SYMBOL, FORMAT_FLOAT_TYPED_SYMBOL, GC_MAX_OBJECT_ALIGNMENT,
    GC_MAX_OBJECT_BYTES, GC_MAX_OBJECT_POINTERS, GC_MAX_REPEATED_POINTER_CELLS,
    GC_MAX_ROOT_BITMAP_WORDS, GC_MAX_ROOT_SLOTS, GC_MAX_ROOT_STATES,
    PARSE_FLOAT_STATUS_INVALID_SYNTAX, PARSE_FLOAT_STATUS_OK, PARSE_FLOAT_STATUS_OUT_OF_RANGE,
    PARSE_FLOAT_SYMBOL, PATH_JOIN_TYPED_ABSOLUTE, PATH_JOIN_TYPED_SYMBOL,
    PROCESS_ARGUMENT_AT_TYPED_SYMBOL, PROCESS_ARGUMENT_COUNT_TYPED_INVALID,
    PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL, PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL,
    PROCESS_ENVIRONMENT_TYPED_FOUND, PROCESS_ENVIRONMENT_TYPED_MISSING,
    PROCESS_ENVIRONMENT_TYPED_SYMBOL, STDOUT_WRITE_FAILED, STDOUT_WRITE_OK, STDOUT_WRITE_SYMBOL,
    TASK_CANCELLED, TASK_COMPLETED, TASK_FAULTED, TASK_JOIN_ALL, TASK_JOIN_ANY, TASK_JOIN_RACE,
    TASK_JOIN_SETTLED, TASK_PENDING, TEXT_CONCAT_TYPED_SYMBOL, TEXT_CONTAINS_SYMBOL,
    TEXT_FROM_UTF8_UNITS_TYPED_INVALID_UTF8, TEXT_FROM_UTF8_UNITS_TYPED_SYMBOL,
    TEXT_GET_TYPED_FOUND, TEXT_GET_TYPED_MISSING, TEXT_GET_TYPED_SYMBOL, TEXT_LAYOUT_SYMBOL,
    TEXT_OBJECT_ALIGNMENT, TEXT_OBJECT_FIELD_BYTE_LENGTH, TEXT_OBJECT_FIELD_BYTES,
    TEXT_OBJECT_FIELD_SCALAR_LENGTH, TEXT_OBJECT_HEADER_SIZE, TYPED_GC_ABI_VERSION,
    TYPED_GC_ALLOC_SYMBOL, TYPED_GC_REPEATED_ABI_VERSION, TYPED_GC_REPEATED_ALLOC_SYMBOL,
    TYPED_GC_ROOT_POP_SYMBOL, TYPED_GC_ROOT_PUSH_SYMBOL, TYPED_IO_ABI_VERSION,
    TYPED_IO_CANCEL_SYMBOL, TYPED_IO_FAULT_CLASS_INVALID_PORT, TYPED_IO_FAULT_CLASS_OPERATION,
    TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE, TYPED_IO_INVALID_RESOURCE_TOKEN,
    TYPED_IO_OPERATION_FILE_CREATE, TYPED_IO_OPERATION_FILE_OPEN_READ,
    TYPED_IO_OPERATION_FILE_READ_TEXT, TYPED_IO_OPERATION_FILE_WRITE_TEXT,
    TYPED_IO_OPERATION_SOCKET_CONNECT, TYPED_IO_OPERATION_SOCKET_READ_TEXT,
    TYPED_IO_OPERATION_SOCKET_WRITE_TEXT, TYPED_IO_OUTCOME_ERROR, TYPED_IO_OUTCOME_RESOURCE,
    TYPED_IO_OUTCOME_TEXT, TYPED_IO_OUTCOME_UNIT, TYPED_IO_POLL_SYMBOL,
    TYPED_IO_TASK_CREATE_SYMBOL, TYPED_JSON_ABI_VERSION, TYPED_JSON_FORMAT_DEPTH_LIMIT,
    TYPED_JSON_FORMAT_NON_FINITE_NUMBER, TYPED_JSON_FORMAT_OK, TYPED_JSON_FORMAT_SYMBOL,
    TYPED_LOG_FIELD_ALIGNMENT, TYPED_LOG_FIELD_KEY_OFFSET, TYPED_LOG_FIELD_SIZE,
    TYPED_LOG_FIELD_VALUE_OFFSET, TYPED_LOG_OK, TYPED_LOG_WRITE_FAILED, TYPED_LOG_WRITE_SYMBOL,
    TYPED_RESOURCE_CLOSE_OK, TYPED_RESOURCE_CLOSE_SYMBOL, TYPED_RESOURCE_KIND_FILE,
    TYPED_RESOURCE_KIND_SOCKET, TYPED_SHADOW_STACK_ABI_VERSION, TYPED_TASK_ABI_VERSION,
    TYPED_TASK_PUBLISH_ADOPTING_SYMBOL, TYPED_TASK_TAKE_OUTCOME_SYMBOL,
    TYPED_TIMER_TASK_CREATE_SYMBOL,
};

use crate::codegen::{DebugSource, NativeObjectArtifact, NativeObjectOptions};
use crate::target::{
    NativeTargetMachine, configure_debug_module_flags, create_llvm_target_machine,
};
use crate::{CodegenError, trace_llvm_stage};

pub(crate) struct LcirEmitter;

const TYPED_TASK_CREATE_SYMBOL: &str = "loom_typed_task_create_v1";
const TYPED_TASK_FRAME_SYMBOL: &str = "loom_typed_task_frame_v1";
const TYPED_TASK_INITIALIZE_SYMBOL: &str = "loom_typed_task_initialize_v1";
const TYPED_TASK_PUBLISH_SYMBOL: &str = "loom_typed_task_publish_v1";
const TYPED_TASK_SET_ROOT_STATE_SYMBOL: &str = "loom_typed_task_set_root_state_v1";
const TYPED_TASK_PUBLISH_RESULT_SYMBOL: &str = "loom_typed_task_publish_result_v1";
const TYPED_TASK_TAKE_RESULT_SYMBOL: &str = "loom_typed_task_take_result_v1";
const TYPED_TASK_ABORT_UNPUBLISHED_SYMBOL: &str = "loom_typed_task_abort_unpublished_v1";
const TYPED_TASK_IS_CANCEL_REQUESTED_SYMBOL: &str = "loom_typed_task_is_cancel_requested_v1";

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
        trace_llvm_stage("lcir.pointer-width.begin");
        let llvm_pointer_bits = target.pointer_bits()?;
        trace_llvm_stage("lcir.pointer-width.end");
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

        trace_llvm_stage("lcir.context.begin");
        let context = Context::create();
        trace_llvm_stage("lcir.context.end");
        trace_llvm_stage("lcir.backend.begin");
        let backend = Backend::new(&context, artifact, options, target)?;
        trace_llvm_stage("lcir.backend.end");
        trace_llvm_stage("lcir.compile.begin");
        backend.compile()?;
        backend.finalize_debug();
        trace_llvm_stage("lcir.compile.end");
        trace_llvm_stage("lcir.verify-before-opt.begin");
        verify(&backend.module)?;
        trace_llvm_stage("lcir.verify-before-opt.end");
        trace_llvm_stage("lcir.optimize.begin");
        backend
            .module
            .run_passes(
                target.optimization().pipeline(),
                &target.machine,
                PassBuilderOptions::create(),
            )
            .map_err(|message| CodegenError::new("LlvmOptimizationFailed", message.to_string()))?;
        trace_llvm_stage("lcir.optimize.end");
        trace_llvm_stage("lcir.verify-after-opt.begin");
        verify(&backend.module)?;
        trace_llvm_stage("lcir.verify-after-opt.end");

        if let Some(ir_path) = &options.emit_ir {
            trace_llvm_stage("lcir.ir-write.begin");
            create_parent_directory(ir_path)?;
            backend.module.print_to_file(ir_path).map_err(|message| {
                CodegenError::new(
                    "LlvmIrWriteFailed",
                    format!("{}: {message}", ir_path.display()),
                )
            })?;
            trace_llvm_stage("lcir.ir-write.end");
        }
        create_parent_directory(output)?;
        trace_llvm_stage("lcir.object-write.begin");
        target
            .machine
            .write_to_file(&backend.module, FileType::Object, output)
            .map_err(|message| CodegenError::new("LlvmObjectWriteFailed", message.to_string()))?;
        trace_llvm_stage("lcir.object-write.end");

        Ok(NativeObjectArtifact {
            object: output.to_path_buf(),
            functions: artifact.functions().len(),
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
    bytes_type: DIType<'ctx>,
    list_type: DIType<'ctx>,
    text_map_type: DIType<'ctx>,
    task_type: DIType<'ctx>,
    dynamic_type: DIType<'ctx>,
    status_type: DIType<'ctx>,
    fault_context_pointer_type: DIType<'ctx>,
    executor_pointer_type: DIType<'ctx>,
    fallible_unit_type: DIType<'ctx>,
    fallible_bool_type: DIType<'ctx>,
    fallible_int_type: DIType<'ctx>,
    fallible_float_type: DIType<'ctx>,
    product_types: RefCell<BTreeMap<u32, DIType<'ctx>>>,
    sum_types: RefCell<BTreeMap<u32, DIType<'ctx>>>,
    optimized: bool,
}

struct DebugParameterSite<'ctx> {
    function: FunctionValue<'ctx>,
    scope: DISubprogram<'ctx>,
    file: DIFile<'ctx>,
    line: u32,
    location: DILocation<'ctx>,
    first: InstructionValue<'ctx>,
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
            concat!("loom lcir ", env!("CARGO_PKG_VERSION")),
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
        let bytes_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "BytesObject",
                file,
                0,
                TEXT_OBJECT_HEADER_SIZE.saturating_mul(8),
                text_alignment_bits,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.BytesObject",
            )
            .as_type();
        let bytes_type = builder
            .create_pointer_type(
                "Bytes",
                bytes_object_type,
                target_data.get_bit_size(&ptr_type),
                abi_alignment_bits(target_data, &ptr_type)?,
                AddressSpace::default(),
            )
            .as_type();
        let list_header_type = context.struct_type(
            &[context.i64_type().into(), context.i64_type().into()],
            false,
        );
        let list_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "ListObject",
                file,
                0,
                target_data.get_bit_size(&list_header_type),
                abi_alignment_bits(target_data, &list_header_type)?,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.ListObject",
            )
            .as_type();
        let list_type = builder
            .create_pointer_type(
                "List",
                list_object_type,
                target_data.get_bit_size(&ptr_type),
                abi_alignment_bits(target_data, &ptr_type)?,
                AddressSpace::default(),
            )
            .as_type();
        let text_map_header_type = context.struct_type(&[context.i64_type().into()], false);
        let text_map_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "TextMapObject",
                file,
                0,
                target_data.get_bit_size(&text_map_header_type),
                abi_alignment_bits(target_data, &text_map_header_type)?,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.TextMapObject",
            )
            .as_type();
        let text_map_type = builder
            .create_pointer_type(
                "TextMap",
                text_map_object_type,
                target_data.get_bit_size(&ptr_type),
                abi_alignment_bits(target_data, &ptr_type)?,
                AddressSpace::default(),
            )
            .as_type();
        let task_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "TaskObject",
                file,
                0,
                0,
                8,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.TaskObject",
            )
            .as_type();
        let task_type = builder
            .create_pointer_type(
                "Task",
                task_object_type,
                target_data.get_bit_size(&ptr_type),
                abi_alignment_bits(target_data, &ptr_type)?,
                AddressSpace::default(),
            )
            .as_type();
        let dynamic_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "DynamicObject",
                file,
                0,
                0,
                8,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.DynamicObject",
            )
            .as_type();
        let dynamic_type = builder
            .create_pointer_type(
                "Dynamic",
                dynamic_object_type,
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
        let executor_object_type = builder
            .create_struct_type(
                file.as_debug_info_scope(),
                "LoomExecutor",
                file,
                0,
                0,
                8,
                DIFlags::ARTIFICIAL,
                None,
                &[],
                0,
                None,
                "loom.compiler.LoomExecutor",
            )
            .as_type();
        let executor_pointer_type = builder
            .create_pointer_type(
                "LoomExecutor*",
                executor_object_type,
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
            bytes_type,
            list_type,
            text_map_type,
            task_type,
            dynamic_type,
            status_type,
            fault_context_pointer_type,
            executor_pointer_type,
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

    fn managed_pointer_debug_type(
        &self,
        backend: &Backend<'ctx, '_>,
        ty: ValueTypeId,
    ) -> Result<(&'static str, DIType<'ctx>), CodegenError> {
        let mut ty = ty;
        let value_type = loop {
            let value_type = backend
                .artifact
                .representations()
                .value_type(ty)
                .ok_or_else(|| {
                    CodegenError::new("LlvmDebugInfoFailed", format!("missing LCIR type {ty}"))
                })?;
            let ValueTypeKind::Transparent { base } = value_type.kind() else {
                break value_type;
            };
            ty = base;
        };
        if backend.artifact.representations().is_managed_bytes_type(
            backend
                .artifact
                .program()
                .as_program()
                .canonical_types()
                .bytes,
            ty,
        ) {
            Ok(("Bytes", self.bytes_type))
        } else if value_type.kind() == ValueTypeKind::ManagedTextMap {
            Ok(("TextMap", self.text_map_type))
        } else {
            match value_type.semantic() {
                Type::Text => Ok(("Text", self.text_type)),
                Type::List(_) => Ok(("List", self.list_type)),
                Type::View { .. } => Ok(("Dynamic", self.dynamic_type)),
                semantic => Err(CodegenError::new(
                    "LlvmDebugInfoFailed",
                    format!("managed LCIR type {semantic:?} has no debug representation"),
                )),
            }
        }
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
            Some(Repr::ManagedPointer) => self
                .managed_pointer_debug_type(backend, ty)
                .map(|(_, debug_type)| debug_type),
            Some(Repr::TaskHandle) => Ok(self.task_type),
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
            Some(Repr::ManagedPointer) => {
                let (name, debug_type) = self.managed_pointer_debug_type(backend, ty)?;
                create_fallible_debug_type(
                    backend.context,
                    &self.builder,
                    self.type_file,
                    &backend.target_data,
                    name,
                    debug_type,
                    backend.ptr_type.into(),
                    self.status_type,
                )
            }
            Some(Repr::TaskHandle) => create_fallible_debug_type(
                backend.context,
                &self.builder,
                self.type_file,
                &backend.target_data,
                "Task",
                self.task_type,
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
        if source.effects().contains(Effects::NEEDS_EXECUTOR) {
            parameters.push(self.executor_pointer_type);
        }
        // This describes the exact callable ABI: direct results stay direct,
        // inout writebacks extend the physical return aggregate, and fallible
        // returns prepend status. Hidden status/writeback fields and the
        // fault-context and executor parameters are marked artificial.
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

        self.attach_hidden_parameter_values(
            source,
            &DebugParameterSite {
                function,
                scope,
                file,
                line,
                location,
                first,
            },
        )
    }

    fn attach_hidden_parameter_values(
        &self,
        source: &Function,
        site: &DebugParameterSite<'ctx>,
    ) -> Result<(), CodegenError> {
        if !source.effects().contains(Effects::MAY_FAULT)
            && !source.effects().contains(Effects::NEEDS_EXECUTOR)
        {
            return Ok(());
        }
        if source.effects().contains(Effects::MAY_FAULT) {
            let llvm_index = u32::try_from(source.signature().params().len())
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let argument_number = llvm_index
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = site.function.get_nth_param(llvm_index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing its fault-context pointer", source.id()),
                )
            })?;
            let variable = self.builder.create_parameter_variable(
                site.scope.as_debug_info_scope(),
                "__loom_fault_context",
                argument_number,
                site.file,
                site.line,
                self.fault_context_pointer_type,
                true,
                DIFlags::ARTIFICIAL,
            );
            insert_dbg_value_before(&self.builder, value, variable, site.location, site.first);
        }
        if source.effects().contains(Effects::NEEDS_EXECUTOR) {
            let llvm_index = source
                .signature()
                .params()
                .len()
                .checked_add(usize::from(source.effects().contains(Effects::MAY_FAULT)))
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let llvm_index = u32::try_from(llvm_index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let argument_number = llvm_index
                .checked_add(1)
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = site.function.get_nth_param(llvm_index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing its executor pointer", source.id()),
                )
            })?;
            let variable = self.builder.create_parameter_variable(
                site.scope.as_debug_info_scope(),
                "__loom_executor",
                argument_number,
                site.file,
                site.line,
                self.executor_pointer_type,
                true,
                DIFlags::ARTIFICIAL,
            );
            insert_dbg_value_before(&self.builder, value, variable, site.location, site.first);
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

// This is an emitter work/resource boundary, not a language-visible ABI. It
// prevents a hostile checked artifact from turning target-layout planning into
// an unbounded allocation or search before LLVM verification.
const SUM_CARRIER_MAX_BYTES: u64 = 64 * 1024;
const SUM_CARRIER_MAX_PLACEMENT_WORK: u64 = 64 * 1024;
const SUM_LAYOUT_MAX_GRAPH_WORK: u64 = 65_536;
const SUM_CARRIER_MAX_EMISSION_BYTE_WORK: u64 = 65_536;

const SUM_BYTE_NON_POINTER: u8 = 1;
const SUM_BYTE_POINTER: u8 = 2;

#[derive(Clone, Debug)]
struct SumPayloadShape {
    size: u64,
    alignment: u32,
    pointer_offsets: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SumCarrierPlan {
    payload_byte_offsets: Vec<u64>,
    byte_len: u64,
    alignment: u32,
    anchor_variant: usize,
    placement_work: u64,
}

fn checked_align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment.checked_sub(1)?)
        .map(|rounded| rounded / alignment * alignment)
}

fn checked_sum_carrier_emission_work(current: u64, amount: u64) -> Result<u64, CodegenError> {
    let work = current.checked_add(amount).ok_or_else(|| {
        CodegenError::new("ProgramTooLarge", "sum carrier emission work overflowed")
    })?;
    if work > SUM_CARRIER_MAX_EMISSION_BYTE_WORK {
        return Err(CodegenError::new(
            "ProgramTooLarge",
            format!(
                "sum carrier pack/unpack exceeds the shared {SUM_CARRIER_MAX_EMISSION_BYTE_WORK}-byte emission limit"
            ),
        ));
    }
    Ok(work)
}

#[expect(
    clippy::too_many_lines,
    reason = "the complete bounded collision proof is intentionally kept at the one physical sum-layout boundary"
)]
fn plan_sum_carrier(
    payloads: &[SumPayloadShape],
    pointer_size: u64,
    pointer_alignment: u64,
    placement_work_limit: u64,
) -> Result<SumCarrierPlan, CodegenError> {
    if placement_work_limit > SUM_CARRIER_MAX_PLACEMENT_WORK {
        return Err(CodegenError::new(
            "LlvmAbiDefect",
            "sum carrier placement received an invalid shared work limit",
        ));
    }
    if payloads.is_empty() {
        return Err(CodegenError::new(
            "LlvmAbiDefect",
            "tagged payload sum has no variants",
        ));
    }
    if pointer_size == 0 || pointer_alignment == 0 || !pointer_alignment.is_power_of_two() {
        return Err(CodegenError::new(
            "LlvmAbiDefect",
            "sum carrier target pointer size/alignment is invalid",
        ));
    }

    let mut alignment = 0_u32;
    let mut anchor_variant = 0_usize;
    for (index, payload) in payloads.iter().enumerate() {
        let payload_alignment = u64::from(payload.alignment);
        if payload_alignment == 0 || !payload_alignment.is_power_of_two() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("sum variant {index} has invalid payload alignment {payload_alignment}"),
            ));
        }
        if payload.size > SUM_CARRIER_MAX_BYTES {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!(
                    "sum variant {index} payload exceeds the {SUM_CARRIER_MAX_BYTES}-byte carrier limit"
                ),
            ));
        }
        for offset in &payload.pointer_offsets {
            if !offset.is_multiple_of(pointer_alignment)
                || offset
                    .checked_add(pointer_size)
                    .is_none_or(|end| end > payload.size)
            {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    format!(
                        "sum variant {index} has invalid managed pointer offset {offset} for payload size {}/pointer size {pointer_size}",
                        payload.size
                    ),
                ));
            }
        }
        if payload.alignment > alignment {
            alignment = payload.alignment;
            anchor_variant = index;
        }
    }
    if alignment == 0 {
        return Err(CodegenError::new(
            "LlvmAbiDefect",
            "sum carrier has no payload alignment",
        ));
    }

    // Pure non-pointer payloads establish the reusable scalar byte class
    // first. Pointer-bearing variants then choose the lowest aligned offset
    // that never aliases a pointer byte with a non-pointer byte. Bytes of the
    // same class may overlap, retaining compact ordinary enum ABIs while
    // making the union pointer map exact for repeated storage.
    let mut placement_order = (0..payloads.len()).collect::<Vec<_>>();
    placement_order.sort_by_key(|index| (!payloads[*index].pointer_offsets.is_empty(), *index));
    let mut payload_byte_offsets = vec![0_u64; payloads.len()];
    let mut occupied = Vec::<u8>::new();
    let mut byte_len = 0_u64;
    let mut work = 0_u64;

    for variant in placement_order {
        let payload = &payloads[variant];
        let payload_len = usize::try_from(payload.size).map_err(|_| {
            CodegenError::new(
                "ProgramTooLarge",
                format!("sum variant {variant} payload does not fit host layout memory"),
            )
        })?;
        work = work.checked_add(payload.size).ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "sum carrier placement work overflowed")
        })?;
        if work > placement_work_limit {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!(
                    "sum carrier placement exceeds the {SUM_CARRIER_MAX_PLACEMENT_WORK}-step work limit"
                ),
            ));
        }
        let mut classes = vec![SUM_BYTE_NON_POINTER; payload_len];
        for offset in &payload.pointer_offsets {
            let start = usize::try_from(*offset).map_err(|_| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "sum managed pointer offset does not fit host layout memory",
                )
            })?;
            let end = usize::try_from(offset.checked_add(pointer_size).ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "sum managed pointer extent overflowed")
            })?)
            .map_err(|_| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "sum managed pointer extent does not fit host layout memory",
                )
            })?;
            classes[start..end].fill(SUM_BYTE_POINTER);
        }

        let step = u64::from(payload.alignment);
        let mut candidate = 0_u64;
        loop {
            let end = candidate.checked_add(payload.size).ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "sum carrier candidate extent overflowed")
            })?;
            if end > SUM_CARRIER_MAX_BYTES {
                return Err(CodegenError::new(
                    "ProgramTooLarge",
                    format!("sum carrier placement exceeds the {SUM_CARRIER_MAX_BYTES}-byte limit"),
                ));
            }
            let mut conflict = false;
            for (local, class) in classes.iter().copied().enumerate() {
                work = work.checked_add(1).ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "sum carrier placement work overflowed")
                })?;
                if work > placement_work_limit {
                    return Err(CodegenError::new(
                        "ProgramTooLarge",
                        format!(
                            "sum carrier placement exceeds the {SUM_CARRIER_MAX_PLACEMENT_WORK}-step work limit"
                        ),
                    ));
                }
                let absolute = usize::try_from(
                    candidate
                        .checked_add(u64::try_from(local).map_err(|_| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "sum carrier local byte index overflowed",
                            )
                        })?)
                        .ok_or_else(|| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "sum carrier absolute byte offset overflowed",
                            )
                        })?,
                )
                .map_err(|_| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "sum carrier byte offset does not fit host layout memory",
                    )
                })?;
                let prior = occupied.get(absolute).copied().unwrap_or(0);
                if (class == SUM_BYTE_POINTER && prior & SUM_BYTE_NON_POINTER != 0)
                    || (class == SUM_BYTE_NON_POINTER && prior & SUM_BYTE_POINTER != 0)
                {
                    conflict = true;
                    break;
                }
            }
            if !conflict {
                let end_usize = usize::try_from(end).map_err(|_| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "sum carrier extent does not fit host layout memory",
                    )
                })?;
                occupied.resize(occupied.len().max(end_usize), 0);
                let candidate_usize = usize::try_from(candidate).map_err(|_| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "sum carrier offset does not fit host layout memory",
                    )
                })?;
                work = work.checked_add(payload.size).ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "sum carrier placement work overflowed")
                })?;
                if work > placement_work_limit {
                    return Err(CodegenError::new(
                        "ProgramTooLarge",
                        format!(
                            "sum carrier placement exceeds the {SUM_CARRIER_MAX_PLACEMENT_WORK}-step work limit"
                        ),
                    ));
                }
                for (local, class) in classes.iter().copied().enumerate() {
                    occupied[candidate_usize + local] |= class;
                }
                payload_byte_offsets[variant] = candidate;
                byte_len = byte_len.max(end);
                break;
            }
            candidate = candidate.checked_add(step).ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "sum carrier search offset overflowed")
            })?;
        }
    }

    let rounded = checked_align_up(byte_len, u64::from(alignment)).ok_or_else(|| {
        CodegenError::new("ProgramTooLarge", "sum carrier aligned size overflowed")
    })?;
    if rounded > SUM_CARRIER_MAX_BYTES {
        return Err(CodegenError::new(
            "ProgramTooLarge",
            format!("aligned sum carrier exceeds the {SUM_CARRIER_MAX_BYTES}-byte limit"),
        ));
    }
    Ok(SumCarrierPlan {
        payload_byte_offsets,
        byte_len,
        alignment,
        anchor_variant,
        placement_work: work,
    })
}

#[derive(Clone)]
struct SumLayout<'ctx> {
    tag: SumTagRepr,
    payloads: Vec<StructType<'ctx>>,
    payload_byte_offsets: Vec<u64>,
    carrier: Option<StructType<'ctx>>,
    physical: BasicTypeEnum<'ctx>,
}

impl SumLayout<'_> {
    fn payload_byte_offset(&self, variant: usize) -> Result<u64, CodegenError> {
        self.payload_byte_offsets
            .get(variant)
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("sum variant {variant} has no carrier byte offset"),
                )
            })
    }
}

#[derive(Clone)]
struct ListLayout<'ctx> {
    object: StructType<'ctx>,
    element: BasicTypeEnum<'ctx>,
    fixed_size: u64,
    object_align: u64,
    element_stride: u64,
    element_align: u32,
    pointer_offsets: Vec<u64>,
}

#[derive(Clone)]
struct TextMapLayout<'ctx> {
    object: StructType<'ctx>,
    entry: StructType<'ctx>,
    value: BasicTypeEnum<'ctx>,
    fixed_size: u64,
    object_align: u64,
    entry_stride: u64,
    entry_align: u32,
    pointer_offsets: Vec<u64>,
}

#[derive(Clone)]
struct CoroutineSuspensionLayout {
    state: u32,
    child_fields: Vec<u32>,
    live_fields: Vec<u32>,
}

#[derive(Clone)]
struct CoroutineLayout<'ctx> {
    frame: StructType<'ctx>,
    parameter_fields: Vec<u32>,
    caller_span_fields: Option<[u32; 3]>,
    suspensions: Vec<CoroutineSuspensionLayout>,
    result_field: u32,
    descriptor: PointerValue<'ctx>,
}

#[derive(Clone)]
struct TaskJoinLayout<'ctx> {
    mode: AwaitMode,
    fault_origin: Option<Origin>,
    frame: StructType<'ctx>,
    inputs: TaskJoinInputs<'ctx>,
    result_field: u32,
    collecting_root_state: Option<u64>,
    output_types: Vec<ValueTypeId>,
    result_type: ValueTypeId,
    callback: FunctionValue<'ctx>,
    descriptor: PointerValue<'ctx>,
}

#[derive(Clone)]
enum TaskJoinInputs<'ctx> {
    Fixed {
        child_fields: Vec<u32>,
    },
    Dynamic {
        source_field: u32,
        source_type: ValueTypeId,
        source_layout: ListLayout<'ctx>,
        output_type: ValueTypeId,
    },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TaskJoinShape {
    mode: AwaitMode,
    source_list_type: Option<ValueTypeId>,
    output_types: Vec<ValueTypeId>,
    result_type: ValueTypeId,
    fault_origin: Option<(u32, Option<u32>, u32, u32, u32)>,
}

#[derive(Clone)]
struct IoTaskLayout<'ctx> {
    operation: IoTaskOperation,
    error_mode: IoTaskErrorMode,
    frame: StructType<'ctx>,
    result_type: ValueTypeId,
    success_type: ValueTypeId,
    error_type: Option<ValueTypeId>,
    error_kind_type: Option<ValueTypeId>,
    result_field: u32,
    scratch_field: u32,
    callback: FunctionValue<'ctx>,
    descriptor: PointerValue<'ctx>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct IoTaskShape {
    operation: IoTaskOperation,
    error_mode: IoTaskErrorMode,
    result_type: ValueTypeId,
}

#[derive(Clone)]
struct DynamicCandidateLayout<'ctx> {
    object: StructType<'ctx>,
    payload: BasicTypeEnum<'ctx>,
    size: u64,
    align: u64,
    pointer_offsets: Vec<u64>,
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
    coroutine_callbacks: Vec<Option<FunctionValue<'ctx>>>,
    coroutine_layouts: Vec<Option<CoroutineLayout<'ctx>>>,
    coroutine_cancel: Option<FunctionValue<'ctx>>,
    task_join_layouts: BTreeMap<TaskJoinShape, TaskJoinLayout<'ctx>>,
    task_join_shapes: BTreeMap<InstructionId, TaskJoinShape>,
    io_task_layouts: BTreeMap<IoTaskShape, IoTaskLayout<'ctx>>,
    io_task_shapes: BTreeMap<InstructionId, IoTaskShape>,
    debug: Option<DebugState<'ctx>>,
    names: Cell<u64>,
    sum_layout_cache: RefCell<BTreeMap<u32, SumLayout<'ctx>>>,
    sum_layout_in_progress: RefCell<BTreeSet<u32>>,
    managed_offset_cache: RefCell<BTreeMap<u32, Vec<u64>>>,
    managed_offset_in_progress: RefCell<BTreeSet<u32>>,
    sum_layout_graph_work: Cell<u64>,
    sum_carrier_placement_work: Cell<u64>,
    sum_carrier_emission_work: Cell<u64>,
}

impl<'ctx, 'artifact> Backend<'ctx, 'artifact> {
    #[expect(
        clippy::too_many_lines,
        reason = "backend construction validates the complete target ABI before publishing any LLVM module state"
    )]
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
        let has_text = artifact
            .representations()
            .value_types()
            .iter()
            .any(|value| value.semantic() == &Type::Text);
        let canonical_bytes = artifact.program().as_program().canonical_types().bytes;
        let has_bytes = canonical_bytes
            .and_then(|bytes| {
                artifact
                    .representations()
                    .type_id(&Type::Nominal(bytes, Vec::new()))
            })
            .is_some_and(|ty| {
                artifact
                    .representations()
                    .is_managed_bytes_type(canonical_bytes, ty)
            });
        if has_text || has_bytes {
            let actual_size = target_data.get_abi_size(&text_object_type);
            let actual_alignment = u64::from(target_data.get_abi_alignment(&text_object_type));
            if actual_size != TEXT_OBJECT_HEADER_SIZE || actual_alignment != TEXT_OBJECT_ALIGNMENT {
                return Err(CodegenError::new(
                    "LcirByteSequenceAbiMismatch",
                    format!(
                        "LLVM target {} gives the runtime Text/Bytes header size/alignment {actual_size}/{actual_alignment}, expected {TEXT_OBJECT_HEADER_SIZE}/{TEXT_OBJECT_ALIGNMENT}",
                        target.triple
                    ),
                ));
            }
        }
        let has_repeated_values = artifact
            .representations()
            .value_types()
            .iter()
            .any(|value| {
                matches!(value.semantic(), Type::List(_))
                    || value.kind() == ValueTypeKind::ManagedTextMap
            });
        if has_repeated_values {
            let pointer_size = target_data.get_abi_size(&ptr_type);
            let pointer_align = target_data.get_abi_alignment(&ptr_type);
            let descriptor_type = context.struct_type(
                &[
                    context.i32_type().into(),
                    context.i32_type().into(),
                    context.i64_type().into(),
                    context.i64_type().into(),
                    context.i64_type().into(),
                    ptr_type.into(),
                    context.i64_type().into(),
                    context.i64_type().into(),
                    ptr_type.into(),
                ],
                false,
            );
            let descriptor_size = target_data.get_abi_size(&descriptor_type);
            let descriptor_align = target_data.get_abi_alignment(&descriptor_type);
            if pointer_size != 8
                || pointer_align != 8
                || descriptor_size != 64
                || descriptor_align != 8
            {
                return Err(CodegenError::new(
                    "LcirRepeatedAbiMismatch",
                    format!(
                        "LLVM target {} gives pointer size/alignment {pointer_size}/{pointer_align} and repeated descriptor size/alignment {descriptor_size}/{descriptor_align}; typed repeated storage requires 8/8 and 64/8",
                        target.triple
                    ),
                ));
            }
        }
        let has_typed_io = artifact.functions().iter().any(|function| {
            function.instructions().iter().any(|instruction| {
                matches!(instruction.kind(), InstructionKind::IoTaskCreate { .. })
            })
        });
        if has_typed_io {
            let byte_view =
                context.struct_type(&[ptr_type.into(), context.i64_type().into()], false);
            let request = context.struct_type(
                &[
                    context.i32_type().into(),
                    context.i32_type().into(),
                    context.i64_type().into(),
                    byte_view.into(),
                    context.i64_type().into(),
                ],
                false,
            );
            let outcome = context.struct_type(
                &[
                    context.i32_type().into(),
                    context.i32_type().into(),
                    context.i64_type().into(),
                ],
                false,
            );
            let pointer_size = target_data.get_abi_size(&ptr_type);
            let pointer_align = target_data.get_abi_alignment(&ptr_type);
            let request_size = target_data.get_abi_size(&request);
            let request_align = target_data.get_abi_alignment(&request);
            let outcome_size = target_data.get_abi_size(&outcome);
            let outcome_align = target_data.get_abi_alignment(&outcome);
            if pointer_size != 8
                || pointer_align != 8
                || request_size != 40
                || request_align != 8
                || outcome_size != 16
                || outcome_align != 8
            {
                return Err(CodegenError::new(
                    "LcirTypedIoAbiMismatch",
                    format!(
                        "LLVM target {} gives pointer {pointer_size}/{pointer_align}, typed I/O request {request_size}/{request_align}, and outcome {outcome_size}/{outcome_align}; required 8/8, 40/8, and 16/8",
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
            coroutine_callbacks: Vec::with_capacity(artifact.functions().len()),
            coroutine_layouts: Vec::with_capacity(artifact.functions().len()),
            coroutine_cancel: None,
            task_join_layouts: BTreeMap::new(),
            task_join_shapes: BTreeMap::new(),
            io_task_layouts: BTreeMap::new(),
            io_task_shapes: BTreeMap::new(),
            debug,
            names: Cell::new(0),
            sum_layout_cache: RefCell::new(BTreeMap::new()),
            sum_layout_in_progress: RefCell::new(BTreeSet::new()),
            managed_offset_cache: RefCell::new(BTreeMap::new()),
            managed_offset_in_progress: RefCell::new(BTreeSet::new()),
            sum_layout_graph_work: Cell::new(0),
            sum_carrier_placement_work: Cell::new(0),
            sum_carrier_emission_work: Cell::new(0),
        };
        backend.declare_functions()?;
        Ok(backend)
    }

    fn declare_functions(&mut self) -> Result<(), CodegenError> {
        if self
            .artifact
            .functions()
            .iter()
            .any(|function| function.coroutine().is_some())
        {
            let callback_type = self.context.i32_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                ],
                false,
            );
            self.coroutine_cancel = Some(self.module.add_function(
                "loom.lcir.coroutine.cancel",
                callback_type,
                Some(Linkage::Internal),
            ));
        }
        for source in self.artifact.functions() {
            if source.coroutine().is_some() {
                let callback_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                let callback = self.module.add_function(
                    &format!("loom.lcir.coroutine.resume.{}", source.id().raw()),
                    callback_type,
                    Some(Linkage::Internal),
                );
                let mut params = source
                    .signature()
                    .params()
                    .iter()
                    .copied()
                    .map(|ty| self.llvm_type(ty).map(Into::into))
                    .collect::<Result<Vec<BasicMetadataTypeEnum<'ctx>>, _>>()?;
                if source
                    .coroutine()
                    .is_some_and(CoroutinePlan::carries_caller_span)
                {
                    for _ in 0..3 {
                        params.push(self.context.i64_type().into());
                    }
                }
                params.push(self.ptr_type.into());
                let constructor = self.module.add_function(
                    &format!("loom.lcir.fn.{}", source.id().raw()),
                    self.ptr_type.fn_type(&params, false),
                    Some(Linkage::Internal),
                );
                // The same checked callback owns both ordinary resume and
                // descriptor-driven cancellation dispatch. Its prologue
                // selects the explicit LCIR cancellation CFG before any
                // operation which could suspend or mutate task topology.
                let layout = self.build_coroutine_layout(source, callback, callback)?;
                self.functions.push(constructor);
                self.coroutine_callbacks.push(Some(callback));
                self.coroutine_layouts.push(Some(layout));
                continue;
            }
            let function = self.declare_synchronous_function(source)?;
            self.functions.push(function);
            self.coroutine_callbacks.push(None);
            self.coroutine_layouts.push(None);
        }
        self.declare_task_join_layouts()?;
        self.declare_io_task_layouts()
    }

    fn declare_synchronous_function(
        &self,
        source: &Function,
    ) -> Result<FunctionValue<'ctx>, CodegenError> {
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
        if source.effects().contains(Effects::NEEDS_EXECUTOR) {
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
        Ok(function)
    }

    fn declare_task_join_layouts(&mut self) -> Result<(), CodegenError> {
        let joins = self
            .artifact
            .functions()
            .iter()
            .flat_map(|source| {
                source.instructions().iter().filter_map(move |instruction| {
                    matches!(
                        instruction.kind(),
                        InstructionKind::TaskJoin { .. } | InstructionKind::TaskJoinList { .. }
                    )
                    .then_some((source, instruction))
                })
            })
            .collect::<Vec<_>>();
        for (source, instruction) in joins {
            let shape = self.task_join_shape(source, instruction)?;
            if self.task_join_shapes.contains_key(&instruction.id()) {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    format!("duplicate typed Task join instruction {}", instruction.id()),
                ));
            }
            if !self.task_join_layouts.contains_key(&shape) {
                let shape_index = self.task_join_layouts.len();
                let mode_name = task_join_mode_name(shape.mode);
                let namespace = if shape.source_list_type.is_some() {
                    "task_join_list"
                } else {
                    "task_join"
                };
                let cancel = self.coroutine_cancel.ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        "typed Task join descriptor has no cancellation callback",
                    )
                })?;
                let callback = self.module.add_function(
                    &format!("loom.lcir.{namespace}.{mode_name}.resume.{shape_index}"),
                    self.context.i32_type().fn_type(
                        &[
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                        ],
                        false,
                    ),
                    Some(Linkage::Internal),
                );
                let layout = self.build_task_join_layout(
                    &shape,
                    instruction.origin(),
                    shape_index,
                    callback,
                    cancel,
                )?;
                self.task_join_layouts.insert(shape.clone(), layout);
            }
            self.task_join_shapes.insert(instruction.id(), shape);
        }
        Ok(())
    }

    fn declare_io_task_layouts(&mut self) -> Result<(), CodegenError> {
        let tasks = self
            .artifact
            .functions()
            .iter()
            .flat_map(|source| {
                source.instructions().iter().filter_map(move |instruction| {
                    matches!(instruction.kind(), InstructionKind::IoTaskCreate { .. })
                        .then_some((source, instruction))
                })
            })
            .collect::<Vec<_>>();
        for (source, instruction) in tasks {
            let shape = self.io_task_shape(source, instruction)?;
            if self.io_task_shapes.contains_key(&instruction.id()) {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    format!("duplicate typed I/O instruction {}", instruction.id()),
                ));
            }
            if !self.io_task_layouts.contains_key(&shape) {
                let shape_index = self.io_task_layouts.len();
                let callback = self.module.add_function(
                    &format!("loom.lcir.io.resume.{shape_index}"),
                    self.context.i32_type().fn_type(
                        &[
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                        ],
                        false,
                    ),
                    Some(Linkage::Internal),
                );
                let layout = self.build_io_task_layout(shape, shape_index, callback)?;
                self.io_task_layouts.insert(shape, layout);
            }
            self.io_task_shapes.insert(instruction.id(), shape);
        }
        Ok(())
    }

    fn io_task_shape(
        &self,
        source: &Function,
        instruction: &Instruction,
    ) -> Result<IoTaskShape, CodegenError> {
        let InstructionKind::IoTaskCreate {
            operation,
            error_mode,
            ..
        } = instruction.kind()
        else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("{} is not a typed I/O instruction", instruction.id()),
            ));
        };
        let result = instruction
            .results()
            .first()
            .and_then(|result| source.value(*result))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O has no Task result"))?;
        let semantic = self
            .artifact
            .representations()
            .value_type(result.ty())
            .map(loom_codegen_ir::ValueType::semantic)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O Task type is missing"))?;
        let Type::Task(output) = semantic else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed I/O result is not a Task handle",
            ));
        };
        let result_type = self
            .artifact
            .representations()
            .type_id(output)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "typed I/O Result type is missing")
            })?;
        Ok(IoTaskShape {
            operation: *operation,
            error_mode: *error_mode,
            result_type,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one target-layout boundary proves the exact typed I/O frame, roots, closed result shape, and immutable descriptor"
    )]
    fn build_io_task_layout(
        &self,
        shape: IoTaskShape,
        shape_index: usize,
        callback: FunctionValue<'ctx>,
    ) -> Result<IoTaskLayout<'ctx>, CodegenError> {
        let (success_type, error_type, error_kind_type) = match shape.error_mode {
            IoTaskErrorMode::Result => {
                let result_sum = self.sum_repr(shape.result_type)?;
                let variants = result_sum.variants();
                let [success_variant, error_variant] = variants else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "Result-mode typed I/O must have exact one-field success and error variants",
                    ));
                };
                let ([success_field], [error_field]) =
                    (success_variant.fields(), error_variant.fields())
                else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "Result-mode typed I/O must have exact one-field success and error variants",
                    ));
                };
                let success_type = *success_field;
                let error_type = *error_field;
                let error_fields = self.product_repr(error_type)?.fields();
                let [error_kind_type, error_message_type] = error_fields else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "typed I/O error must have exact kind and message fields",
                    ));
                };
                let error_kind_type = *error_kind_type;
                let error_kind = self.sum_repr(error_kind_type)?;
                if error_kind.tag() != SumTagRepr::I8
                    || error_kind.variants().len() != 10
                    || error_kind
                        .variants()
                        .iter()
                        .any(|variant| !variant.fields().is_empty())
                    || self
                        .artifact
                        .representations()
                        .value_type(*error_message_type)
                        .is_none_or(|value| value.semantic() != &Type::Text)
                    || self.repr_of(*error_message_type)? != Repr::ManagedPointer
                {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "typed I/O error does not use canonical IoErrorKind and managed Text",
                    ));
                }
                (success_type, Some(error_type), Some(error_kind_type))
            }
            IoTaskErrorMode::Fault => (shape.result_type, None, None),
        };
        let success_repr = self.repr_of(success_type)?;
        let success_valid = match shape.operation {
            IoTaskOperation::FileOpenRead
            | IoTaskOperation::FileCreate
            | IoTaskOperation::SocketConnect => {
                let integer = self
                    .artifact
                    .representations()
                    .type_id(&Type::Int)
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Int type is missing"))?;
                matches!(success_repr, Repr::Product(_))
                    && self.product_repr(success_type)?.fields() == [integer]
            }
            IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => {
                success_repr == Repr::ManagedPointer
                    && self
                        .artifact
                        .representations()
                        .value_type(success_type)
                        .is_some_and(|value| value.semantic() == &Type::Text)
            }
            IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => {
                success_repr == Repr::Zst
                    && self
                        .artifact
                        .representations()
                        .value_type(success_type)
                        .is_some_and(|value| value.semantic() == &Type::Unit)
            }
        };
        if !success_valid {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed I/O operation does not match its direct success representation",
            ));
        }

        let result_physical = self.llvm_type(shape.result_type)?;
        let frame = self
            .context
            .struct_type(&[result_physical, self.ptr_type.into()], false);
        let result_field = 0_u32;
        let scratch_field = 1_u32;
        let frame_size = self.target_data.get_abi_size(&frame);
        let frame_align = u64::from(self.target_data.get_abi_alignment(&frame));
        let result_offset = self.coroutine_field_offset(frame, result_field)?;
        let scratch_offset = self.coroutine_field_offset(frame, scratch_field)?;
        let result_size = self.target_data.get_abi_size(&result_physical);
        let result_align = u64::from(self.target_data.get_abi_alignment(&result_physical));
        if frame_size == 0
            || frame_size > GC_MAX_OBJECT_BYTES
            || frame_align == 0
            || !frame_align.is_power_of_two()
            || frame_align > GC_MAX_OBJECT_ALIGNMENT
        {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!(
                    "typed I/O frame has unsupported size/alignment {frame_size}/{frame_align}"
                ),
            ));
        }

        let completed_roots = self
            .managed_element_offsets(shape.result_type)?
            .into_iter()
            .map(|offset| {
                result_offset.checked_add(offset).ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "typed I/O result root offset overflowed")
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut all_offsets = completed_roots.clone();
        all_offsets.insert(scratch_offset);
        let offsets = all_offsets.into_iter().collect::<Vec<_>>();
        let indexes = offsets
            .iter()
            .copied()
            .enumerate()
            .map(|(index, offset)| (offset, index))
            .collect::<BTreeMap<_, _>>();
        let bitmap_words = offsets.len().div_ceil(64);
        let mut bitmaps = vec![0_u64; 2 * bitmap_words];
        let scratch_index = indexes[&scratch_offset];
        bitmaps[scratch_index / 64] |= 1_u64 << (scratch_index % 64);
        for root in completed_roots {
            let index = indexes[&root];
            bitmaps[bitmap_words + index / 64] |= 1_u64 << (index % 64);
        }
        let stem = format!("loom.lcir.io.{shape_index}");
        let offsets_pointer = self.emit_i64_array(&format!("{stem}.root_offsets"), &offsets)?;
        let bitmaps_pointer = self.emit_i64_array(&format!("{stem}.live_bitmaps"), &bitmaps)?;
        let descriptor_type = self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
            ],
            false,
        );
        let descriptor =
            self.module
                .add_global(descriptor_type, None, &format!("{stem}.descriptor"));
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.context
                    .i32_type()
                    .const_int(u64::from(TYPED_TASK_ABI_VERSION), false)
                    .into(),
                self.context.i32_type().const_zero().into(),
                callback.as_global_value().as_pointer_value().into(),
                self.typed_io_cancel()
                    .as_global_value()
                    .as_pointer_value()
                    .into(),
                self.ptr_type.const_null().into(),
                self.context.i64_type().const_int(frame_size, false).into(),
                self.context.i64_type().const_int(frame_align, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_offset, false)
                    .into(),
                self.context.i64_type().const_int(result_size, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_align, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(offsets.len() as u64, false)
                    .into(),
                self.context.i64_type().const_int(2, false).into(),
                self.context
                    .i64_type()
                    .const_int(bitmap_words as u64, false)
                    .into(),
                offsets_pointer.into(),
                bitmaps_pointer.into(),
                self.context.i64_type().const_int(1, false).into(),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(IoTaskLayout {
            operation: shape.operation,
            error_mode: shape.error_mode,
            frame,
            result_type: shape.result_type,
            success_type,
            error_type,
            error_kind_type,
            result_field,
            scratch_field,
            callback,
            descriptor: descriptor.as_pointer_value(),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "exact Task-join validation keeps fixed/runtime-width mode semantics, canonical output identities, and result agreement in one audit boundary"
    )]
    fn task_join_shape(
        &self,
        source: &Function,
        instruction: &Instruction,
    ) -> Result<TaskJoinShape, CodegenError> {
        let (mode, source_list_type, output_types) = match instruction.kind() {
            InstructionKind::TaskJoin { mode, tasks } => {
                if tasks.is_empty() {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "typed fixed Task join cannot have an empty exact shape",
                    ));
                }
                let output_types = tasks
                    .iter()
                    .copied()
                    .map(|task| {
                        let semantic = source
                            .value(task)
                            .and_then(|value| {
                                self.artifact.representations().value_type(value.ty())
                            })
                            .map(loom_codegen_ir::ValueType::semantic)
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    format!("typed Task join child {task} has no semantic type"),
                                )
                            })?;
                        let Type::Task(output) = semantic else {
                            return Err(CodegenError::new(
                                "LlvmAbiDefect",
                                format!("typed Task join child {task} is not a Task"),
                            ));
                        };
                        self.artifact
                            .representations()
                            .type_id(output)
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    format!("typed Task join child {task} output has no LCIR type"),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (*mode, None, output_types)
            }
            InstructionKind::TaskJoinList { mode, tasks } => {
                let list = source.value(*tasks).ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("typed Task join List operand {tasks} is missing"),
                    )
                })?;
                let list_type = list.ty();
                let semantic = self
                    .artifact
                    .representations()
                    .value_type(list_type)
                    .map(loom_codegen_ir::ValueType::semantic)
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            "typed Task join List semantic type is missing",
                        )
                    })?;
                let Type::List(element) = semantic else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "typed Task join List operand is not a List",
                    ));
                };
                let Type::Task(output) = element.as_ref() else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "typed Task join List element is not an exact Task handle",
                    ));
                };
                let output = self
                    .artifact
                    .representations()
                    .type_id(output)
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            "typed Task join List output has no LCIR type",
                        )
                    })?;
                (*mode, Some(list_type), vec![output])
            }
            _ => {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is not a typed Task join instruction", instruction.id()),
                ));
            }
        };
        let result = instruction
            .results()
            .first()
            .and_then(|result| source.value(*result))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed Task join has no result"))?;
        let result_semantic = self
            .artifact
            .representations()
            .value_type(result.ty())
            .map(loom_codegen_ir::ValueType::semantic)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "typed Task join result type is missing")
            })?;
        let Type::Task(result_type) = result_semantic else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed Task join result is not a Task",
            ));
        };
        let result_type = self
            .artifact
            .representations()
            .type_id(result_type)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "typed Task join output has no LCIR type")
            })?;
        let actual = self
            .artifact
            .representations()
            .value_type(result_type)
            .map(loom_codegen_ir::ValueType::semantic)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "typed Task join output type is missing")
            })?;
        let outputs = output_types
            .iter()
            .map(|ty| {
                self.artifact
                    .representations()
                    .value_type(*ty)
                    .map(|value| value.semantic().clone())
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            "typed Task join output type disappeared",
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if matches!(mode, AwaitMode::Any | AwaitMode::Race)
            && outputs.iter().any(|output| output != &outputs[0])
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed Task.any/race children do not share one output type",
            ));
        }
        let outcome = |output: Type| {
            self.artifact
                .program()
                .as_program()
                .canonical_types()
                .task_outcome
                .map(|outcome| Type::Nominal(outcome, vec![output]))
                .ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        "typed Task.settled/race has no canonical TaskOutcome identity",
                    )
                })
        };
        let expected = match (source_list_type, mode) {
            (None, AwaitMode::All) => Type::Tuple(outputs),
            (None, AwaitMode::Settled) => Type::Tuple(
                outputs
                    .into_iter()
                    .map(outcome)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            (Some(_), AwaitMode::All) => Type::List(Box::new(outputs[0].clone())),
            (Some(_), AwaitMode::Settled) => Type::List(Box::new(outcome(outputs[0].clone())?)),
            (_, AwaitMode::Any) => outputs[0].clone(),
            (_, AwaitMode::Race) => outcome(outputs[0].clone())?,
        };
        if actual != &expected {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed Task join result does not match its mode and child outputs",
            ));
        }
        let origin = instruction.origin();
        let faults_on_empty =
            source_list_type.is_some() && matches!(mode, AwaitMode::Any | AwaitMode::Race);
        Ok(TaskJoinShape {
            mode,
            source_list_type,
            output_types,
            result_type,
            fault_origin: (mode == AwaitMode::Any || faults_on_empty).then_some((
                origin.source_function.0,
                origin.expression.map(|expression| expression.0),
                origin.span.file.0,
                origin.span.range.start,
                origin.span.range.end,
            )),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one target-data proof constructs each exact Task-join frame, state-specific roots, and immutable descriptor"
    )]
    fn build_task_join_layout(
        &self,
        shape: &TaskJoinShape,
        origin: Origin,
        shape_index: usize,
        callback: FunctionValue<'ctx>,
        cancel: FunctionValue<'ctx>,
    ) -> Result<TaskJoinLayout<'ctx>, CodegenError> {
        let TaskJoinShape {
            mode,
            source_list_type,
            output_types,
            result_type,
            fault_origin,
        } = shape;
        let mode = *mode;
        let actual_fault_origin = (
            origin.source_function.0,
            origin.expression.map(|expression| expression.0),
            origin.span.file.0,
            origin.span.range.start,
            origin.span.range.end,
        );
        if fault_origin.is_some_and(|expected| expected != actual_fault_origin) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed Task join shape disagrees with its fault origin",
            ));
        }

        let mut fields = vec![self.context.i64_type().into()];
        let mut next_field = 1_u32;
        let inputs = if let Some(source_type) = source_list_type {
            let [output_type] = output_types.as_slice() else {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "typed dynamic Task join does not have one homogeneous output type",
                ));
            };
            let source_field = next_field;
            next_field = next_field.checked_add(1).ok_or_else(|| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "typed Task join frame has too many fields",
                )
            })?;
            fields.push(self.ptr_type.into());
            TaskJoinInputs::Dynamic {
                source_field,
                source_type: *source_type,
                source_layout: self.list_layout(*source_type)?,
                output_type: *output_type,
            }
        } else {
            let mut child_fields = Vec::with_capacity(output_types.len());
            for _ in output_types {
                child_fields.push(next_field);
                next_field = next_field.checked_add(1).ok_or_else(|| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "typed Task join frame has too many fields",
                    )
                })?;
                fields.push(self.ptr_type.into());
            }
            TaskJoinInputs::Fixed { child_fields }
        };
        let result_field = next_field;
        fields.push(self.llvm_type(*result_type)?);
        let frame = self.context.struct_type(&fields, false);
        let frame_size = self.target_data.get_abi_size(&frame);
        let frame_align = u64::from(self.target_data.get_abi_alignment(&frame));
        if frame_size == 0
            || frame_size > GC_MAX_OBJECT_BYTES
            || frame_align == 0
            || !frame_align.is_power_of_two()
            || frame_align > GC_MAX_OBJECT_ALIGNMENT
        {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!(
                    "typed Task join has unsupported frame size/alignment {frame_size}/{frame_align}"
                ),
            ));
        }
        let result_offset = self.coroutine_field_offset(frame, result_field)?;
        let mut completed_roots = BTreeSet::new();
        self.collect_coroutine_root_offsets(*result_type, result_offset, &mut completed_roots)?;
        let source_root = match &inputs {
            TaskJoinInputs::Fixed { .. } => None,
            TaskJoinInputs::Dynamic { source_field, .. } => {
                Some(self.coroutine_field_offset(frame, *source_field)?)
            }
        };
        let mut all_roots = completed_roots.clone();
        if let Some(source_root) = source_root {
            all_roots.insert(source_root);
        }
        if u64::try_from(all_roots.len()).unwrap_or(u64::MAX) > GC_MAX_ROOT_SLOTS {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                "typed Task join result has too many exact managed roots",
            ));
        }
        let offsets = all_roots.into_iter().collect::<Vec<_>>();
        let indexes = offsets
            .iter()
            .copied()
            .enumerate()
            .map(|(index, offset)| (offset, index))
            .collect::<BTreeMap<_, _>>();
        let bitmap_words = offsets.len().div_ceil(64);
        let dynamic = source_list_type.is_some();
        let collecting_root_state = (dynamic || mode == AwaitMode::Settled).then_some(2_u64);
        let completed_root_state = collecting_root_state.map_or(2_u64, |_| 3_u64);
        let state_count = usize::try_from(completed_root_state.saturating_add(1))
            .map_err(|_| CodegenError::new("ProgramTooLarge", "Task join state overflowed"))?;
        let total_bitmap_words = state_count.checked_mul(bitmap_words).ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "typed Task join root bitmap overflowed")
        })?;
        if u64::try_from(total_bitmap_words).unwrap_or(u64::MAX) > GC_MAX_ROOT_BITMAP_WORDS {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                "typed Task join root bitmap exceeds the runtime ABI limit",
            ));
        }
        let mut bitmaps = vec![0_u64; total_bitmap_words];
        if let Some(source_root) = source_root {
            let slot = indexes[&source_root];
            for state in 0..=2_usize {
                bitmaps[state * bitmap_words + slot / 64] |= 1_u64 << (slot % 64);
            }
        }
        for state in collecting_root_state.unwrap_or(completed_root_state)..=completed_root_state {
            let state = usize::try_from(state).map_err(|_| {
                CodegenError::new("ProgramTooLarge", "Task join root state overflowed")
            })?;
            for root in &completed_roots {
                let slot = indexes[root];
                bitmaps[state * bitmap_words + slot / 64] |= 1_u64 << (slot % 64);
            }
        }
        let namespace = if dynamic {
            "task_join_list"
        } else {
            "task_join"
        };
        let stem = format!(
            "loom.lcir.{namespace}.{}.{shape_index}",
            task_join_mode_name(mode)
        );
        let offsets_pointer = self.emit_i64_array(&format!("{stem}.root_offsets"), &offsets)?;
        let bitmaps_pointer = self.emit_i64_array(&format!("{stem}.live_bitmaps"), &bitmaps)?;
        let result_physical = self.llvm_type(*result_type)?;
        let result_size = self.target_data.get_abi_size(&result_physical);
        let result_align = u64::from(self.target_data.get_abi_alignment(&result_physical));
        let descriptor_type = self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
            ],
            false,
        );
        let descriptor =
            self.module
                .add_global(descriptor_type, None, &format!("{stem}.descriptor"));
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.context
                    .i32_type()
                    .const_int(u64::from(TYPED_TASK_ABI_VERSION), false)
                    .into(),
                self.context.i32_type().const_zero().into(),
                callback.as_global_value().as_pointer_value().into(),
                cancel.as_global_value().as_pointer_value().into(),
                self.ptr_type.const_null().into(),
                self.context.i64_type().const_int(frame_size, false).into(),
                self.context.i64_type().const_int(frame_align, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_offset, false)
                    .into(),
                self.context.i64_type().const_int(result_size, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_align, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(offsets.len() as u64, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(state_count as u64, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(bitmap_words as u64, false)
                    .into(),
                offsets_pointer.into(),
                bitmaps_pointer.into(),
                self.context
                    .i64_type()
                    .const_int(completed_root_state, false)
                    .into(),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(TaskJoinLayout {
            mode,
            fault_origin: fault_origin.map(|_| origin),
            frame,
            inputs,
            result_field,
            collecting_root_state,
            output_types: output_types.clone(),
            result_type: *result_type,
            callback,
            descriptor: descriptor.as_pointer_value(),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one target-data proof constructs the complete typed coroutine frame and immutable runtime descriptor"
    )]
    fn build_coroutine_layout(
        &self,
        source: &Function,
        callback: FunctionValue<'ctx>,
        cancel: FunctionValue<'ctx>,
    ) -> Result<CoroutineLayout<'ctx>, CodegenError> {
        let plan = source.coroutine().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "coroutine layout requested for a sync function",
            )
        })?;
        let mut fields = vec![self.context.i64_type().into()];
        let mut next_field = 1_u32;
        let mut parameter_fields = Vec::with_capacity(source.signature().params().len());
        for parameter in source.signature().params() {
            parameter_fields.push(next_field);
            next_field = next_field.checked_add(1).ok_or_else(|| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "typed coroutine frame has too many fields",
                )
            })?;
            fields.push(self.llvm_type(*parameter)?);
        }
        let caller_span_fields = if plan.carries_caller_span() {
            let end = next_field.checked_add(3).ok_or_else(|| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "typed coroutine frame has too many fields",
                )
            })?;
            let span_fields = [next_field, next_field + 1, next_field + 2];
            next_field = end;
            for _ in 0..3 {
                fields.push(self.context.i64_type().into());
            }
            Some(span_fields)
        } else {
            None
        };
        let mut suspensions = Vec::with_capacity(plan.suspensions().len());
        for suspension in plan.suspensions() {
            let mut child_fields = Vec::with_capacity(suspension.awaited().len());
            for _ in suspension.awaited() {
                child_fields.push(next_field);
                next_field = next_field.checked_add(1).ok_or_else(|| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "typed coroutine frame has too many fields",
                    )
                })?;
                fields.push(self.ptr_type.into());
            }
            let mut live_fields = Vec::with_capacity(suspension.live().len());
            for live in suspension.live() {
                live_fields.push(next_field);
                next_field = next_field.checked_add(1).ok_or_else(|| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "typed coroutine frame has too many fields",
                    )
                })?;
                fields.push(self.llvm_type(*live)?);
            }
            suspensions.push(CoroutineSuspensionLayout {
                state: suspension.state(),
                child_fields,
                live_fields,
            });
        }
        let result_field = next_field;
        fields.push(self.llvm_type(plan.output())?);
        let frame = self.context.struct_type(&fields, false);
        let frame_size = self.target_data.get_abi_size(&frame);
        let frame_align = u64::from(self.target_data.get_abi_alignment(&frame));
        if frame_size == 0
            || frame_size > GC_MAX_OBJECT_BYTES
            || frame_align == 0
            || !frame_align.is_power_of_two()
            || frame_align > GC_MAX_OBJECT_ALIGNMENT
        {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!(
                    "{} has unsupported typed coroutine frame size/alignment {frame_size}/{frame_align}",
                    source.id()
                ),
            ));
        }

        let state_count = suspensions.len().checked_add(2).ok_or_else(|| {
            CodegenError::new(
                "ProgramTooLarge",
                "typed coroutine root-state count overflowed",
            )
        })?;
        if u64::try_from(state_count).unwrap_or(u64::MAX) > GC_MAX_ROOT_STATES {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                "typed coroutine has too many root states",
            ));
        }
        let mut state_offsets = vec![BTreeSet::<u64>::new(); state_count];
        for (ty, field) in source
            .signature()
            .params()
            .iter()
            .copied()
            .zip(parameter_fields.iter().copied())
        {
            let base = self.coroutine_field_offset(frame, field)?;
            self.collect_coroutine_root_offsets(ty, base, &mut state_offsets[0])?;
        }
        for (index, (plan_row, layout_row)) in
            plan.suspensions().iter().zip(&suspensions).enumerate()
        {
            let state = index.saturating_add(1);
            for (ty, field) in plan_row
                .live()
                .iter()
                .copied()
                .zip(layout_row.live_fields.iter().copied())
            {
                let base = self.coroutine_field_offset(frame, field)?;
                self.collect_coroutine_root_offsets(ty, base, &mut state_offsets[state])?;
            }
        }
        let completed_state = state_count - 1;
        let result_offset = self.coroutine_field_offset(frame, result_field)?;
        self.collect_coroutine_root_offsets(
            plan.output(),
            result_offset,
            &mut state_offsets[completed_state],
        )?;
        let all_offsets = state_offsets
            .iter()
            .flat_map(|offsets| offsets.iter().copied())
            .collect::<BTreeSet<_>>();
        if u64::try_from(all_offsets.len()).unwrap_or(u64::MAX) > GC_MAX_ROOT_SLOTS {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                "typed coroutine frame has too many exact managed roots",
            ));
        }
        let offsets = all_offsets.into_iter().collect::<Vec<_>>();
        let offset_indexes = offsets
            .iter()
            .copied()
            .enumerate()
            .map(|(index, offset)| (offset, index))
            .collect::<BTreeMap<_, _>>();
        let bitmap_words = offsets.len().div_ceil(64);
        let total_bitmap_words = state_count.checked_mul(bitmap_words).ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "typed coroutine root bitmap overflowed")
        })?;
        if u64::try_from(total_bitmap_words).unwrap_or(u64::MAX) > GC_MAX_ROOT_BITMAP_WORDS {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                "typed coroutine root bitmap exceeds the runtime ABI limit",
            ));
        }
        let mut bitmaps = vec![0_u64; total_bitmap_words];
        for (state, roots) in state_offsets.iter().enumerate() {
            for root in roots {
                let slot = offset_indexes[root];
                bitmaps[state * bitmap_words + slot / 64] |= 1_u64 << (slot % 64);
            }
        }
        let offsets_pointer = self.emit_i64_array(
            &format!("loom.lcir.coroutine.root_offsets.{}", source.id().raw()),
            &offsets,
        )?;
        let bitmaps_pointer = self.emit_i64_array(
            &format!("loom.lcir.coroutine.live_bitmaps.{}", source.id().raw()),
            &bitmaps,
        )?;
        let result_type = self.llvm_type(plan.output())?;
        let result_size = self.target_data.get_abi_size(&result_type);
        let result_align = u64::from(self.target_data.get_abi_alignment(&result_type));
        let descriptor_type = self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
            ],
            false,
        );
        let descriptor = self.module.add_global(
            descriptor_type,
            None,
            &format!("loom.lcir.coroutine.descriptor.{}", source.id().raw()),
        );
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.context
                    .i32_type()
                    .const_int(u64::from(TYPED_TASK_ABI_VERSION), false)
                    .into(),
                self.context.i32_type().const_zero().into(),
                callback.as_global_value().as_pointer_value().into(),
                cancel.as_global_value().as_pointer_value().into(),
                self.ptr_type.const_null().into(),
                self.context.i64_type().const_int(frame_size, false).into(),
                self.context.i64_type().const_int(frame_align, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_offset, false)
                    .into(),
                self.context.i64_type().const_int(result_size, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_align, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(offsets.len() as u64, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(state_count as u64, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(bitmap_words as u64, false)
                    .into(),
                offsets_pointer.into(),
                bitmaps_pointer.into(),
                self.context
                    .i64_type()
                    .const_int(completed_state as u64, false)
                    .into(),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(CoroutineLayout {
            frame,
            parameter_fields,
            caller_span_fields,
            suspensions,
            result_field,
            descriptor: descriptor.as_pointer_value(),
        })
    }

    fn coroutine_field_offset(
        &self,
        frame: StructType<'ctx>,
        field: u32,
    ) -> Result<u64, CodegenError> {
        self.target_data
            .offset_of_element(&frame, field)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("typed coroutine frame field {field} has no target offset"),
                )
            })
    }

    fn collect_coroutine_root_offsets(
        &self,
        root: ValueTypeId,
        base: u64,
        offsets: &mut BTreeSet<u64>,
    ) -> Result<(), CodegenError> {
        if !self.coroutine_frame_contains_managed_pointer(root)? {
            return Ok(());
        }
        for offset in self.managed_element_offsets(root)? {
            offsets.insert(base.checked_add(offset).ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "typed coroutine root offset overflowed")
            })?);
        }
        Ok(())
    }

    fn coroutine_frame_contains_managed_pointer(
        &self,
        root: ValueTypeId,
    ) -> Result<bool, CodegenError> {
        let mut pending = vec![(root, 0_usize)];
        let mut visited = 0_usize;
        while let Some((ty, depth)) = pending.pop() {
            visited = visited.saturating_add(1);
            if visited > MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE
                || depth > MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE
            {
                return Err(CodegenError::new(
                    "ProgramTooLarge",
                    "typed coroutine root projection exceeds its structural budget",
                ));
            }
            match self.repr_of(ty)? {
                Repr::ManagedPointer => return Ok(true),
                Repr::Product(product) => {
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
                        .fields();
                    pending.extend(
                        fields
                            .iter()
                            .copied()
                            .map(|field| (field, depth.saturating_add(1))),
                    );
                }
                Repr::Sum(sum) => {
                    let sum = self.artifact.representations().sum(sum).ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("missing LCIR sum representation {sum}"),
                        )
                    })?;
                    for field in sum
                        .variants()
                        .iter()
                        .flat_map(|variant| variant.fields().iter().copied())
                    {
                        pending.push((field, depth.saturating_add(1)));
                    }
                }
                Repr::Zst | Repr::Scalar(_) | Repr::ImmortalText | Repr::TaskHandle => {}
                Repr::Uninhabited => {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        format!("unsupported typed coroutine frame type {ty}"),
                    ));
                }
            }
        }
        Ok(false)
    }

    fn repr_of(&self, ty: ValueTypeId) -> Result<Repr, CodegenError> {
        let value_type = self
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        self.artifact
            .representations()
            .repr(value_type.repr())
            .copied()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR repr {ty}")))
    }

    fn emit_i64_array(
        &self,
        name: &str,
        values: &[u64],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        if values.is_empty() {
            return Ok(self.ptr_type.const_null());
        }
        let values = values
            .iter()
            .map(|value| self.context.i64_type().const_int(*value, false))
            .collect::<Vec<_>>();
        let count = u32::try_from(values.len()).map_err(|_| {
            CodegenError::new("ProgramTooLarge", "typed coroutine metadata is too large")
        })?;
        let global = self
            .module
            .add_global(self.context.i64_type().array_type(count), None, name);
        global.set_initializer(&self.context.i64_type().const_array(&values));
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
        global.set_unnamed_address(UnnamedAddress::Global);
        Ok(global.as_pointer_value())
    }

    fn compile(&self) -> Result<(), CodegenError> {
        self.emit_coroutine_cancel()?;
        for layout in self.task_join_layouts.values() {
            self.emit_task_join_callback(layout)?;
        }
        for layout in self.io_task_layouts.values() {
            self.emit_io_task_callback(layout)?;
        }
        for source in self
            .artifact
            .functions()
            .iter()
            .filter(|source| source.coroutine().is_some())
        {
            self.emit_coroutine_constructor(source)?;
        }
        for source in self.artifact.functions() {
            FunctionEmitter::new(self, source)?.compile()?;
        }
        self.emit_main()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one generated callback owns the shared fixed/runtime-width Task-join protocol and exact result publication"
    )]
    fn emit_task_join_callback(&self, layout: &TaskJoinLayout<'ctx>) -> Result<(), CodegenError> {
        let task = layout
            .callback
            .get_nth_param(0)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Task join callback has no task"))?
            .into_pointer_value();
        let executor = layout
            .callback
            .get_nth_param(1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Task join callback has no executor")
            })?
            .into_pointer_value();
        let frame = layout
            .callback
            .get_nth_param(2)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Task join callback has no frame"))?
            .into_pointer_value();
        let entry = self.context.append_basic_block(layout.callback, "entry");
        let start = self
            .context
            .append_basic_block(layout.callback, "join.start");
        let resume = self
            .context
            .append_basic_block(layout.callback, "join.resume");
        let pending = self
            .context
            .append_basic_block(layout.callback, "join.pending");
        let completed = self
            .context
            .append_basic_block(layout.callback, "join.completed");
        let faulted = self
            .context
            .append_basic_block(layout.callback, "join.faulted");
        let cancelled = self
            .context
            .append_basic_block(layout.callback, "join.cancelled");
        let invalid = self
            .context
            .append_basic_block(layout.callback, "join.invalid");

        self.builder.position_at_end(entry);
        let state_pointer = self
            .builder
            .build_struct_gep(layout.frame, frame, 0, "task.join.state.pointer")
            .map_err(builder_error)?;
        let state = self
            .builder
            .build_load(self.context.i64_type(), state_pointer, "task.join.state")
            .map_err(builder_error)?
            .into_int_value();
        self.builder
            .build_switch(
                state,
                invalid,
                &[
                    (self.context.i64_type().const_zero(), start),
                    (self.context.i64_type().const_int(1, false), resume),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(start);
        let dynamic_length = if matches!(&layout.inputs, TaskJoinInputs::Dynamic { .. }) {
            let length =
                self.load_dynamic_task_join_length(layout, frame, "task.join.dynamic.source")?;
            let empty = self
                .context
                .append_basic_block(layout.callback, "join.dynamic.empty");
            let nonempty = self
                .context
                .append_basic_block(layout.callback, "join.dynamic.nonempty");
            let is_empty = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    length,
                    self.context.i64_type().const_zero(),
                    "task.join.dynamic.is_empty",
                )
                .map_err(builder_error)?;
            self.builder
                .build_conditional_branch(is_empty, empty, nonempty)
                .map_err(builder_error)?;

            self.builder.position_at_end(empty);
            match layout.mode {
                AwaitMode::All | AwaitMode::Settled => {
                    self.builder
                        .build_unconditional_branch(completed)
                        .map_err(builder_error)?;
                }
                AwaitMode::Any | AwaitMode::Race => {
                    self.raise_fault(
                        executor,
                        FaultCode::EmptyTaskJoin,
                        layout.fault_origin.ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                "empty winner-selecting Task join has no producer origin",
                            )
                        })?,
                    )?;
                    self.emit_task_step_return(TASK_FAULTED)?;
                }
            }
            self.builder.position_at_end(nonempty);
            Some(length)
        } else {
            None
        };
        let runtime_mode = match layout.mode {
            AwaitMode::All => TASK_JOIN_ALL,
            AwaitMode::Any => TASK_JOIN_ANY,
            AwaitMode::Settled => TASK_JOIN_SETTLED,
            AwaitMode::Race => TASK_JOIN_RACE,
        };
        let prepared = call_int(
            &self.builder,
            self.task_prepare_join(),
            &[
                executor.into(),
                task.into(),
                self.context
                    .i32_type()
                    .const_int(u64::from(runtime_mode), false)
                    .into(),
            ],
            "task.join.prepare",
        )?;
        self.require_zero_status(prepared, "task.join.prepare")?;
        match &layout.inputs {
            TaskJoinInputs::Fixed { child_fields } => {
                for (index, field) in child_fields.iter().copied().enumerate() {
                    let pointer = self
                        .builder
                        .build_struct_gep(
                            layout.frame,
                            frame,
                            field,
                            &format!("task.join.child.{index}.pointer"),
                        )
                        .map_err(builder_error)?;
                    let child = self
                        .builder
                        .build_load(self.ptr_type, pointer, &format!("task.join.child.{index}"))
                        .map_err(builder_error)?
                        .into_pointer_value();
                    let name = format!("task.join.add_child.{index}");
                    let added = call_int(
                        &self.builder,
                        self.task_add_join_child(),
                        &[executor.into(), task.into(), child.into()],
                        &name,
                    )?;
                    self.require_zero_status(added, &name)?;
                }
            }
            TaskJoinInputs::Dynamic { .. } => {
                let length = dynamic_length.ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        "dynamic Task join registration has no runtime length",
                    )
                })?;
                self.emit_dynamic_task_join_loop(
                    layout,
                    frame,
                    length,
                    "task.join.dynamic.register",
                    |_, child| {
                        let added = call_int(
                            &self.builder,
                            self.task_add_join_child(),
                            &[executor.into(), task.into(), child.into()],
                            "task.join.dynamic.add_child",
                        )?;
                        self.require_zero_status(added, "task.join.dynamic.add_child")
                    },
                )?;
            }
        }
        self.builder
            .build_store(state_pointer, self.context.i64_type().const_int(1, false))
            .map_err(builder_error)?;
        self.set_task_join_root_state(task, 1, "task.join.root_state")?;
        let suspended = call_int(
            &self.builder,
            self.task_suspend_join(),
            &[executor.into(), task.into()],
            "task.join.suspend",
        )?;
        self.builder
            .build_switch(
                suspended,
                invalid,
                &[
                    (self.context.i32_type().const_zero(), resume),
                    (self.context.i32_type().const_int(1, false), pending),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(pending);
        self.emit_task_step_return(TASK_PENDING)?;

        self.builder.position_at_end(resume);
        let step = call_int(
            &self.builder,
            self.task_join_step(),
            &[task.into()],
            "task.join.join_step",
        )?;
        self.builder
            .build_switch(
                step,
                invalid,
                &[
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_COMPLETED as u64, false),
                        completed,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_FAULTED as u64, false),
                        faulted,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_CANCELLED as u64, false),
                        cancelled,
                    ),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(completed);
        let result_pointer = self
            .builder
            .build_struct_gep(
                layout.frame,
                frame,
                layout.result_field,
                "task.join.result.pointer",
            )
            .map_err(builder_error)?;
        self.emit_task_join_result(layout, task, frame, result_pointer)?;
        let published = call_int(
            &self.builder,
            self.typed_task_publish_result(),
            &[task.into()],
            "task.join.publish_result",
        )?;
        self.require_zero_status(published, "task.join.publish_result")?;
        self.emit_task_step_return(TASK_COMPLETED)?;

        self.builder.position_at_end(faulted);
        if layout.mode == AwaitMode::Any {
            let winner = call_int(
                &self.builder,
                self.task_join_winner(),
                &[task.into()],
                "task.join.any.fault.winner",
            )?;
            let no_winner = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    winner,
                    self.context.i64_type().const_all_ones(),
                    "task.join.any.no_winner",
                )
                .map_err(builder_error)?;
            let report = self
                .context
                .append_basic_block(layout.callback, "join.any.report_failed");
            let propagate = self
                .context
                .append_basic_block(layout.callback, "join.any.propagate_fault");
            self.builder
                .build_conditional_branch(no_winner, report, propagate)
                .map_err(builder_error)?;
            self.builder.position_at_end(report);
            self.raise_fault(
                executor,
                FaultCode::TaskAnyFailed,
                layout.fault_origin.ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "Task.any join has no fault origin")
                })?,
            )?;
            self.emit_task_step_return(TASK_FAULTED)?;
            self.builder.position_at_end(propagate);
        }
        self.emit_task_step_return(TASK_FAULTED)?;
        self.builder.position_at_end(cancelled);
        self.emit_task_step_return(TASK_CANCELLED)?;
        self.builder.position_at_end(invalid);
        self.emit_task_step_return(TASK_FAULTED)
    }

    fn task_join_child(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        frame: PointerValue<'ctx>,
        index: usize,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let TaskJoinInputs::Fixed { child_fields } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "fixed Task join child requested from a runtime-width source",
            ));
        };
        let field = child_fields.get(index).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("Task join child {index} is missing"),
            )
        })?;
        let pointer = self
            .builder
            .build_struct_gep(layout.frame, frame, field, &format!("{name}.pointer"))
            .map_err(builder_error)?;
        self.builder
            .build_load(self.ptr_type, pointer, name)
            .map(BasicValueEnum::into_pointer_value)
            .map_err(builder_error)
    }

    fn dynamic_task_join_source(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        frame: PointerValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let TaskJoinInputs::Dynamic { source_field, .. } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width Task join source requested from a fixed join",
            ));
        };
        let pointer = self
            .builder
            .build_struct_gep(
                layout.frame,
                frame,
                *source_field,
                &format!("{name}.pointer"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_load(self.ptr_type, pointer, name)
            .map(BasicValueEnum::into_pointer_value)
            .map_err(builder_error)
    }

    fn load_dynamic_task_join_length(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        frame: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let TaskJoinInputs::Dynamic { source_layout, .. } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width length requested from a fixed Task join",
            ));
        };
        let source = self.dynamic_task_join_source(layout, frame, name)?;
        let current = self.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List length has no block")
        })?;
        let function = current.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List length has no function")
        })?;
        let empty = self
            .context
            .append_basic_block(function, &format!("{name}.null"));
        let present = self
            .context
            .append_basic_block(function, &format!("{name}.present"));
        let merge = self
            .context
            .append_basic_block(function, &format!("{name}.length.merge"));
        let is_null = self
            .builder
            .build_is_null(source, &format!("{name}.is_null"))
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(is_null, empty, present)
            .map_err(builder_error)?;

        self.builder.position_at_end(empty);
        let zero = self.context.i64_type().const_zero();
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(present);
        let length_pointer =
            self.task_join_list_field(source_layout, source, 0, &format!("{name}.length"))?;
        let length = self
            .builder
            .build_load(
                self.context.i64_type(),
                length_pointer,
                &format!("{name}.loaded_length"),
            )
            .map_err(builder_error)?
            .into_int_value();
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(merge);
        let selected = self
            .builder
            .build_phi(self.context.i64_type(), &format!("{name}.runtime_length"))
            .map_err(builder_error)?;
        selected.add_incoming(&[(&zero, empty), (&length, present)]);
        Ok(selected.as_basic_value().into_int_value())
    }

    fn emit_dynamic_task_join_loop<F>(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        frame: PointerValue<'ctx>,
        length: IntValue<'ctx>,
        name: &str,
        mut emit: F,
    ) -> Result<BasicBlock<'ctx>, CodegenError>
    where
        F: FnMut(IntValue<'ctx>, PointerValue<'ctx>) -> Result<(), CodegenError>,
    {
        if !matches!(&layout.inputs, TaskJoinInputs::Dynamic { .. }) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width loop requested from a fixed Task join",
            ));
        }
        let current = self.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List loop has no block")
        })?;
        let function = current.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List loop has no function")
        })?;
        let header = self
            .context
            .append_basic_block(function, &format!("{name}.header"));
        let body = self
            .context
            .append_basic_block(function, &format!("{name}.body"));
        let done = self
            .context
            .append_basic_block(function, &format!("{name}.done"));
        self.builder
            .build_unconditional_branch(header)
            .map_err(builder_error)?;

        self.builder.position_at_end(header);
        let index = self
            .builder
            .build_phi(self.context.i64_type(), &format!("{name}.index"))
            .map_err(builder_error)?;
        let zero = self.context.i64_type().const_zero();
        index.add_incoming(&[(&zero, current)]);
        let ordinal = index.as_basic_value().into_int_value();
        let in_bounds = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                ordinal,
                length,
                &format!("{name}.in_bounds"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(in_bounds, body, done)
            .map_err(builder_error)?;

        self.builder.position_at_end(body);
        let child = self.dynamic_task_join_child(layout, frame, ordinal, name)?;
        emit(ordinal, child)?;
        let predecessor = self.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List loop lost its block")
        })?;
        let successor = self
            .builder
            .build_int_add(
                ordinal,
                self.context.i64_type().const_int(1, false),
                &format!("{name}.successor"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(header)
            .map_err(builder_error)?;
        index.add_incoming(&[(&successor, predecessor)]);
        self.builder.position_at_end(done);
        Ok(done)
    }

    fn dynamic_task_join_child(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        frame: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let TaskJoinInputs::Dynamic { source_layout, .. } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width child requested from a fixed Task join",
            ));
        };
        // The frame is stable scheduler storage, while its rooted List moves.
        let source =
            self.dynamic_task_join_source(layout, frame, &format!("{name}.source.reload"))?;
        let pointer = self.task_join_list_element_pointer(
            source_layout,
            source,
            index,
            &format!("{name}.child.pointer"),
        )?;
        self.builder
            .build_load(self.ptr_type, pointer, &format!("{name}.child"))
            .map(BasicValueEnum::into_pointer_value)
            .map_err(builder_error)
    }

    fn task_outcome_type(&self, output: ValueTypeId) -> Result<ValueTypeId, CodegenError> {
        let output = self
            .artifact
            .representations()
            .value_type(output)
            .map(|output| output.semantic().clone())
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Task output type is missing"))?;
        let outcome = self
            .artifact
            .program()
            .as_program()
            .canonical_types()
            .task_outcome
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "canonical TaskOutcome identity is missing")
            })?;
        self.artifact
            .representations()
            .type_id(&Type::Nominal(outcome, vec![output]))
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "canonical TaskOutcome type is missing")
            })
    }

    fn set_task_join_root_state(
        &self,
        task: PointerValue<'ctx>,
        state: u64,
        name: &str,
    ) -> Result<(), CodegenError> {
        let rooted = call_int(
            &self.builder,
            self.typed_task_set_root_state(),
            &[
                task.into(),
                self.context.i64_type().const_int(state, false).into(),
            ],
            name,
        )?;
        self.require_zero_status(rooted, name)
    }

    fn emit_task_join_result(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        task: PointerValue<'ctx>,
        frame: PointerValue<'ctx>,
        result_pointer: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        if matches!(&layout.inputs, TaskJoinInputs::Dynamic { .. }) {
            return self.emit_dynamic_task_join_result(layout, task, frame, result_pointer);
        }
        match layout.mode {
            AwaitMode::All => {
                let mut tuple = self
                    .llvm_type(layout.result_type)?
                    .into_struct_type()
                    .get_undef();
                for (index, output) in layout.output_types.iter().copied().enumerate() {
                    let child = self.task_join_child(
                        layout,
                        frame,
                        index,
                        &format!("task.join.all.child.{index}"),
                    )?;
                    let value = self.take_typed_task_result_exact(
                        child,
                        output,
                        &format!("task.join.all.result.{index}"),
                    )?;
                    let index = u32::try_from(index).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "Task.all has too many fields")
                    })?;
                    tuple = self
                        .builder
                        .build_insert_value(tuple, value, index, "task.join.all.tuple")
                        .map_err(builder_error)?
                        .into_struct_value();
                }
                self.builder
                    .build_store(result_pointer, tuple)
                    .map_err(builder_error)?;
            }
            AwaitMode::Settled => {
                self.emit_task_join_settled_result(layout, task, frame, result_pointer)?;
            }
            AwaitMode::Any | AwaitMode::Race => {
                let selected = self.emit_task_join_selected_result(layout, task, frame)?;
                self.builder
                    .build_store(result_pointer, selected)
                    .map_err(builder_error)?;
            }
        }
        Ok(())
    }

    fn emit_dynamic_task_join_result(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        task: PointerValue<'ctx>,
        frame: PointerValue<'ctx>,
        result_pointer: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let collecting = layout.collecting_root_state.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width Task join has no collecting root state",
            )
        })?;
        self.set_task_join_root_state(task, collecting, "task.join.dynamic.collecting_root_state")?;
        match layout.mode {
            AwaitMode::All | AwaitMode::Settled => {
                self.emit_dynamic_task_join_list_result(layout, frame, result_pointer)
            }
            AwaitMode::Any | AwaitMode::Race => {
                let selected = self.emit_dynamic_task_join_selected_result(layout, task, frame)?;
                self.builder
                    .build_store(result_pointer, selected)
                    .map_err(builder_error)?;
                Ok(())
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one runtime-width result loop keeps exact repeated allocation, source-order capture, and post-GC frame reloads adjacent"
    )]
    fn emit_dynamic_task_join_list_result(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        frame: PointerValue<'ctx>,
        result_pointer: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let TaskJoinInputs::Dynamic { output_type, .. } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width List result requested from a fixed Task join",
            ));
        };
        let result_layout = self.list_layout(layout.result_type)?;
        let result_element = self.list_element_type(layout.result_type)?;
        let expected_element = match layout.mode {
            AwaitMode::All => *output_type,
            AwaitMode::Settled => self.task_outcome_type(*output_type)?,
            AwaitMode::Any | AwaitMode::Race => {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "winner-selecting runtime-width Task join requested a List result",
                ));
            }
        };
        if result_element != expected_element {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width Task join result List has the wrong exact element type",
            ));
        }
        let length =
            self.load_dynamic_task_join_length(layout, frame, "task.join.dynamic.result.source")?;
        let current = self.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List result has no block")
        })?;
        let function = current.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join List result has no function")
        })?;
        let empty = self
            .context
            .append_basic_block(function, "join.dynamic.result.empty");
        let allocate = self
            .context
            .append_basic_block(function, "join.dynamic.result.allocate");
        let is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                length,
                self.context.i64_type().const_zero(),
                "task.join.dynamic.result.is_empty",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(is_empty, empty, allocate)
            .map_err(builder_error)?;

        self.builder.position_at_end(allocate);
        let descriptor = self.list_descriptor(layout.result_type, &result_layout)?;
        let status = call_int(
            &self.builder,
            self.typed_repeated_alloc(),
            &[descriptor.into(), length.into(), result_pointer.into()],
            "task.join.dynamic.result.allocate.status",
        )?;
        self.require_zero_status(status, "task.join.dynamic.result.allocate")?;
        // Allocation may move both the source List and the partial result.
        // Their only authoritative addresses are the stable task-frame cells.
        let result = self
            .builder
            .build_load(
                self.ptr_type,
                result_pointer,
                "task.join.dynamic.result.reload.after_allocate",
            )
            .map_err(builder_error)?
            .into_pointer_value();
        self.require_nonnull(result, "task.join.dynamic.result.allocate")?;
        for (field, name) in [(0_u32, "length"), (1_u32, "capacity")] {
            let pointer = self
                .builder
                .build_struct_gep(
                    result_layout.object,
                    result,
                    field,
                    &format!("task.join.dynamic.result.{name}.pointer"),
                )
                .map_err(builder_error)?;
            self.builder
                .build_store(pointer, length)
                .map_err(builder_error)?;
        }
        let done = self.emit_dynamic_task_join_loop(
            layout,
            frame,
            length,
            "task.join.dynamic.result",
            |ordinal, child| {
                let value = match layout.mode {
                    AwaitMode::All => self.take_typed_task_result_exact(
                        child,
                        *output_type,
                        "task.join.dynamic.all.result",
                    )?,
                    AwaitMode::Settled => self.take_typed_task_outcome_exact(
                        child,
                        *output_type,
                        expected_element,
                        "task.join.dynamic.settled.outcome",
                    )?,
                    AwaitMode::Any | AwaitMode::Race => unreachable!(
                        "winner-selecting joins were rejected before List result emission"
                    ),
                };
                // Outcome capture may collect; both bases live in frame cells.
                let result = self
                    .builder
                    .build_load(
                        self.ptr_type,
                        result_pointer,
                        "task.join.dynamic.result.partial.reload",
                    )
                    .map_err(builder_error)?
                    .into_pointer_value();
                let destination = self.task_join_list_element_pointer(
                    &result_layout,
                    result,
                    ordinal,
                    "task.join.dynamic.result.element",
                )?;
                self.builder
                    .build_store(destination, value)
                    .map_err(builder_error)?;
                Ok(())
            },
        )?;

        // The constructor left canonical zero in the result cell, so empty
        // all/settled reaches publication without invoking the allocator.
        self.builder.position_at_end(empty);
        self.builder
            .build_unconditional_branch(done)
            .map_err(builder_error)?;
        self.builder.position_at_end(done);
        Ok(())
    }

    fn emit_dynamic_task_join_selected_result(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        task: PointerValue<'ctx>,
        frame: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let TaskJoinInputs::Dynamic { output_type, .. } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "runtime-width winner requested from a fixed Task join",
            ));
        };
        let winner = call_int(
            &self.builder,
            self.task_join_winner(),
            &[task.into()],
            "task.join.dynamic.winner",
        )?;
        let length =
            self.load_dynamic_task_join_length(layout, frame, "task.join.dynamic.winner.source")?;
        let current = self.builder.get_insert_block().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join winner has no block")
        })?;
        let function = current.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "Task join winner has no function")
        })?;
        let valid = self
            .context
            .append_basic_block(function, "join.dynamic.winner.valid");
        let invalid = self
            .context
            .append_basic_block(function, "join.dynamic.winner.invalid");
        let in_bounds = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                winner,
                length,
                "task.join.dynamic.winner.in_bounds",
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(in_bounds, valid, invalid)
            .map_err(builder_error)?;
        self.builder.position_at_end(invalid);
        self.emit_task_step_return(TASK_FAULTED)?;

        self.builder.position_at_end(valid);
        let child =
            self.dynamic_task_join_child(layout, frame, winner, "task.join.dynamic.winner")?;
        match layout.mode {
            AwaitMode::Any => self.take_typed_task_result_exact(
                child,
                *output_type,
                "task.join.dynamic.any.result",
            ),
            AwaitMode::Race => self.take_typed_task_outcome_exact(
                child,
                *output_type,
                layout.result_type,
                "task.join.dynamic.race.outcome",
            ),
            AwaitMode::All | AwaitMode::Settled => Err(CodegenError::new(
                "LlvmAbiDefect",
                "non-selecting runtime-width Task join requested a winner result",
            )),
        }
    }

    fn task_join_list_element_pointer(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let data = self.task_join_list_field(layout, object, 2, &format!("{name}.data"))?;
        let base = self
            .builder
            .build_ptr_to_int(data, self.context.i64_type(), &format!("{name}.base"))
            .map_err(builder_error)?;
        let offset = self
            .builder
            .build_int_mul(
                index,
                self.context
                    .i64_type()
                    .const_int(layout.element_stride, false),
                &format!("{name}.offset"),
            )
            .map_err(builder_error)?;
        let address = self
            .builder
            .build_int_add(base, offset, &format!("{name}.address"))
            .map_err(builder_error)?;
        self.builder
            .build_int_to_ptr(address, self.ptr_type, name)
            .map_err(builder_error)
    }

    fn task_join_list_field(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.builder
            .build_struct_gep(layout.object, object, field, name)
            .map_err(builder_error)
    }

    fn emit_task_join_settled_result(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        task: PointerValue<'ctx>,
        frame: PointerValue<'ctx>,
        result_pointer: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let collecting = layout.collecting_root_state.ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Task.settled has no collecting root state")
        })?;
        self.builder
            .build_store(result_pointer, self.zero(layout.result_type)?)
            .map_err(builder_error)?;
        self.set_task_join_root_state(task, collecting, "task.join.settled.collecting_root_state")?;
        let tuple_type = self.llvm_type(layout.result_type)?.into_struct_type();
        for (index, output) in layout.output_types.iter().copied().enumerate() {
            let child = self.task_join_child(
                layout,
                frame,
                index,
                &format!("task.join.settled.child.{index}"),
            )?;
            let outcome = self.task_outcome_type(output)?;
            let value = self.take_typed_task_outcome_exact(
                child,
                output,
                outcome,
                &format!("task.join.settled.outcome.{index}"),
            )?;
            // `take_outcome` may move managed leaves already stored in earlier
            // fields. Reload the precisely rooted frame result after every
            // capture before inserting the next outcome.
            let tuple = self
                .builder
                .build_load(
                    tuple_type,
                    result_pointer,
                    "task.join.settled.partial.reload",
                )
                .map_err(builder_error)?
                .into_struct_value();
            let index = u32::try_from(index).map_err(|_| {
                CodegenError::new("ProgramTooLarge", "Task.settled has too many fields")
            })?;
            let tuple = self
                .builder
                .build_insert_value(tuple, value, index, "task.join.settled.partial")
                .map_err(builder_error)?;
            self.builder
                .build_store(result_pointer, tuple)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    fn emit_task_join_selected_result(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        task: PointerValue<'ctx>,
        frame: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let output = layout.output_types.first().copied().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "winner-selecting Task join has no output")
        })?;
        if layout
            .output_types
            .iter()
            .any(|candidate| *candidate != output)
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "winner-selecting Task join has heterogeneous outputs",
            ));
        }
        let winner = call_int(
            &self.builder,
            self.task_join_winner(),
            &[task.into()],
            "task.join.winner",
        )?;
        let invalid = self
            .context
            .append_basic_block(layout.callback, "join.winner.invalid");
        let merge = self
            .context
            .append_basic_block(layout.callback, "join.winner.merge");
        let TaskJoinInputs::Fixed { child_fields } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "fixed Task join winner requested from a runtime-width source",
            ));
        };
        let cases = child_fields
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let ordinal = u64::try_from(index).map_err(|_| {
                    CodegenError::new("ProgramTooLarge", "Task join has too many children")
                })?;
                Ok((
                    self.context.i64_type().const_int(ordinal, false),
                    self.context
                        .append_basic_block(layout.callback, &format!("join.winner.{ordinal}")),
                ))
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        self.builder
            .build_switch(winner, invalid, &cases)
            .map_err(builder_error)?;
        self.builder.position_at_end(invalid);
        self.emit_task_step_return(TASK_FAULTED)?;

        let mut incoming = Vec::with_capacity(cases.len());
        for (index, (_, block)) in cases.into_iter().enumerate() {
            self.builder.position_at_end(block);
            let child = self.task_join_child(
                layout,
                frame,
                index,
                &format!("task.join.winner.child.{index}"),
            )?;
            let value = match layout.mode {
                AwaitMode::Any => self.take_typed_task_result_exact(
                    child,
                    output,
                    &format!("task.join.any.result.{index}"),
                )?,
                AwaitMode::Race => self.take_typed_task_outcome_exact(
                    child,
                    output,
                    layout.result_type,
                    &format!("task.join.race.outcome.{index}"),
                )?,
                AwaitMode::All | AwaitMode::Settled => {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "non-selecting Task join requested a winner result",
                    ));
                }
            };
            let predecessor = self.builder.get_insert_block().ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "Task join winner lost its block")
            })?;
            self.builder
                .build_unconditional_branch(merge)
                .map_err(builder_error)?;
            incoming.push((value, predecessor));
        }
        self.builder.position_at_end(merge);
        let phi = self
            .builder
            .build_phi(self.llvm_type(layout.result_type)?, "task.join.selected")
            .map_err(builder_error)?;
        for (value, block) in &incoming {
            phi.add_incoming(&[(value as &dyn inkwell::values::BasicValue<'ctx>, *block)]);
        }
        Ok(phi.as_basic_value())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one generated leaf callback validates the primitive runtime outcome and writes the exact target-native Result frame without an intermediate value ABI"
    )]
    fn emit_io_task_callback(&self, layout: &IoTaskLayout<'ctx>) -> Result<(), CodegenError> {
        let task = layout
            .callback
            .get_nth_param(0)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O callback has no task"))?
            .into_pointer_value();
        let executor = layout
            .callback
            .get_nth_param(1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "typed I/O callback has no executor")
            })?
            .into_pointer_value();
        let frame = layout
            .callback
            .get_nth_param(2)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O callback has no frame"))?
            .into_pointer_value();
        let entry = self.context.append_basic_block(layout.callback, "io.entry");
        let completed = self
            .context
            .append_basic_block(layout.callback, "io.completed");
        let pending = self
            .context
            .append_basic_block(layout.callback, "io.pending");
        let faulted = self
            .context
            .append_basic_block(layout.callback, "io.faulted");
        let cancelled = self
            .context
            .append_basic_block(layout.callback, "io.cancelled");
        let success = self
            .context
            .append_basic_block(layout.callback, "io.success");
        let success_write = self
            .context
            .append_basic_block(layout.callback, "io.success.write");
        let error = self.context.append_basic_block(layout.callback, "io.error");
        let error_write = self
            .context
            .append_basic_block(layout.callback, "io.error.write");
        let publish = self
            .context
            .append_basic_block(layout.callback, "io.publish");
        let invalid = self
            .context
            .append_basic_block(layout.callback, "io.invalid");

        self.builder.position_at_end(entry);
        let scratch = self
            .builder
            .build_struct_gep(
                layout.frame,
                frame,
                layout.scratch_field,
                "io.scratch.pointer",
            )
            .map_err(builder_error)?;
        let result_pointer = self
            .builder
            .build_struct_gep(
                layout.frame,
                frame,
                layout.result_field,
                "io.result.pointer",
            )
            .map_err(builder_error)?;
        let outcome_type = self.typed_io_outcome_type();
        let outcome = self
            .builder
            .build_alloca(outcome_type, "io.outcome")
            .map_err(builder_error)?;
        self.builder
            .build_store(outcome, outcome_type.const_zero())
            .map_err(builder_error)?;
        let step = call_int(
            &self.builder,
            self.typed_io_poll(),
            &[task.into(), executor.into(), scratch.into(), outcome.into()],
            "io.poll",
        )?;
        self.builder
            .build_switch(
                step,
                invalid,
                &[
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_COMPLETED as u64, false),
                        completed,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_PENDING as u64, false),
                        pending,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_FAULTED as u64, false),
                        faulted,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_CANCELLED as u64, false),
                        cancelled,
                    ),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(completed);
        let kind_pointer = self
            .builder
            .build_struct_gep(outcome_type, outcome, 0, "io.outcome.kind.pointer")
            .map_err(builder_error)?;
        let kind = self
            .builder
            .build_load(self.context.i32_type(), kind_pointer, "io.outcome.kind")
            .map_err(builder_error)?
            .into_int_value();
        let expected = match layout.operation {
            IoTaskOperation::FileOpenRead
            | IoTaskOperation::FileCreate
            | IoTaskOperation::SocketConnect => TYPED_IO_OUTCOME_RESOURCE,
            IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => {
                TYPED_IO_OUTCOME_TEXT
            }
            IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => {
                TYPED_IO_OUTCOME_UNIT
            }
        };
        self.builder
            .build_switch(
                kind,
                invalid,
                &[
                    (
                        self.context
                            .i32_type()
                            .const_int(u64::from(expected), false),
                        success,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(u64::from(TYPED_IO_OUTCOME_ERROR), false),
                        error,
                    ),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(success);
        let scratch_value = self
            .builder
            .build_load(self.ptr_type, scratch, "io.scratch.success")
            .map_err(builder_error)?
            .into_pointer_value();
        let bits_pointer = self
            .builder
            .build_struct_gep(outcome_type, outcome, 2, "io.outcome.bits.pointer")
            .map_err(builder_error)?;
        let bits = self
            .builder
            .build_load(self.context.i64_type(), bits_pointer, "io.outcome.bits")
            .map_err(builder_error)?
            .into_int_value();
        let success_valid = match layout.operation {
            IoTaskOperation::FileOpenRead
            | IoTaskOperation::FileCreate
            | IoTaskOperation::SocketConnect => self
                .builder
                .build_int_compare(
                    IntPredicate::NE,
                    bits,
                    self.context
                        .i64_type()
                        .const_int(TYPED_IO_INVALID_RESOURCE_TOKEN, false),
                    "io.resource.valid",
                )
                .map_err(builder_error)?,
            IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => self
                .builder
                .build_is_not_null(scratch_value, "io.text.valid")
                .map_err(builder_error)?,
            IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => {
                self.context.bool_type().const_int(1, false)
            }
        };
        self.builder
            .build_conditional_branch(success_valid, success_write, invalid)
            .map_err(builder_error)?;

        self.builder.position_at_end(success_write);
        match layout.error_mode {
            IoTaskErrorMode::Result => {
                let success_offset = self.sum_payload_field_offset(layout.result_type, 0, 0)?;
                match layout.operation {
                    IoTaskOperation::FileOpenRead
                    | IoTaskOperation::FileCreate
                    | IoTaskOperation::SocketConnect => {
                        let success_physical =
                            self.llvm_type(layout.success_type)?.into_struct_type();
                        let field_offset = self
                            .target_data
                            .offset_of_element(&success_physical, 0)
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    "I/O resource field offset is missing",
                                )
                            })?;
                        let pointer = self.io_frame_byte_pointer(
                            result_pointer,
                            success_offset.checked_add(field_offset).ok_or_else(|| {
                                CodegenError::new(
                                    "ProgramTooLarge",
                                    "I/O resource offset overflowed",
                                )
                            })?,
                            "io.result.resource.pointer",
                        )?;
                        self.builder
                            .build_store(pointer, bits)
                            .map_err(builder_error)?;
                    }
                    IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => {
                        let pointer = self.io_frame_byte_pointer(
                            result_pointer,
                            success_offset,
                            "io.result.text.pointer",
                        )?;
                        self.builder
                            .build_store(pointer, scratch_value)
                            .map_err(builder_error)?;
                    }
                    IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => {}
                }
                self.store_io_result_tag(layout.result_type, result_pointer, 0)?;
            }
            IoTaskErrorMode::Fault => match layout.operation {
                IoTaskOperation::FileOpenRead
                | IoTaskOperation::FileCreate
                | IoTaskOperation::SocketConnect => {
                    let physical = self.llvm_type(layout.success_type)?.into_struct_type();
                    let resource = self
                        .builder
                        .build_insert_value(physical.get_undef(), bits, 0, "io.fault_mode.resource")
                        .map_err(builder_error)?
                        .into_struct_value();
                    self.builder
                        .build_store(result_pointer, resource)
                        .map_err(builder_error)?;
                }
                IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => {
                    self.builder
                        .build_store(result_pointer, scratch_value)
                        .map_err(builder_error)?;
                }
                IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => {}
            },
        }
        self.builder
            .build_store(scratch, self.ptr_type.const_null())
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(publish)
            .map_err(builder_error)?;

        self.builder.position_at_end(error);
        let message = self
            .builder
            .build_load(self.ptr_type, scratch, "io.error.message")
            .map_err(builder_error)?
            .into_pointer_value();
        let detail_pointer = self
            .builder
            .build_struct_gep(outcome_type, outcome, 1, "io.outcome.detail.pointer")
            .map_err(builder_error)?;
        let detail = self
            .builder
            .build_load(self.context.i32_type(), detail_pointer, "io.outcome.detail")
            .map_err(builder_error)?
            .into_int_value();
        let fault_class_pointer = self
            .builder
            .build_struct_gep(outcome_type, outcome, 2, "io.outcome.fault_class.pointer")
            .map_err(builder_error)?;
        let fault_class = self
            .builder
            .build_load(
                self.context.i64_type(),
                fault_class_pointer,
                "io.outcome.fault_class",
            )
            .map_err(builder_error)?
            .into_int_value();
        let detail_valid = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                detail,
                self.context.i32_type().const_int(10, false),
                "io.error.kind.valid",
            )
            .map_err(builder_error)?;
        let message_valid = self
            .builder
            .build_is_not_null(message, "io.error.message.valid")
            .map_err(builder_error)?;
        let fault_class_limit = if layout.operation == IoTaskOperation::SocketConnect {
            TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE + 1
        } else {
            TYPED_IO_FAULT_CLASS_OPERATION + 1
        };
        let fault_class_valid = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                fault_class,
                self.context.i64_type().const_int(fault_class_limit, false),
                "io.error.fault_class.valid",
            )
            .map_err(builder_error)?;
        let error_shape_valid = self
            .builder
            .build_and(detail_valid, message_valid, "io.error.shape.valid")
            .map_err(builder_error)?;
        let error_valid = self
            .builder
            .build_and(error_shape_valid, fault_class_valid, "io.error.valid")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(error_valid, error_write, invalid)
            .map_err(builder_error)?;

        self.builder.position_at_end(error_write);
        match layout.error_mode {
            IoTaskErrorMode::Result => {
                let error_type = layout.error_type.ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "Result-mode I/O has no IoError type")
                })?;
                let error_kind_type = layout.error_kind_type.ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "Result-mode I/O has no IoErrorKind type")
                })?;
                let error_base = self.sum_payload_field_offset(layout.result_type, 1, 0)?;
                let error_physical = self.llvm_type(error_type)?.into_struct_type();
                let kind_offset = self
                    .target_data
                    .offset_of_element(&error_physical, 0)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "IoError.kind offset is missing")
                    })?;
                let message_offset = self
                    .target_data
                    .offset_of_element(&error_physical, 1)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "IoError.message offset is missing")
                    })?;
                let kind_pointer = self.io_frame_byte_pointer(
                    result_pointer,
                    error_base.checked_add(kind_offset).ok_or_else(|| {
                        CodegenError::new("ProgramTooLarge", "IoError.kind offset overflowed")
                    })?,
                    "io.result.error.kind.pointer",
                )?;
                let kind_type = self.llvm_type(error_kind_type)?.into_int_type();
                let detail = self
                    .builder
                    .build_int_truncate(detail, kind_type, "io.error.kind")
                    .map_err(builder_error)?;
                self.builder
                    .build_store(kind_pointer, detail)
                    .map_err(builder_error)?;
                let message_pointer = self.io_frame_byte_pointer(
                    result_pointer,
                    error_base.checked_add(message_offset).ok_or_else(|| {
                        CodegenError::new("ProgramTooLarge", "IoError.message offset overflowed")
                    })?,
                    "io.result.error.message.pointer",
                )?;
                self.builder
                    .build_store(message_pointer, message)
                    .map_err(builder_error)?;
                self.store_io_result_tag(layout.result_type, result_pointer, 1)?;
                self.builder
                    .build_store(scratch, self.ptr_type.const_null())
                    .map_err(builder_error)?;
                self.builder
                    .build_unconditional_branch(publish)
                    .map_err(builder_error)?;
            }
            IoTaskErrorMode::Fault => {
                self.raise_io_fault(executor, layout.operation, fault_class, message)?;
                self.builder
                    .build_store(scratch, self.ptr_type.const_null())
                    .map_err(builder_error)?;
                self.emit_task_step_return(TASK_FAULTED)?;
            }
        }

        self.builder.position_at_end(publish);
        let published = call_int(
            &self.builder,
            self.typed_task_publish_result(),
            &[task.into()],
            "io.result.publish",
        )?;
        self.require_zero_status(published, "io.result.publish")?;
        self.emit_task_step_return(TASK_COMPLETED)?;

        self.builder.position_at_end(pending);
        self.emit_task_step_return(TASK_PENDING)?;
        self.builder.position_at_end(faulted);
        self.emit_task_step_return(TASK_FAULTED)?;
        self.builder.position_at_end(cancelled);
        self.emit_task_step_return(TASK_CANCELLED)?;
        self.builder.position_at_end(invalid);
        self.emit_task_step_return(TASK_FAULTED)
    }

    fn io_fault_literal(
        &self,
        code: &'static str,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let data = self
            .builder
            .build_global_string_ptr(code, &self.unique("io.fault.code"))
            .map_err(builder_error)?
            .as_pointer_value();
        Ok((
            data,
            self.context.i64_type().const_int(code.len() as u64, false),
        ))
    }

    fn socket_connect_fault_literal(
        &self,
        fault_class: IntValue<'ctx>,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let choices = [
            self.io_fault_literal("SocketConnectFault")?,
            self.io_fault_literal("SocketResolveFault")?,
            self.io_fault_literal("InvalidPort")?,
        ];
        let is_resolution = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                fault_class,
                self.context
                    .i64_type()
                    .const_int(TYPED_IO_FAULT_CLASS_SOCKET_RESOLVE, false),
                "io.fault.is_resolve",
            )
            .map_err(builder_error)?;
        let fallback_data = self
            .builder
            .build_select(
                is_resolution,
                choices[1].0,
                choices[0].0,
                "io.fault.resolve.data",
            )
            .map_err(builder_error)?
            .into_pointer_value();
        let fallback_length = self
            .builder
            .build_select(
                is_resolution,
                choices[1].1,
                choices[0].1,
                "io.fault.resolve.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let is_invalid_port = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                fault_class,
                self.context
                    .i64_type()
                    .const_int(TYPED_IO_FAULT_CLASS_INVALID_PORT, false),
                "io.fault.is_invalid_port",
            )
            .map_err(builder_error)?;
        let code_data = self
            .builder
            .build_select(
                is_invalid_port,
                choices[2].0,
                fallback_data,
                "io.fault.code.data",
            )
            .map_err(builder_error)?
            .into_pointer_value();
        let code_length = self
            .builder
            .build_select(
                is_invalid_port,
                choices[2].1,
                fallback_length,
                "io.fault.code.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        Ok((code_data, code_length))
    }

    fn raise_io_fault(
        &self,
        executor: PointerValue<'ctx>,
        operation: IoTaskOperation,
        fault_class: IntValue<'ctx>,
        message: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let (code_data, code_length) = match operation {
            IoTaskOperation::FileOpenRead => self.io_fault_literal("FileOpenFault")?,
            IoTaskOperation::FileCreate => self.io_fault_literal("FileCreateFault")?,
            IoTaskOperation::FileReadText => self.io_fault_literal("FileReadFault")?,
            IoTaskOperation::FileWriteText => self.io_fault_literal("FileWriteFault")?,
            IoTaskOperation::SocketConnect => self.socket_connect_fault_literal(fault_class)?,
            IoTaskOperation::SocketReadText => self.io_fault_literal("SocketReadFault")?,
            IoTaskOperation::SocketWriteText => self.io_fault_literal("SocketWriteFault")?,
        };
        let (message_data, message_length) = self.text_parts(message, "io.fault.message")?;
        let status = call_int(
            &self.builder,
            self.context_raise_fault()?,
            &[
                executor.into(),
                code_data.into(),
                code_length.into(),
                message_data.into(),
                message_length.into(),
                message_data.into(),
                message_length.into(),
                self.ptr_type.const_null().into(),
                self.context.i64_type().const_zero().into(),
            ],
            "io.fault.raise",
        )?;
        self.require_zero_status(status, "io.fault.raise")
    }

    #[expect(
        unsafe_code,
        reason = "target-data-proven typed I/O field offsets stay within the descriptor-validated direct result frame"
    )]
    fn io_frame_byte_pointer(
        &self,
        base: PointerValue<'ctx>,
        offset: u64,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        // SAFETY: the typed I/O descriptor was derived from the same target
        // layouts, and every supplied offset names a complete field inside its
        // validated zeroed result range.
        unsafe {
            self.builder
                .build_gep(
                    self.context.i8_type(),
                    base,
                    &[self.context.i64_type().const_int(offset, false)],
                    name,
                )
                .map_err(builder_error)
        }
    }

    fn store_io_result_tag(
        &self,
        result_type: ValueTypeId,
        result: PointerValue<'ctx>,
        variant: u32,
    ) -> Result<(), CodegenError> {
        let layout = self.sum_layout(result_type)?;
        let physical = layout.physical.into_struct_type();
        let tag_type = self
            .sum_tag_type(layout.tag)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "I/O Result tag is missing"))?;
        let offset = self
            .target_data
            .offset_of_element(&physical, 0)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "I/O Result tag offset is missing")
            })?;
        let pointer = self.io_frame_byte_pointer(result, offset, "io.result.tag.pointer")?;
        self.builder
            .build_store(pointer, tag_type.const_int(u64::from(variant), false))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_task_step_return(&self, step: i32) -> Result<(), CodegenError> {
        self.builder
            .build_return(Some(
                &self
                    .context
                    .i32_type()
                    .const_int(u64::from(step.cast_unsigned()), false),
            ))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_coroutine_cancel(&self) -> Result<(), CodegenError> {
        let Some(cancel) = self.coroutine_cancel else {
            return Ok(());
        };
        let entry = self.context.append_basic_block(cancel, "entry");
        self.builder.position_at_end(entry);
        self.builder
            .build_return(Some(
                &self
                    .context
                    .i32_type()
                    .const_int(TASK_CANCELLED as u64, false),
            ))
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_coroutine_constructor(&self, source: &Function) -> Result<(), CodegenError> {
        let function = self.function(source.id())?;
        let layout = self.coroutine_layout(source.id())?;
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);
        let parameter_count = u32::try_from(source.signature().params().len())
            .map_err(|_| CodegenError::new("ProgramTooLarge", "too many coroutine parameters"))?;
        let executor_index = parameter_count
            .checked_add(3 * u32::from(layout.caller_span_fields.is_some()))
            .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many coroutine parameters"))?;
        let executor = function
            .get_nth_param(executor_index)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "coroutine constructor has no executor")
            })?
            .into_pointer_value();
        let task = call_pointer(
            &self.builder,
            self.typed_task_create(),
            &[executor.into(), layout.descriptor.into()],
            "task.create",
        )?;
        self.require_nonnull(task, "task.create")?;
        let frame = call_pointer(
            &self.builder,
            self.typed_task_frame(),
            &[task.into()],
            "task.frame",
        )?;
        self.require_nonnull(frame, "task.frame")?;
        let state = self
            .builder
            .build_struct_gep(layout.frame, frame, 0, "task.frame.state")
            .map_err(builder_error)?;
        self.builder
            .build_store(state, self.context.i64_type().const_zero())
            .map_err(builder_error)?;
        if let Some(fields) = layout.caller_span_fields {
            for (offset, field) in fields.into_iter().enumerate() {
                let offset = u32::try_from(offset)
                    .map_err(|_| CodegenError::new("ProgramTooLarge", "invalid caller span"))?;
                let index = parameter_count.checked_add(offset).ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "too many coroutine parameters")
                })?;
                let value = function.get_nth_param(index).ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("coroutine constructor is missing caller-span field {offset}"),
                    )
                })?;
                let pointer = self
                    .builder
                    .build_struct_gep(layout.frame, frame, field, "task.frame.caller_span")
                    .map_err(builder_error)?;
                self.builder
                    .build_store(pointer, value)
                    .map_err(builder_error)?;
            }
        }
        for (index, field) in layout.parameter_fields.iter().copied().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many task parameters"))?;
            let parameter = function.get_nth_param(index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("coroutine constructor is missing parameter {index}"),
                )
            })?;
            let pointer = self
                .builder
                .build_struct_gep(layout.frame, frame, field, "task.frame.parameter")
                .map_err(builder_error)?;
            self.builder
                .build_store(pointer, parameter)
                .map_err(builder_error)?;
        }
        let initialized = call_int(
            &self.builder,
            self.typed_task_initialize(),
            &[task.into(), self.context.i64_type().const_zero().into()],
            "task.initialize",
        )?;
        self.require_zero_status(initialized, "task.initialize")?;
        let published = call_int(
            &self.builder,
            self.typed_task_publish(),
            &[executor.into(), task.into()],
            "task.publish",
        )?;
        self.require_zero_status(published, "task.publish")?;
        self.builder
            .build_return(Some(&task))
            .map_err(builder_error)?;
        Ok(())
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
            Some(Repr::ImmortalText | Repr::ManagedPointer | Repr::TaskHandle) => {
                Ok(self.ptr_type.into())
            }
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

    fn product_repr(&self, ty: ValueTypeId) -> Result<&loom_codegen_ir::ProductRepr, CodegenError> {
        let value_type = self
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        let Repr::Product(product) = self
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
                format!("LCIR type {ty} is not a product"),
            ));
        };
        self.artifact
            .representations()
            .product(product)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("missing LCIR product representation {product}"),
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

    fn charge_sum_layout_graph_work(&self, amount: u64) -> Result<(), CodegenError> {
        let work = self
            .sum_layout_graph_work
            .get()
            .checked_add(amount)
            .ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "sum layout graph work overflowed")
            })?;
        if work > SUM_LAYOUT_MAX_GRAPH_WORK {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!(
                    "sum layout graph exceeds the shared {SUM_LAYOUT_MAX_GRAPH_WORK}-step work limit"
                ),
            ));
        }
        self.sum_layout_graph_work.set(work);
        Ok(())
    }

    fn charge_sum_carrier_emission_work(&self, amount: u64) -> Result<(), CodegenError> {
        let work = checked_sum_carrier_emission_work(self.sum_carrier_emission_work.get(), amount)?;
        self.sum_carrier_emission_work.set(work);
        Ok(())
    }

    fn sum_layout(&self, ty: ValueTypeId) -> Result<SumLayout<'ctx>, CodegenError> {
        if let Some(layout) = self.sum_layout_cache.borrow().get(&ty.raw()).cloned() {
            return Ok(layout);
        }
        self.charge_sum_layout_graph_work(1)?;
        if !self.sum_layout_in_progress.borrow_mut().insert(ty.raw()) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("cyclic physical sum layout reached LCIR type {ty}"),
            ));
        }
        let result = self.compute_sum_layout(ty);
        self.sum_layout_in_progress.borrow_mut().remove(&ty.raw());
        if let Ok(layout) = &result {
            self.sum_layout_cache
                .borrow_mut()
                .insert(ty.raw(), layout.clone());
        }
        result
    }

    #[expect(
        clippy::too_many_lines,
        reason = "sum layout selection and its target-data size/alignment proof are intentionally one atomic computation"
    )]
    fn compute_sum_layout(&self, ty: ValueTypeId) -> Result<SumLayout<'ctx>, CodegenError> {
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
                payload_byte_offsets: vec![0; payloads.len()],
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
                payload_byte_offsets: vec![0; payloads.len()],
                payloads,
                carrier: None,
                physical: tag_type.into(),
            });
        }

        let pointer_size = self.target_data.get_abi_size(&self.ptr_type);
        let pointer_alignment = u64::from(self.target_data.get_abi_alignment(&self.ptr_type));
        let shapes = sum
            .variants()
            .iter()
            .zip(&payloads)
            .enumerate()
            .map(|(variant_index, (variant, payload))| {
                let mut pointer_offsets = BTreeSet::new();
                for (field_index, field) in variant.fields().iter().copied().enumerate() {
                    let field_index = u32::try_from(field_index).map_err(|_| {
                        CodegenError::new(
                            "ProgramTooLarge",
                            format!("sum type {ty} variant {variant_index} has too many fields"),
                        )
                    })?;
                    let field_base = self
                        .target_data
                        .offset_of_element(payload, field_index)
                        .ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                format!(
                                    "sum type {ty} variant {variant_index} field {field_index} has no target offset"
                                ),
                            )
                        })?;
                    for offset in self.managed_element_offsets(field)? {
                        pointer_offsets.insert(field_base.checked_add(offset).ok_or_else(|| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "sum payload managed pointer offset overflowed",
                            )
                        })?);
                    }
                }
                Ok(SumPayloadShape {
                    size: self.target_data.get_abi_size(payload),
                    alignment: self.target_data.get_abi_alignment(payload),
                    pointer_offsets: pointer_offsets.into_iter().collect(),
                })
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        let placement_before = self.sum_carrier_placement_work.get();
        let placement_remaining = SUM_CARRIER_MAX_PLACEMENT_WORK
            .checked_sub(placement_before)
            .ok_or_else(|| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "sum carrier placement budget underflowed",
                )
            })?;
        let plan = plan_sum_carrier(
            &shapes,
            pointer_size,
            pointer_alignment,
            placement_remaining,
        )?;
        self.sum_carrier_placement_work.set(
            placement_before
                .checked_add(plan.placement_work)
                .ok_or_else(|| {
                    CodegenError::new("ProgramTooLarge", "sum carrier placement work overflowed")
                })?,
        );
        let anchor = payloads.get(plan.anchor_variant).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR sum type {ty} carrier anchor disappeared"),
            )
        })?;
        let carrier_bytes = u32::try_from(plan.byte_len).map_err(|_| {
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
        let expected_size =
            checked_align_up(plan.byte_len, u64::from(plan.alignment)).ok_or_else(|| {
                CodegenError::new(
                    "ProgramTooLarge",
                    format!("LCIR sum type {ty} aligned carrier size overflowed"),
                )
            })?;
        let actual_size = self.target_data.get_abi_size(&carrier);
        let actual_alignment = self.target_data.get_abi_alignment(&carrier);
        if actual_size != expected_size || actual_alignment != plan.alignment {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "LCIR sum type {ty} carrier has ABI size/alignment {actual_size}/{actual_alignment}, expected {expected_size}/{}",
                    plan.alignment
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
            payload_byte_offsets: plan.payload_byte_offsets,
            carrier: Some(carrier),
            physical,
        })
    }

    fn list_element_type(&self, ty: ValueTypeId) -> Result<ValueTypeId, CodegenError> {
        let value_type = self
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        let Type::List(element) = value_type.semantic() else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR type {ty} is not a List"),
            ));
        };
        self.artifact
            .representations()
            .type_id(element)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("List type {ty} has no canonical element representation"),
                )
            })
    }

    fn text_map_value_type(&self, ty: ValueTypeId) -> Result<ValueTypeId, CodegenError> {
        let value_type = self
            .artifact
            .representations()
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        if value_type.kind() != ValueTypeKind::ManagedTextMap {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("LCIR type {ty} is not a TextMap"),
            ));
        }
        let Type::Nominal(_, arguments) = value_type.semantic() else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("TextMap type {ty} is not nominal"),
            ));
        };
        let [value] = arguments.as_slice() else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("TextMap type {ty} does not have one value argument"),
            ));
        };
        self.artifact
            .representations()
            .type_id(value)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("TextMap type {ty} has no canonical value representation"),
                )
            })
    }

    fn managed_element_offsets(&self, root: ValueTypeId) -> Result<Vec<u64>, CodegenError> {
        if let Some(offsets) = self.managed_offset_cache.borrow().get(&root.raw()).cloned() {
            return Ok(offsets);
        }
        self.charge_sum_layout_graph_work(1)?;
        if !self
            .managed_offset_in_progress
            .borrow_mut()
            .insert(root.raw())
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("cyclic managed-offset layout reached LCIR type {root}"),
            ));
        }
        let result = self.compute_managed_element_offsets(root);
        self.managed_offset_in_progress
            .borrow_mut()
            .remove(&root.raw());
        if let Ok(offsets) = &result {
            self.managed_offset_cache
                .borrow_mut()
                .insert(root.raw(), offsets.clone());
        }
        result
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one bounded target-layout walk must keep products, nested sums, and pointer leaves under the same offset and alignment checks"
    )]
    fn compute_managed_element_offsets(&self, root: ValueTypeId) -> Result<Vec<u64>, CodegenError> {
        let mut offsets = BTreeSet::new();
        let mut pending = vec![(root, 0_u64, 0_usize)];
        let mut visited_nodes = 0_usize;
        while let Some((ty, base, depth)) = pending.pop() {
            self.charge_sum_layout_graph_work(1)?;
            visited_nodes = visited_nodes.checked_add(1).ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "List element pointer walk overflowed")
            })?;
            if depth > MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE
                || visited_nodes > MANAGED_ROOT_MAX_CANDIDATE_SLOTS_PER_VALUE
            {
                return Err(CodegenError::new(
                    "ProgramTooLarge",
                    "List element managed-pointer graph exceeds its structural budget",
                ));
            }
            let value_type = self
                .artifact
                .representations()
                .value_type(ty)
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}"))
                })?;
            let repr = self
                .artifact
                .representations()
                .repr(value_type.repr())
                .copied()
                .ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("missing representation for LCIR type {ty}"),
                    )
                })?;
            match repr {
                Repr::ManagedPointer => {
                    offsets.insert(base);
                    if offsets.len() > usize::try_from(GC_MAX_OBJECT_POINTERS).unwrap_or(usize::MAX)
                    {
                        return Err(CodegenError::new(
                            "ProgramTooLarge",
                            "List element has too many exact managed-pointer offsets",
                        ));
                    }
                }
                Repr::Product(product) => {
                    let fields = self
                        .artifact
                        .representations()
                        .product(product)
                        .ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                format!("missing product representation {product}"),
                            )
                        })?
                        .fields();
                    let physical = self.llvm_type(ty)?.into_struct_type();
                    for (index, field) in fields.iter().copied().enumerate().rev() {
                        let index = u32::try_from(index).map_err(|_| {
                            CodegenError::new("ProgramTooLarge", "too many List product fields")
                        })?;
                        let field_offset = self
                            .target_data
                            .offset_of_element(&physical, index)
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    format!("missing target offset for List product field {index}"),
                                )
                            })?;
                        pending.push((
                            field,
                            base.checked_add(field_offset).ok_or_else(|| {
                                CodegenError::new(
                                    "ProgramTooLarge",
                                    "List product pointer offset overflowed",
                                )
                            })?,
                            depth.saturating_add(1),
                        ));
                    }
                }
                Repr::Sum(_) => {
                    let sum = self.sum_repr(ty)?;
                    let layout = self.sum_layout(ty)?;
                    if layout.tag == SumTagRepr::Tagless {
                        let payload = layout.payloads.first().copied().ok_or_else(|| {
                            CodegenError::new("LlvmAbiDefect", "tagless List sum has no payload")
                        })?;
                        let variant = sum.variants().first().ok_or_else(|| {
                            CodegenError::new("LlvmAbiDefect", "tagless List sum has no variant")
                        })?;
                        for (index, field) in variant.fields().iter().copied().enumerate().rev() {
                            let index = u32::try_from(index).map_err(|_| {
                                CodegenError::new("ProgramTooLarge", "too many List sum fields")
                            })?;
                            let field_offset = self
                                .target_data
                                .offset_of_element(&payload, index)
                                .ok_or_else(|| {
                                    CodegenError::new(
                                        "LlvmAbiDefect",
                                        "missing target offset for tagless List sum field",
                                    )
                                })?;
                            pending.push((
                                field,
                                base.checked_add(field_offset).ok_or_else(|| {
                                    CodegenError::new(
                                        "ProgramTooLarge",
                                        "tagless List sum pointer offset overflowed",
                                    )
                                })?,
                                depth.saturating_add(1),
                            ));
                        }
                    } else if let Some(carrier) = layout.carrier {
                        let physical = layout.physical.into_struct_type();
                        let carrier_offset = self
                            .target_data
                            .offset_of_element(&physical, 1)
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    "missing tagged List sum carrier offset",
                                )
                            })?;
                        let bytes_offset = self
                            .target_data
                            .offset_of_element(&carrier, 1)
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    "missing tagged List sum byte-carrier offset",
                                )
                            })?;
                        let payload_base = base
                            .checked_add(carrier_offset)
                            .and_then(|offset| offset.checked_add(bytes_offset))
                            .ok_or_else(|| {
                                CodegenError::new(
                                    "ProgramTooLarge",
                                    "tagged List sum carrier offset overflowed",
                                )
                            })?;
                        for (variant_index, (variant, payload)) in sum
                            .variants()
                            .iter()
                            .zip(&layout.payloads)
                            .enumerate()
                            .rev()
                        {
                            let variant_offset = layout
                                .payload_byte_offsets
                                .get(variant_index)
                                .copied()
                                .ok_or_else(|| {
                                    CodegenError::new(
                                        "LlvmAbiDefect",
                                        "tagged sum payload offset disappeared",
                                    )
                                })?;
                            for (index, field) in variant.fields().iter().copied().enumerate().rev()
                            {
                                let index = u32::try_from(index).map_err(|_| {
                                    CodegenError::new(
                                        "ProgramTooLarge",
                                        "too many List sum payload fields",
                                    )
                                })?;
                                let field_offset = self
                                    .target_data
                                    .offset_of_element(payload, index)
                                    .ok_or_else(|| {
                                        CodegenError::new(
                                            "LlvmAbiDefect",
                                            "missing target offset for tagged List sum field",
                                        )
                                    })?;
                                pending.push((
                                    field,
                                    payload_base
                                        .checked_add(variant_offset)
                                        .and_then(|offset| offset.checked_add(field_offset))
                                        .ok_or_else(|| {
                                            CodegenError::new(
                                                "ProgramTooLarge",
                                                "tagged repeated sum pointer offset overflowed",
                                            )
                                        })?,
                                    depth.saturating_add(1),
                                ));
                            }
                        }
                    }
                }
                Repr::Uninhabited | Repr::ImmortalText => {
                    return Err(CodegenError::new(
                        "LcirListDescriptorUnsupported",
                        format!("List element type {root} contains an unsupported {repr:?} leaf"),
                    ));
                }
                // Task handles are stable scheduler pointers, not moving-GC
                // object references, including when carried by a product or
                // sum rooted in a coroutine frame.
                Repr::TaskHandle | Repr::Zst | Repr::Scalar(_) => {}
            }
        }
        Ok(offsets.into_iter().collect())
    }

    fn list_layout(&self, ty: ValueTypeId) -> Result<ListLayout<'ctx>, CodegenError> {
        let element_ty = self.list_element_type(ty)?;
        let element = self.llvm_type(element_ty)?;
        let element_size = self.target_data.get_abi_size(&element);
        let storage = if element_size == 0 {
            self.context.i8_type().into()
        } else {
            element
        };
        let element_stride = self.target_data.get_abi_size(&storage);
        let element_align = self.target_data.get_abi_alignment(&storage);
        let object = self.context.struct_type(
            &[
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                storage.array_type(0).into(),
            ],
            false,
        );
        let fixed_size = self
            .target_data
            .offset_of_element(&object, 2)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "List data offset is missing"))?;
        let object_size = self.target_data.get_abi_size(&object);
        let object_align = u64::from(self.target_data.get_abi_alignment(&object));
        if fixed_size == 0
            || fixed_size != object_size
            || element_stride == 0
            || object_align == 0
            || !object_align.is_power_of_two()
            || object_align > GC_MAX_OBJECT_ALIGNMENT
            || u64::from(element_align) > object_align
        {
            return Err(CodegenError::new(
                "LcirListDescriptorUnsupported",
                format!(
                    "List type {ty} has unsupported fixed-size/object-align/stride/element-align {fixed_size}/{object_align}/{element_stride}/{element_align}"
                ),
            ));
        }
        // Task handles are stable scheduler pointers, not moving-GC object
        // references. The only repeated carrier allowed to contain them is
        // the exact `List[Task[T]]` shape validated by LCIR; its descriptor
        // therefore has no managed element offsets. The general walker also
        // skips TaskHandle leaves in by-value coroutine products and sums.
        let pointer_offsets = if self.repr_of(element_ty)? == Repr::TaskHandle {
            let semantic = self
                .artifact
                .representations()
                .value_type(element_ty)
                .map(loom_codegen_ir::ValueType::semantic)
                .ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("List type {ty} has no semantic element type"),
                    )
                })?;
            if !matches!(semantic, Type::Task(_)) || element != BasicTypeEnum::from(self.ptr_type) {
                return Err(CodegenError::new(
                    "LcirListDescriptorUnsupported",
                    format!("List type {ty} does not use the exact TaskHandle pointer layout"),
                ));
            }
            Vec::new()
        } else {
            self.managed_element_offsets(element_ty)?
        };
        let pointer_size = self.target_data.get_abi_size(&self.ptr_type);
        let pointer_align = u64::from(self.target_data.get_abi_alignment(&self.ptr_type));
        for offset in &pointer_offsets {
            if !offset.is_multiple_of(pointer_align)
                || offset
                    .checked_add(pointer_size)
                    .is_none_or(|end| end > element_stride)
            {
                return Err(CodegenError::new(
                    "LcirListDescriptorUnsupported",
                    format!(
                        "List type {ty} has out-of-stride or unaligned managed offset {offset} for stride {element_stride}"
                    ),
                ));
            }
        }
        if !pointer_offsets.is_empty()
            && (!fixed_size.is_multiple_of(pointer_align)
                || !element_stride.is_multiple_of(pointer_align)
                || object_align < pointer_align)
        {
            return Err(CodegenError::new(
                "LcirListDescriptorUnsupported",
                format!("List type {ty} cannot satisfy repeated pointer-cell alignment"),
            ));
        }
        Ok(ListLayout {
            object,
            element,
            fixed_size,
            object_align,
            element_stride,
            element_align,
            pointer_offsets,
        })
    }

    fn text_map_layout(&self, ty: ValueTypeId) -> Result<TextMapLayout<'ctx>, CodegenError> {
        let value_ty = self.text_map_value_type(ty)?;
        let value = self.llvm_type(value_ty)?;
        let entry = self
            .context
            .struct_type(&[self.ptr_type.into(), value], false);
        let entry_stride = self.target_data.get_abi_size(&entry);
        let entry_align = self.target_data.get_abi_alignment(&entry);
        let object = self.context.struct_type(
            &[self.context.i64_type().into(), entry.array_type(0).into()],
            false,
        );
        let fixed_size = self
            .target_data
            .offset_of_element(&object, 1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "TextMap data offset is missing"))?;
        let object_size = self.target_data.get_abi_size(&object);
        let object_align = u64::from(self.target_data.get_abi_alignment(&object));
        if fixed_size == 0
            || fixed_size != object_size
            || entry_stride == 0
            || object_align == 0
            || !object_align.is_power_of_two()
            || object_align > GC_MAX_OBJECT_ALIGNMENT
            || u64::from(entry_align) > object_align
        {
            return Err(CodegenError::new(
                "LcirTextMapDescriptorUnsupported",
                format!(
                    "TextMap type {ty} has unsupported fixed-size/object-align/stride/entry-align {fixed_size}/{object_align}/{entry_stride}/{entry_align}"
                ),
            ));
        }
        let value_offset = self
            .target_data
            .offset_of_element(&entry, 1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "TextMap value offset is missing"))?;
        let mut pointer_offsets = vec![0_u64];
        for offset in self.managed_element_offsets(value_ty)? {
            pointer_offsets.push(value_offset.checked_add(offset).ok_or_else(|| {
                CodegenError::new("ProgramTooLarge", "TextMap value pointer offset overflowed")
            })?);
        }
        pointer_offsets.sort_unstable();
        pointer_offsets.dedup();
        let pointer_size = self.target_data.get_abi_size(&self.ptr_type);
        let pointer_align = u64::from(self.target_data.get_abi_alignment(&self.ptr_type));
        for offset in &pointer_offsets {
            if !offset.is_multiple_of(pointer_align)
                || offset
                    .checked_add(pointer_size)
                    .is_none_or(|end| end > entry_stride)
            {
                return Err(CodegenError::new(
                    "LcirTextMapDescriptorUnsupported",
                    format!(
                        "TextMap type {ty} has out-of-stride or unaligned managed offset {offset} for stride {entry_stride}"
                    ),
                ));
            }
        }
        if !fixed_size.is_multiple_of(pointer_align)
            || !entry_stride.is_multiple_of(pointer_align)
            || object_align < pointer_align
        {
            return Err(CodegenError::new(
                "LcirTextMapDescriptorUnsupported",
                format!("TextMap type {ty} cannot satisfy repeated pointer-cell alignment"),
            ));
        }
        Ok(TextMapLayout {
            object,
            entry,
            value,
            fixed_size,
            object_align,
            entry_stride,
            entry_align,
            pointer_offsets,
        })
    }

    fn list_descriptor(
        &self,
        ty: ValueTypeId,
        layout: &ListLayout<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.repeated_descriptor(
            "list",
            ty,
            layout.fixed_size,
            layout.object_align,
            layout.element_stride,
            &layout.pointer_offsets,
        )
    }

    fn text_map_descriptor(
        &self,
        ty: ValueTypeId,
        layout: &TextMapLayout<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.repeated_descriptor(
            "text_map",
            ty,
            layout.fixed_size,
            layout.object_align,
            layout.entry_stride,
            &layout.pointer_offsets,
        )
    }

    fn repeated_descriptor(
        &self,
        namespace: &str,
        ty: ValueTypeId,
        fixed_size: u64,
        object_align: u64,
        element_stride: u64,
        pointer_offsets: &[u64],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let descriptor_name = format!("loom.lcir.{namespace}.descriptor.{}", ty.raw());
        if let Some(existing) = self.module.get_global(&descriptor_name) {
            return Ok(existing.as_pointer_value());
        }
        let offsets_pointer = if pointer_offsets.is_empty() {
            self.ptr_type.const_null()
        } else {
            let values = pointer_offsets
                .iter()
                .map(|offset| self.context.i64_type().const_int(*offset, false))
                .collect::<Vec<_>>();
            let count = u32::try_from(values.len()).map_err(|_| {
                CodegenError::new(
                    "ProgramTooLarge",
                    format!("too many {namespace} repeated-element pointer offsets"),
                )
            })?;
            let array_type = self.context.i64_type().array_type(count);
            let offsets = self.module.add_global(
                array_type,
                None,
                &format!("loom.lcir.{namespace}.pointer_offsets.{}", ty.raw()),
            );
            offsets.set_initializer(&self.context.i64_type().const_array(&values));
            offsets.set_constant(true);
            offsets.set_linkage(Linkage::Private);
            offsets.set_unnamed_address(UnnamedAddress::Global);
            offsets.as_pointer_value()
        };
        let descriptor_type = self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
            ],
            false,
        );
        let descriptor = self
            .module
            .add_global(descriptor_type, None, &descriptor_name);
        let pointer_count = u64::try_from(pointer_offsets.len()).map_err(|_| {
            CodegenError::new(
                "ProgramTooLarge",
                format!("too many {namespace} repeated-element pointer offsets"),
            )
        })?;
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.context
                    .i32_type()
                    .const_int(u64::from(TYPED_GC_REPEATED_ABI_VERSION), false)
                    .into(),
                self.context.i32_type().const_zero().into(),
                self.context.i64_type().const_int(fixed_size, false).into(),
                self.context
                    .i64_type()
                    .const_int(object_align, false)
                    .into(),
                self.context.i64_type().const_zero().into(),
                self.ptr_type.const_null().into(),
                self.context
                    .i64_type()
                    .const_int(element_stride, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(pointer_count, false)
                    .into(),
                offsets_pointer.into(),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(descriptor.as_pointer_value())
    }

    fn sum_payload_field_offset(
        &self,
        ty: ValueTypeId,
        variant: usize,
        field: u32,
    ) -> Result<u64, CodegenError> {
        let layout = self.sum_layout(ty)?;
        if layout.tag == SumTagRepr::Tagless {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("typed JSON sum type {ty} is unexpectedly tagless"),
            ));
        }
        let physical = layout.physical.into_struct_type();
        let carrier = layout.carrier.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("typed JSON sum type {ty} has no payload carrier"),
            )
        })?;
        let payload = layout.payloads.get(variant).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("typed JSON sum type {ty} has no variant {variant}"),
            )
        })?;
        let carrier_base = self
            .target_data
            .offset_of_element(&physical, 1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "JSON carrier offset is missing"))?;
        let carrier_bytes = self
            .target_data
            .offset_of_element(&carrier, 1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "JSON carrier byte offset is missing")
            })?;
        let payload_base = layout.payload_byte_offset(variant)?;
        let field_base = self
            .target_data
            .offset_of_element(&payload, field)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("typed JSON variant {variant} field {field} offset is missing"),
                )
            })?;
        carrier_base
            .checked_add(carrier_bytes)
            .and_then(|offset| offset.checked_add(payload_base))
            .and_then(|offset| offset.checked_add(field_base))
            .ok_or_else(|| CodegenError::new("ProgramTooLarge", "typed JSON offset overflowed"))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one compiler-private descriptor must prove the complete recursive JSON/List/TextMap target layout"
    )]
    fn typed_json_layout_descriptor(
        &self,
        json_ty: ValueTypeId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let descriptor_name = format!("loom.lcir.typed_json.layout.{}", json_ty.raw());
        if let Some(existing) = self.module.get_global(&descriptor_name) {
            return Ok(existing.as_pointer_value());
        }

        let representations = self.artifact.representations();
        let json_value = representations.value_type(json_ty).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("missing JSON value type {json_ty}"),
            )
        })?;
        let json_semantic = json_value.semantic().clone();
        let sum = self.sum_repr(json_ty)?;
        let expected_fields = [0_usize, 1, 1, 1, 1, 1];
        if sum.variants().len() != expected_fields.len()
            || sum
                .variants()
                .iter()
                .zip(expected_fields)
                .any(|(variant, fields)| variant.fields().len() != fields)
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed JSON requires the canonical six-variant payload shape",
            ));
        }
        let field_type = |variant: usize| {
            sum.variants()
                .get(variant)
                .and_then(|variant| variant.fields().first())
                .copied()
                .ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("typed JSON variant {variant} has no payload"),
                    )
                })
        };
        let bool_ty = field_type(1)?;
        let number_ty = field_type(2)?;
        let text_ty = field_type(3)?;
        let list_ty = field_type(4)?;
        let map_ty = field_type(5)?;
        let semantic = |ty: ValueTypeId| {
            representations
                .value_type(ty)
                .map(loom_codegen_ir::ValueType::semantic)
        };
        if semantic(bool_ty) != Some(&Type::Bool)
            || semantic(number_ty) != Some(&Type::Float)
            || semantic(text_ty) != Some(&Type::Text)
            || self.list_element_type(list_ty)? != json_ty
            || self.text_map_value_type(map_ty)? != json_ty
            || !matches!(
                semantic(list_ty),
                Some(Type::List(element)) if element.as_ref() == &json_semantic
            )
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed JSON payload types do not match the canonical recursive shape",
            ));
        }

        let json_layout = self.sum_layout(json_ty)?;
        let json_physical = json_layout.physical.into_struct_type();
        let json_size = self.target_data.get_abi_size(&json_physical);
        let json_alignment = u64::from(self.target_data.get_abi_alignment(&json_physical));
        let tag_offset = self
            .target_data
            .offset_of_element(&json_physical, 0)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "JSON tag offset is missing"))?;
        let tag_type = self
            .sum_tag_type(json_layout.tag)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed JSON tag type is missing"))?;
        let tag_size = self.target_data.get_abi_size(&tag_type);
        let bool_payload_offset = self.sum_payload_field_offset(json_ty, 1, 0)?;
        let number_payload_offset = self.sum_payload_field_offset(json_ty, 2, 0)?;
        let text_payload_offset = self.sum_payload_field_offset(json_ty, 3, 0)?;
        let array_payload_offset = self.sum_payload_field_offset(json_ty, 4, 0)?;
        let object_payload_offset = self.sum_payload_field_offset(json_ty, 5, 0)?;

        let list = self.list_layout(list_ty)?;
        let list_length_offset = self
            .target_data
            .offset_of_element(&list.object, 0)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "JSON List length offset is missing")
            })?;
        let list_capacity_offset = self
            .target_data
            .offset_of_element(&list.object, 1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "JSON List capacity offset is missing")
            })?;
        let list_data_offset = self
            .target_data
            .offset_of_element(&list.object, 2)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "JSON List data offset is missing")
            })?;

        let map = self.text_map_layout(map_ty)?;
        let map_length_offset = self
            .target_data
            .offset_of_element(&map.object, 0)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "JSON map length offset is missing")
            })?;
        let map_data_offset = self
            .target_data
            .offset_of_element(&map.object, 1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "JSON map data offset is missing"))?;
        let map_key_offset = self
            .target_data
            .offset_of_element(&map.entry, 0)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "JSON map key offset is missing"))?;
        let map_value_offset = self
            .target_data
            .offset_of_element(&map.entry, 1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "JSON map value offset is missing")
            })?;

        let descriptor_type = self.context.struct_type(
            &std::iter::once(self.context.i32_type().into())
                .chain(std::iter::once(self.context.i32_type().into()))
                .chain(std::iter::repeat_n(self.context.i64_type().into(), 18))
                .collect::<Vec<_>>(),
            false,
        );
        let descriptor_size = self.target_data.get_abi_size(&descriptor_type);
        let descriptor_alignment = self.target_data.get_abi_alignment(&descriptor_type);
        if descriptor_size != 152 || descriptor_alignment != 8 {
            return Err(CodegenError::new(
                "LcirTypedJsonAbiMismatch",
                format!(
                    "LLVM target gives typed JSON descriptor size/alignment {descriptor_size}/{descriptor_alignment}, expected 152/8"
                ),
            ));
        }
        let u64_value = |value: u64| -> BasicValueEnum<'ctx> {
            self.context.i64_type().const_int(value, false).into()
        };
        let descriptor = self
            .module
            .add_global(descriptor_type, None, &descriptor_name);
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.context
                    .i32_type()
                    .const_int(u64::from(TYPED_JSON_ABI_VERSION), false)
                    .into(),
                self.context.i32_type().const_zero().into(),
                u64_value(json_size),
                u64_value(json_alignment),
                u64_value(tag_offset),
                u64_value(tag_size),
                u64_value(bool_payload_offset),
                u64_value(number_payload_offset),
                u64_value(text_payload_offset),
                u64_value(array_payload_offset),
                u64_value(object_payload_offset),
                u64_value(list_length_offset),
                u64_value(list_capacity_offset),
                u64_value(list_data_offset),
                u64_value(list.element_stride),
                u64_value(map_length_offset),
                u64_value(map_data_offset),
                u64_value(map.entry_stride),
                u64_value(map_key_offset),
                u64_value(map_value_offset),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(descriptor.as_pointer_value())
    }

    fn dynamic_candidate_type(
        &self,
        view: ValueTypeId,
        variant: u32,
    ) -> Result<ValueTypeId, CodegenError> {
        self.artifact
            .representations()
            .dynamic(view)
            .and_then(|dynamic| {
                usize::try_from(variant)
                    .ok()
                    .and_then(|index| dynamic.candidates().get(index))
            })
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("dynamic View {view} has no candidate {variant}"),
                )
            })
    }

    fn dynamic_candidate_layout(
        &self,
        view: ValueTypeId,
        variant: u32,
    ) -> Result<DynamicCandidateLayout<'ctx>, CodegenError> {
        let candidate = self.dynamic_candidate_type(view, variant)?;
        let payload = self.llvm_type(candidate)?;
        let object = self
            .context
            .struct_type(&[self.context.i32_type().into(), payload], false);
        let size = self.target_data.get_abi_size(&object);
        let align = u64::from(self.target_data.get_abi_alignment(&object));
        if size == 0
            || size > GC_MAX_OBJECT_BYTES
            || align == 0
            || !align.is_power_of_two()
            || align > GC_MAX_OBJECT_ALIGNMENT
        {
            return Err(CodegenError::new(
                "LcirDynamicDescriptorUnsupported",
                format!(
                    "dynamic View {view} candidate {variant} has unsupported object size/alignment {size}/{align}"
                ),
            ));
        }
        let payload_base = self
            .target_data
            .offset_of_element(&object, 1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "dynamic payload offset is missing")
            })?;
        let mut pointer_offsets = self
            .managed_element_offsets(candidate)?
            .into_iter()
            .map(|offset| {
                payload_base.checked_add(offset).ok_or_else(|| {
                    CodegenError::new(
                        "ProgramTooLarge",
                        "dynamic payload pointer offset overflowed",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        pointer_offsets.sort_unstable();
        pointer_offsets.dedup();
        let pointer_size = self.target_data.get_abi_size(&self.ptr_type);
        let pointer_align = u64::from(self.target_data.get_abi_alignment(&self.ptr_type));
        for offset in &pointer_offsets {
            if !offset.is_multiple_of(pointer_align)
                || offset
                    .checked_add(pointer_size)
                    .is_none_or(|end| end > size)
            {
                return Err(CodegenError::new(
                    "LcirDynamicDescriptorUnsupported",
                    format!(
                        "dynamic View {view} candidate {variant} has invalid managed offset {offset}"
                    ),
                ));
            }
        }
        Ok(DynamicCandidateLayout {
            object,
            payload,
            size,
            align,
            pointer_offsets,
        })
    }

    fn dynamic_descriptor(
        &self,
        view: ValueTypeId,
        variant: u32,
        layout: &DynamicCandidateLayout<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let descriptor_name = format!("loom.lcir.dyn.descriptor.{}.{}", view.raw(), variant);
        if let Some(existing) = self.module.get_global(&descriptor_name) {
            return Ok(existing.as_pointer_value());
        }
        let offsets_pointer = if layout.pointer_offsets.is_empty() {
            self.ptr_type.const_null()
        } else {
            let values = layout
                .pointer_offsets
                .iter()
                .map(|offset| self.context.i64_type().const_int(*offset, false))
                .collect::<Vec<_>>();
            let count = u32::try_from(values.len()).map_err(|_| {
                CodegenError::new(
                    "ProgramTooLarge",
                    "too many dynamic payload pointer offsets",
                )
            })?;
            let offsets = self.module.add_global(
                self.context.i64_type().array_type(count),
                None,
                &format!("loom.lcir.dyn.pointer_offsets.{}.{}", view.raw(), variant),
            );
            offsets.set_initializer(&self.context.i64_type().const_array(&values));
            offsets.set_constant(true);
            offsets.set_linkage(Linkage::Private);
            offsets.set_unnamed_address(UnnamedAddress::Global);
            offsets.as_pointer_value()
        };
        let descriptor_type = self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
            ],
            false,
        );
        let pointer_count = u64::try_from(layout.pointer_offsets.len()).map_err(|_| {
            CodegenError::new(
                "ProgramTooLarge",
                "too many dynamic payload pointer offsets",
            )
        })?;
        let descriptor = self
            .module
            .add_global(descriptor_type, None, &descriptor_name);
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.context
                    .i32_type()
                    .const_int(u64::from(TYPED_GC_ABI_VERSION), false)
                    .into(),
                self.context.i32_type().const_zero().into(),
                self.context.i64_type().const_int(layout.size, false).into(),
                self.context
                    .i64_type()
                    .const_int(layout.align, false)
                    .into(),
                self.context
                    .i64_type()
                    .const_int(pointer_count, false)
                    .into(),
                offsets_pointer.into(),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(descriptor.as_pointer_value())
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

    fn coroutine_callback(&self, id: InstanceId) -> Result<FunctionValue<'ctx>, CodegenError> {
        self.coroutine_callbacks
            .get(id.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR coroutine {id} has no resume callback"),
                )
            })
    }

    fn coroutine_layout(&self, id: InstanceId) -> Result<&CoroutineLayout<'ctx>, CodegenError> {
        self.coroutine_layouts
            .get(id.index())
            .and_then(Option::as_ref)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR coroutine {id} has no physical frame layout"),
                )
            })
    }

    fn task_join_layout(&self, id: InstructionId) -> Result<&TaskJoinLayout<'ctx>, CodegenError> {
        let shape = self.task_join_shapes.get(&id).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("typed Task join instruction {id} has no exact shape"),
            )
        })?;
        self.task_join_layouts.get(shape).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("typed Task join instruction {id} has no generated descriptor"),
            )
        })
    }

    fn io_task_layout(&self, id: InstructionId) -> Result<&IoTaskLayout<'ctx>, CodegenError> {
        let shape = self.io_task_shapes.get(&id).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("typed I/O instruction {id} has no exact shape"),
            )
        })?;
        self.io_task_layouts.get(shape).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("typed I/O instruction {id} has no generated descriptor"),
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

    fn libc_memcmp(&self) -> FunctionValue<'ctx> {
        self.module.get_function("memcmp").unwrap_or_else(|| {
            let function_type = self.context.i32_type().fn_type(
                &[
                    self.ptr_type.into(),
                    self.ptr_type.into(),
                    self.context.i64_type().into(),
                ],
                false,
            );
            self.module.add_function("memcmp", function_type, None)
        })
    }

    fn runtime_text_concat_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TEXT_CONCAT_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TEXT_CONCAT_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_text_get_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TEXT_GET_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TEXT_GET_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_process_arguments_initialize_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[self.context.i32_type().into(), self.ptr_type.into()],
                    false,
                );
                self.module.add_function(
                    PROCESS_ARGUMENTS_INITIALIZE_TYPED_SYMBOL,
                    function_type,
                    None,
                )
            })
    }

    fn runtime_process_argument_count_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    PROCESS_ARGUMENT_COUNT_TYPED_SYMBOL,
                    self.context.i64_type().fn_type(&[], false),
                    None,
                )
            })
    }

    fn runtime_process_argument_at_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(PROCESS_ARGUMENT_AT_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[self.context.i64_type().into(), self.ptr_type.into()],
                    false,
                );
                self.module
                    .add_function(PROCESS_ARGUMENT_AT_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_process_environment_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(PROCESS_ENVIRONMENT_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function(PROCESS_ENVIRONMENT_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_text_from_utf8_units_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TEXT_FROM_UTF8_UNITS_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TEXT_FROM_UTF8_UNITS_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_path_join_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(PATH_JOIN_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(PATH_JOIN_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_bytes_append_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(BYTES_APPEND_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(BYTES_APPEND_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_bytes_decode_utf8_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(BYTES_DECODE_UTF8_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self
                    .context
                    .i32_type()
                    .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
                self.module
                    .add_function(BYTES_DECODE_UTF8_TYPED_SYMBOL, function_type, None)
            })
    }

    fn typed_repeated_alloc(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_GC_REPEATED_ALLOC_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_GC_REPEATED_ALLOC_SYMBOL, function_type, None)
            })
    }

    fn typed_alloc(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_GC_ALLOC_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_GC_ALLOC_SYMBOL, function_type, None)
            })
    }

    fn runtime_format_float_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(FORMAT_FLOAT_TYPED_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[self.context.f64_type().into(), self.ptr_type.into()],
                    false,
                );
                self.module
                    .add_function(FORMAT_FLOAT_TYPED_SYMBOL, function_type, None)
            })
    }

    fn runtime_json_format_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_JSON_FORMAT_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_JSON_FORMAT_SYMBOL, function_type, None)
            })
    }

    fn runtime_parse_float(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(PARSE_FLOAT_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(PARSE_FLOAT_SYMBOL, function_type, None)
            })
    }

    fn typed_resource_close(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_RESOURCE_CLOSE_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.context.i32_type().into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_RESOURCE_CLOSE_SYMBOL, function_type, None)
            })
    }

    fn runtime_log_write_typed(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_LOG_WRITE_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.context.i32_type().into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_LOG_WRITE_SYMBOL, function_type, None)
            })
    }

    fn typed_task_create(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_TASK_CREATE_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_TASK_CREATE_SYMBOL,
                    self.ptr_type
                        .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false),
                    None,
                )
            })
    }

    fn typed_task_frame(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_TASK_FRAME_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_TASK_FRAME_SYMBOL,
                    self.ptr_type.fn_type(&[self.ptr_type.into()], false),
                    None,
                )
            })
    }

    fn typed_task_initialize(&self) -> FunctionValue<'ctx> {
        self.task_i32_u64_function(TYPED_TASK_INITIALIZE_SYMBOL)
    }

    fn typed_task_publish(&self) -> FunctionValue<'ctx> {
        self.executor_task_status_function(TYPED_TASK_PUBLISH_SYMBOL)
    }

    fn typed_task_set_root_state(&self) -> FunctionValue<'ctx> {
        self.task_i32_u64_function(TYPED_TASK_SET_ROOT_STATE_SYMBOL)
    }

    fn typed_task_publish_result(&self) -> FunctionValue<'ctx> {
        self.task_status_function(TYPED_TASK_PUBLISH_RESULT_SYMBOL)
    }

    fn typed_task_is_cancel_requested(&self) -> FunctionValue<'ctx> {
        self.task_status_function(TYPED_TASK_IS_CANCEL_REQUESTED_SYMBOL)
    }

    fn typed_task_take_result(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_TASK_TAKE_RESULT_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.context.i64_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_TASK_TAKE_RESULT_SYMBOL, function_type, None)
            })
    }

    fn typed_task_take_outcome(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_TASK_TAKE_OUTCOME_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.context.i64_type().into(),
                        self.context.i64_type().into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function(TYPED_TASK_TAKE_OUTCOME_SYMBOL, function_type, None)
            })
    }

    fn take_typed_task_result_exact(
        &self,
        child: PointerValue<'ctx>,
        output: ValueTypeId,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let physical = self.llvm_type(output)?;
        let size = self.target_data.get_abi_size(&physical);
        let alignment = u64::from(self.target_data.get_abi_alignment(&physical));
        let storage = (size != 0)
            .then(|| {
                self.builder
                    .build_alloca(physical, &format!("{name}.storage"))
                    .map_err(builder_error)
            })
            .transpose()?;
        let status = call_int(
            &self.builder,
            self.typed_task_take_result(),
            &[
                child.into(),
                storage.unwrap_or_else(|| self.ptr_type.const_null()).into(),
                self.context.i64_type().const_int(size, false).into(),
                self.context.i64_type().const_int(alignment, false).into(),
            ],
            &format!("{name}.take"),
        )?;
        self.require_zero_status(status, &format!("{name}.take"))?;
        if let Some(storage) = storage {
            self.builder
                .build_load(physical, storage, &format!("{name}.value"))
                .map_err(builder_error)
        } else {
            self.zero(output)
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "exact callback-side sum construction keeps payload layout, tag layout, and carrier copying in one audited ABI path"
    )]
    fn construct_sum_exact(
        &self,
        ty: ValueTypeId,
        variant: u32,
        payload: &[BasicValueEnum<'ctx>],
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let layout = self.sum_layout(ty)?;
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
                    "sum type {ty} variant {variant} has {} physical fields but {} values",
                    payload_type.count_fields(),
                    payload.len()
                ),
            ));
        }
        let mut payload_value = payload_type.get_undef();
        for (index, value) in payload.iter().copied().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many sum fields"))?;
            payload_value = self
                .builder
                .build_insert_value(payload_value, value, index, &format!("{name}.payload"))
                .map_err(builder_error)?
                .into_struct_value();
        }
        match layout.tag {
            SumTagRepr::Tagless => Ok(payload_value.into()),
            SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                let tag = self
                    .sum_tag_type(layout.tag)
                    .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sum tag is missing"))?
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
                let physical = layout.physical.into_struct_type();
                let storage = self
                    .builder
                    .build_alloca(physical, &format!("{name}.storage"))
                    .map_err(builder_error)?;
                self.builder
                    .build_store(storage, physical.const_zero())
                    .map_err(builder_error)?;
                let tag_pointer = self
                    .builder
                    .build_struct_gep(physical, storage, 0, &format!("{name}.tag.pointer"))
                    .map_err(builder_error)?;
                self.builder
                    .build_store(tag_pointer, tag)
                    .map_err(builder_error)?;
                let payload_size = self.target_data.get_abi_size(&payload_type);
                self.charge_sum_carrier_emission_work(payload_size)?;
                if payload_size != 0 {
                    let carrier = self
                        .builder
                        .build_struct_gep(physical, storage, 1, &format!("{name}.carrier.pointer"))
                        .map_err(builder_error)?;
                    let bytes = self
                        .builder
                        .build_struct_gep(
                            carrier_type,
                            carrier,
                            1,
                            &format!("{name}.bytes.pointer"),
                        )
                        .map_err(builder_error)?;
                    let address = self
                        .builder
                        .build_ptr_to_int(
                            bytes,
                            self.context.i64_type(),
                            &format!("{name}.bytes.address"),
                        )
                        .map_err(builder_error)?;
                    let address = self
                        .builder
                        .build_int_add(
                            address,
                            self.context
                                .i64_type()
                                .const_int(layout.payload_byte_offset(variant_index)?, false),
                            &format!("{name}.payload.address"),
                        )
                        .map_err(builder_error)?;
                    let destination = self
                        .builder
                        .build_int_to_ptr(address, self.ptr_type, &format!("{name}.destination"))
                        .map_err(builder_error)?;
                    let source = self
                        .builder
                        .build_alloca(payload_type, &format!("{name}.payload.storage"))
                        .map_err(builder_error)?;
                    self.builder
                        .build_store(source, payload_value)
                        .map_err(builder_error)?;
                    let alignment = self.target_data.get_abi_alignment(&payload_type);
                    self.builder
                        .build_memcpy(
                            destination,
                            alignment,
                            source,
                            alignment,
                            self.context.i64_type().const_int(payload_size, false),
                        )
                        .map_err(builder_error)?;
                }
                self.builder
                    .build_load(physical, storage, &format!("{name}.value"))
                    .map_err(builder_error)
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exact outcome capture keeps the terminal runtime ABI and canonical three-variant sum construction together"
    )]
    fn take_typed_task_outcome_exact(
        &self,
        child: PointerValue<'ctx>,
        output: ValueTypeId,
        outcome: ValueTypeId,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let fault = self
            .sum_repr(outcome)?
            .variants()
            .get(usize::try_from(TASK_OUTCOME_FAULTED_VARIANT).map_err(|_| {
                CodegenError::new("LlvmAbiDefect", "TaskOutcome fault variant overflowed")
            })?)
            .and_then(|variant| variant.fields().first())
            .copied()
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "TaskOutcome fault payload is missing")
            })?;
        let output_physical = self.llvm_type(output)?;
        let output_size = self.target_data.get_abi_size(&output_physical);
        let output_alignment = u64::from(self.target_data.get_abi_alignment(&output_physical));
        let output_storage = (output_size != 0)
            .then(|| {
                self.builder
                    .build_alloca(output_physical, &format!("{name}.result.storage"))
                    .map_err(builder_error)
            })
            .transpose()?;
        let code_output = self
            .builder
            .build_alloca(self.ptr_type, &format!("{name}.code.output"))
            .map_err(builder_error)?;
        let message_output = self
            .builder
            .build_alloca(self.ptr_type, &format!("{name}.message.output"))
            .map_err(builder_error)?;
        for cell in [code_output, message_output] {
            self.builder
                .build_store(cell, self.ptr_type.const_null())
                .map_err(builder_error)?;
        }
        let status = call_int(
            &self.builder,
            self.typed_task_take_outcome(),
            &[
                child.into(),
                output_storage
                    .unwrap_or_else(|| self.ptr_type.const_null())
                    .into(),
                self.context.i64_type().const_int(output_size, false).into(),
                self.context
                    .i64_type()
                    .const_int(output_alignment, false)
                    .into(),
                code_output.into(),
                message_output.into(),
            ],
            &format!("{name}.take"),
        )?;
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "outcome has no function"))?;
        let completed = self
            .context
            .append_basic_block(function, "join.outcome.completed");
        let faulted = self
            .context
            .append_basic_block(function, "join.outcome.faulted");
        let cancelled = self
            .context
            .append_basic_block(function, "join.outcome.cancelled");
        let invalid = self
            .context
            .append_basic_block(function, "join.outcome.invalid");
        let merge = self
            .context
            .append_basic_block(function, "join.outcome.merge");
        self.builder
            .build_switch(
                status,
                invalid,
                &[
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_COMPLETED as u64, false),
                        completed,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_FAULTED as u64, false),
                        faulted,
                    ),
                    (
                        self.context
                            .i32_type()
                            .const_int(TASK_CANCELLED as u64, false),
                        cancelled,
                    ),
                ],
            )
            .map_err(builder_error)?;

        self.builder.position_at_end(invalid);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.invalid.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;

        self.builder.position_at_end(completed);
        let completed_payload = if let Some(storage) = output_storage {
            self.builder
                .build_load(
                    output_physical,
                    storage,
                    &format!("{name}.completed.payload"),
                )
                .map_err(builder_error)?
        } else {
            self.zero(output)?
        };
        let completed_value = self.construct_sum_exact(
            outcome,
            TASK_OUTCOME_COMPLETED_VARIANT,
            &[completed_payload],
            &format!("{name}.completed"),
        )?;
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(faulted);
        let code = self
            .builder
            .build_load(self.ptr_type, code_output, &format!("{name}.fault.code"))
            .map_err(builder_error)?;
        let message = self
            .builder
            .build_load(
                self.ptr_type,
                message_output,
                &format!("{name}.fault.message"),
            )
            .map_err(builder_error)?;
        let fault_type = self.llvm_type(fault)?.into_struct_type();
        let fault_value = self
            .builder
            .build_insert_value(
                fault_type.get_undef(),
                code,
                0,
                &format!("{name}.fault.code"),
            )
            .map_err(builder_error)?
            .into_struct_value();
        let fault_value = self
            .builder
            .build_insert_value(fault_value, message, 1, &format!("{name}.fault.message"))
            .map_err(builder_error)?
            .into_struct_value();
        let faulted_value = self.construct_sum_exact(
            outcome,
            TASK_OUTCOME_FAULTED_VARIANT,
            &[fault_value.into()],
            &format!("{name}.faulted"),
        )?;
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(cancelled);
        let cancelled_value = self.construct_sum_exact(
            outcome,
            TASK_OUTCOME_CANCELLED_VARIANT,
            &[],
            &format!("{name}.cancelled"),
        )?;
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(merge);
        let phi = self
            .builder
            .build_phi(self.llvm_type(outcome)?, &format!("{name}.value"))
            .map_err(builder_error)?;
        phi.add_incoming(&[
            (&completed_value, completed),
            (&faulted_value, faulted),
            (&cancelled_value, cancelled),
        ]);
        Ok(phi.as_basic_value())
    }

    fn typed_task_abort_unpublished(&self) -> FunctionValue<'ctx> {
        self.executor_task_status_function(TYPED_TASK_ABORT_UNPUBLISHED_SYMBOL)
    }

    fn typed_task_publish_adopting(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_TASK_PUBLISH_ADOPTING_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_TASK_PUBLISH_ADOPTING_SYMBOL,
                    self.context.i32_type().fn_type(
                        &[
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.context.i64_type().into(),
                        ],
                        false,
                    ),
                    None,
                )
            })
    }

    fn typed_timer_task_create(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_TIMER_TASK_CREATE_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_TIMER_TASK_CREATE_SYMBOL,
                    self.ptr_type.fn_type(
                        &[self.ptr_type.into(), self.context.i64_type().into()],
                        false,
                    ),
                    None,
                )
            })
    }

    fn typed_io_request_type(&self) -> StructType<'ctx> {
        let byte_view = self.context.struct_type(
            &[self.ptr_type.into(), self.context.i64_type().into()],
            false,
        );
        self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.context.i64_type().into(),
                byte_view.into(),
                self.context.i64_type().into(),
            ],
            false,
        )
    }

    fn typed_io_outcome_type(&self) -> StructType<'ctx> {
        self.context.struct_type(
            &[
                self.context.i32_type().into(),
                self.context.i32_type().into(),
                self.context.i64_type().into(),
            ],
            false,
        )
    }

    fn typed_io_task_create(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_IO_TASK_CREATE_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_IO_TASK_CREATE_SYMBOL,
                    self.ptr_type.fn_type(
                        &[
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                        ],
                        false,
                    ),
                    None,
                )
            })
    }

    fn typed_io_poll(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_IO_POLL_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_IO_POLL_SYMBOL,
                    self.context.i32_type().fn_type(
                        &[
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                        ],
                        false,
                    ),
                    None,
                )
            })
    }

    fn typed_io_cancel(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(TYPED_IO_CANCEL_SYMBOL)
            .unwrap_or_else(|| {
                self.module.add_function(
                    TYPED_IO_CANCEL_SYMBOL,
                    self.context.i32_type().fn_type(
                        &[
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                            self.ptr_type.into(),
                        ],
                        false,
                    ),
                    None,
                )
            })
    }

    fn wait_now_ns(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_wait_now_ns")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "loom_wait_now_ns",
                    self.context.i64_type().fn_type(&[], false),
                    None,
                )
            })
    }

    fn task_i32_u64_function(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self.context.i32_type().fn_type(
                &[self.ptr_type.into(), self.context.i64_type().into()],
                false,
            );
            self.module.add_function(name, function_type, None)
        })
    }

    fn task_status_function(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into()], false);
            self.module.add_function(name, function_type, None)
        })
    }

    fn executor_task_status_function(&self, name: &str) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let function_type = self
                .context
                .i32_type()
                .fn_type(&[self.ptr_type.into(), self.ptr_type.into()], false);
            self.module.add_function(name, function_type, None)
        })
    }

    fn task_prepare_join(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_prepare_join")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.context.i32_type().into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_prepare_join", function_type, None)
            })
    }

    fn task_add_join_child(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_add_join_child")
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                        self.ptr_type.into(),
                    ],
                    false,
                );
                self.module
                    .add_function("loom_task_add_join_child", function_type, None)
            })
    }

    fn task_suspend_join(&self) -> FunctionValue<'ctx> {
        self.executor_task_status_function("loom_task_suspend_join")
    }

    fn task_join_step(&self) -> FunctionValue<'ctx> {
        self.task_status_function("loom_task_join_step")
    }

    fn task_join_winner(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_task_join_winner")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "loom_task_join_winner",
                    self.context
                        .i64_type()
                        .fn_type(&[self.ptr_type.into()], false),
                    None,
                )
            })
    }

    fn executor_create_for_runtime(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_create_for_runtime_v1")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "loom_executor_create_for_runtime_v1",
                    self.ptr_type.fn_type(&[self.ptr_type.into()], false),
                    None,
                )
            })
    }

    fn executor_destroy(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function("loom_executor_destroy")
            .unwrap_or_else(|| {
                self.module.add_function(
                    "loom_executor_destroy",
                    self.context
                        .void_type()
                        .fn_type(&[self.ptr_type.into()], false),
                    None,
                )
            })
    }

    fn executor_run(&self) -> FunctionValue<'ctx> {
        self.executor_task_status_function("loom_executor_run")
    }

    fn task_report_fault(&self) -> FunctionValue<'ctx> {
        self.task_status_function("loom_task_report_fault")
    }

    fn typed_root_push(&self) -> FunctionValue<'ctx> {
        self.runtime_status_function(TYPED_GC_ROOT_PUSH_SYMBOL)
    }

    fn typed_root_pop(&self) -> FunctionValue<'ctx> {
        self.runtime_status_function(TYPED_GC_ROOT_POP_SYMBOL)
    }

    fn require_zero_status(&self, status: IntValue<'ctx>, name: &str) -> Result<(), CodegenError> {
        self.require_exact_status(status, 0, name)
    }

    fn require_exact_status(
        &self,
        status: IntValue<'ctx>,
        expected: u32,
        name: &str,
    ) -> Result<(), CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "status guard has no active function")
            })?;
        let success = self
            .context
            .append_basic_block(function, &format!("{name}.ok"));
        let failure = self
            .context
            .append_basic_block(function, &format!("{name}.failed"));
        let ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context
                    .i32_type()
                    .const_int(u64::from(expected), false),
                &format!("{name}.status.ok"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(ok, success, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(success);
        Ok(())
    }

    fn require_nonnegative_i64(
        &self,
        value: IntValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "integer guard has no active function")
            })?;
        let success = self
            .context
            .append_basic_block(function, &format!("{name}.ok"));
        let failure = self
            .context
            .append_basic_block(function, &format!("{name}.failed"));
        let valid = self
            .builder
            .build_int_compare(
                IntPredicate::SGT,
                value,
                self.context
                    .i64_type()
                    .const_int(PROCESS_ARGUMENT_COUNT_TYPED_INVALID.cast_unsigned(), true),
                &format!("{name}.nonnegative"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(valid, success, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(success);
        Ok(())
    }

    fn require_nonnull(&self, pointer: PointerValue<'ctx>, name: &str) -> Result<(), CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "pointer guard has no active function")
            })?;
        let success = self
            .context
            .append_basic_block(function, &format!("{name}.ok"));
        let failure = self
            .context
            .append_basic_block(function, &format!("{name}.failed"));
        let exists = self
            .builder
            .build_is_not_null(pointer, &format!("{name}.nonnull"))
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(exists, success, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(success);
        Ok(())
    }

    fn require_text_get_status(
        &self,
        status: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        self.require_missing_or_found_status(
            status,
            TEXT_GET_TYPED_MISSING,
            TEXT_GET_TYPED_FOUND,
            "text.get",
        )
    }

    fn require_missing_or_found_status(
        &self,
        status: IntValue<'ctx>,
        missing_status: i32,
        found_status: i32,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmBuilderFailed",
                    format!("{name} status guard has no active function"),
                )
            })?;
        let success = self
            .context
            .append_basic_block(function, &format!("{name}.status.ok"));
        let failure = self
            .context
            .append_basic_block(function, &format!("{name}.status.failed"));
        let missing = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_int(
                    u64::try_from(missing_status).expect("missing status is non-negative"),
                    false,
                ),
                &format!("{name}.missing"),
            )
            .map_err(builder_error)?;
        let found = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_int(
                    u64::try_from(found_status).expect("found status is non-negative"),
                    false,
                ),
                &format!("{name}.found"),
            )
            .map_err(builder_error)?;
        let valid = self
            .builder
            .build_or(missing, found, &format!("{name}.status.valid"))
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(valid, success, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.status.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(success);
        Ok(found)
    }

    fn require_bytes_decode_status(
        &self,
        status: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        self.require_decode_text_status(
            status,
            "bytes.decode_utf8",
            BYTES_DECODE_UTF8_TYPED_INVALID_UTF8,
        )
    }

    fn require_decode_text_status(
        &self,
        status: IntValue<'ctx>,
        name: &str,
        invalid_status: i32,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        self.require_zero_or_status(status, name, invalid_status)
    }

    fn require_zero_or_status(
        &self,
        status: IntValue<'ctx>,
        name: &str,
        ordinary_status: i32,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmBuilderFailed",
                    format!("{name} status guard has no active function"),
                )
            })?;
        let accepted = self
            .context
            .append_basic_block(function, &format!("{name}.status.ok"));
        let failure = self
            .context
            .append_basic_block(function, &format!("{name}.status.failed"));
        let success = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_zero(),
                &format!("{name}.valid"),
            )
            .map_err(builder_error)?;
        let ordinary_status = self.context.i32_type().const_int(
            u64::from(u32::from_ne_bytes(ordinary_status.to_ne_bytes())),
            false,
        );
        let ordinary = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                ordinary_status,
                &format!("{name}.ordinary_error"),
            )
            .map_err(builder_error)?;
        let recognized = self
            .builder
            .build_or(success, ordinary, &format!("{name}.status.valid"))
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(recognized, accepted, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.status.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(accepted);
        Ok(success)
    }

    fn require_float_parse_status(
        &self,
        status: IntValue<'ctx>,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let name = "parse.float";
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmBuilderFailed",
                    "Float parse status guard has no active function",
                )
            })?;
        let success = self
            .context
            .append_basic_block(function, &format!("{name}.status.ok"));
        let failure = self
            .context
            .append_basic_block(function, &format!("{name}.status.failed"));
        let ok = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_int(
                    u64::try_from(PARSE_FLOAT_STATUS_OK)
                        .expect("Float parse success status is nonnegative"),
                    false,
                ),
                &format!("{name}.ok"),
            )
            .map_err(builder_error)?;
        let invalid = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_int(
                    u64::try_from(PARSE_FLOAT_STATUS_INVALID_SYNTAX)
                        .expect("Float parse invalid-syntax status is nonnegative"),
                    false,
                ),
                &format!("{name}.invalid_syntax"),
            )
            .map_err(builder_error)?;
        let out_of_range = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context.i32_type().const_int(
                    u64::try_from(PARSE_FLOAT_STATUS_OUT_OF_RANGE)
                        .expect("Float parse out-of-range status is nonnegative"),
                    false,
                ),
                &format!("{name}.out_of_range"),
            )
            .map_err(builder_error)?;
        let known_failure = self
            .builder
            .build_or(invalid, out_of_range, &format!("{name}.known_failure"))
            .map_err(builder_error)?;
        let valid = self
            .builder
            .build_or(ok, known_failure, &format!("{name}.status.valid"))
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(valid, success, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], &format!("{name}.status.trap"))
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(success);
        Ok((ok, out_of_range))
    }

    fn require_json_format_status(
        &self,
        status: IntValue<'ctx>,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmBuilderFailed",
                    "JSON format status guard has no active function",
                )
            })?;
        let success = self
            .context
            .append_basic_block(function, "json.format.status.ok");
        let failure = self
            .context
            .append_basic_block(function, "json.format.status.failed");
        let equals = |expected: i32, name: &str| {
            self.builder
                .build_int_compare(
                    IntPredicate::EQ,
                    status,
                    self.context.i32_type().const_int(
                        u64::try_from(expected)
                            .expect("typed JSON public result statuses are nonnegative"),
                        false,
                    ),
                    name,
                )
                .map_err(builder_error)
        };
        let ok = equals(TYPED_JSON_FORMAT_OK, "json.format.ok")?;
        let depth = equals(TYPED_JSON_FORMAT_DEPTH_LIMIT, "json.format.depth_limit")?;
        let non_finite = equals(
            TYPED_JSON_FORMAT_NON_FINITE_NUMBER,
            "json.format.non_finite",
        )?;
        let known_error = self
            .builder
            .build_or(depth, non_finite, "json.format.known_error")
            .map_err(builder_error)?;
        let valid = self
            .builder
            .build_or(ok, known_error, "json.format.status.valid")
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(valid, success, failure)
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.builder
            .build_call(trap, &[], "json.format.status.trap")
            .map_err(builder_error)?;
        self.builder.build_unreachable().map_err(builder_error)?;
        self.builder.position_at_end(success);
        Ok((ok, depth, non_finite))
    }
}

#[derive(Clone, Copy)]
enum FaultEmission<'metadata> {
    Runtime { code: FaultCode, origin: Origin },
    Contract(&'metadata ContractFaultMetadata),
}

#[derive(Clone, Copy)]
struct CoroutineCallerSpan<'ctx> {
    file: IntValue<'ctx>,
    start: IntValue<'ctx>,
    end: IntValue<'ctx>,
}

struct CoroutineEmission<'ctx> {
    layout: CoroutineLayout<'ctx>,
    prologue: BasicBlock<'ctx>,
    dispatch: BasicBlock<'ctx>,
    cancel_dispatch: BasicBlock<'ctx>,
    cancel_start: BasicBlock<'ctx>,
    invalid_state: BasicBlock<'ctx>,
    resume_blocks: Vec<BasicBlock<'ctx>>,
    cancel_blocks: Vec<BasicBlock<'ctx>>,
    task: PointerValue<'ctx>,
    executor: PointerValue<'ctx>,
    frame: PointerValue<'ctx>,
}

struct AwaitExitTargets {
    mode: AwaitMode,
    origin: Origin,
    normal: ResultTarget,
    fault: UnwindTarget,
    cancel: BlockTarget,
}

struct FunctionEmitter<'backend, 'ctx, 'artifact> {
    backend: &'backend Backend<'ctx, 'artifact>,
    source: &'artifact Function,
    function: FunctionValue<'ctx>,
    coroutine: Option<CoroutineEmission<'ctx>>,
    blocks: Vec<BasicBlock<'ctx>>,
    emission_order: Vec<BlockId>,
    phis: Vec<Option<PhiValue<'ctx>>>,
    values: Vec<Option<BasicValueEnum<'ctx>>>,
    fault_context: Option<PointerValue<'ctx>>,
    executor_context: Option<PointerValue<'ctx>>,
    root_plan: ManagedRootPlan,
    root_slot_ranges: Vec<Option<(usize, usize)>>,
    root_cells: Vec<Option<PointerValue<'ctx>>>,
    root_frame: Option<PointerValue<'ctx>>,
    root_state: Option<PointerValue<'ctx>>,
    resource_close_token_cells: Vec<Option<PointerValue<'ctx>>>,
    managed_output_cells: Vec<Option<PointerValue<'ctx>>>,
    json_input_cells: Vec<Option<PointerValue<'ctx>>>,
    float_parse_output_cells: Vec<Option<PointerValue<'ctx>>>,
}

impl<'backend, 'ctx, 'artifact> FunctionEmitter<'backend, 'ctx, 'artifact> {
    #[expect(
        clippy::too_many_lines,
        reason = "one constructor must establish the coroutine callback, exact root plan, reachable LLVM blocks, and SSA storage before emission"
    )]
    fn new(
        backend: &'backend Backend<'ctx, 'artifact>,
        source: &'artifact Function,
    ) -> Result<Self, CodegenError> {
        let (function, coroutine, executor_context) = if source.coroutine().is_some() {
            let function = backend.coroutine_callback(source.id())?;
            let layout = backend.coroutine_layout(source.id())?.clone();
            let task = function
                .get_nth_param(0)
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "coroutine callback has no task")
                })?
                .into_pointer_value();
            let executor = function
                .get_nth_param(1)
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "coroutine callback has no executor")
                })?
                .into_pointer_value();
            let frame = function
                .get_nth_param(2)
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "coroutine callback has no frame")
                })?
                .into_pointer_value();
            task.set_name("__loom_task");
            executor.set_name("__loom_executor");
            frame.set_name("__loom_frame");
            let prologue = backend
                .context
                .append_basic_block(function, "coroutine.prologue");
            let dispatch = backend
                .context
                .append_basic_block(function, "coroutine.dispatch");
            let cancel_dispatch = backend
                .context
                .append_basic_block(function, "coroutine.cancel.dispatch");
            let cancel_start = backend
                .context
                .append_basic_block(function, "coroutine.cancel.start");
            let invalid_state = backend
                .context
                .append_basic_block(function, "coroutine.invalid_state");
            let resume_blocks = layout
                .suspensions
                .iter()
                .map(|suspension| {
                    backend.context.append_basic_block(
                        function,
                        &format!("coroutine.resume.{}", suspension.state),
                    )
                })
                .collect();
            let cancel_blocks = layout
                .suspensions
                .iter()
                .map(|suspension| {
                    backend.context.append_basic_block(
                        function,
                        &format!("coroutine.cancel.{}", suspension.state),
                    )
                })
                .collect();
            (
                function,
                Some(CoroutineEmission {
                    layout,
                    prologue,
                    dispatch,
                    cancel_dispatch,
                    cancel_start,
                    invalid_state,
                    resume_blocks,
                    cancel_blocks,
                    task,
                    executor,
                    frame,
                }),
                Some(executor),
            )
        } else {
            (backend.function(source.id())?, None, None)
        };
        let root_plan =
            plan_managed_roots(backend.artifact.program(), source.id()).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("could not derive managed-root plan for {}", source.id()),
                )
            })?;
        // This is a target-emission resource boundary, not unsupported source
        // coverage. Exceeding it is a deterministic ProgramTooLarge failure
        // and must remain on the exact typed primitive ABI.
        if u64::try_from(root_plan.slots().len()).unwrap_or(u64::MAX) > GC_MAX_ROOT_SLOTS
            || u64::try_from(root_plan.state_count()).unwrap_or(u64::MAX) > GC_MAX_ROOT_STATES
            || u64::try_from(root_plan.bitmaps().len()).unwrap_or(u64::MAX)
                > GC_MAX_ROOT_BITMAP_WORDS
        {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                format!("{} exceeds the typed shadow-root ABI limits", source.id()),
            ));
        }
        let root_slot_ranges = Self::managed_root_ranges(source, &root_plan)?;
        let root_slot_count = root_plan.slots().len();
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
            coroutine,
            blocks,
            emission_order,
            phis: vec![None; source.values().len()],
            values: vec![None; source.values().len()],
            fault_context: None,
            executor_context,
            root_plan,
            root_slot_ranges,
            root_cells: vec![None; root_slot_count],
            root_frame: None,
            root_state: None,
            resource_close_token_cells: vec![None; source.instructions().len()],
            managed_output_cells: vec![None; source.instructions().len()],
            json_input_cells: vec![None; source.instructions().len()],
            float_parse_output_cells: vec![None; source.instructions().len()],
        };
        emitter.prepare_parameters()?;
        Ok(emitter)
    }

    fn managed_root_ranges(
        source: &Function,
        plan: &ManagedRootPlan,
    ) -> Result<Vec<Option<(usize, usize)>>, CodegenError> {
        let mut ranges = vec![None; source.values().len()];
        for (index, slot) in plan.slots().iter().enumerate() {
            let range = ranges.get_mut(slot.value().index()).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("managed-root slot references missing {}", slot.value()),
                )
            })?;
            match range {
                None => *range = Some((index, index.saturating_add(1))),
                Some((_, end)) if *end == index => *end = index.saturating_add(1),
                Some(_) => {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        format!("managed-root slots for {} are not contiguous", slot.value()),
                    ));
                }
            }
        }
        Ok(ranges)
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
        if self.coroutine.is_some() {
            self.prepare_coroutine_fault_context()?;
            return self.prepare_coroutine_dispatch(entry, entry_block);
        }
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
        if self.source.effects().contains(Effects::NEEDS_EXECUTOR) {
            let index = self
                .source
                .signature()
                .params()
                .len()
                .checked_add(usize::from(
                    self.source.effects().contains(Effects::MAY_FAULT),
                ))
                .ok_or_else(|| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many LCIR parameters"))?;
            let value = self.function.get_nth_param(index).ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("{} is missing its executor pointer", self.source.id()),
                )
            })?;
            value.set_name("__loom_executor");
            self.executor_context = Some(value.into_pointer_value());
        }
        Ok(())
    }

    fn prepare_coroutine_fault_context(&mut self) -> Result<(), CodegenError> {
        if !self.source.effects().contains(Effects::MAY_FAULT) {
            return Ok(());
        }
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "coroutine fault context has no frame plan")
        })?;
        self.backend.builder.position_at_end(coroutine.prologue);
        let context = self
            .backend
            .builder
            .build_alloca(self.backend.fault_context_type, "fault.context")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(context, self.backend.fault_context_type.const_zero())
            .map_err(builder_error)?;
        let runtime_pointer = self
            .backend
            .builder
            .build_struct_gep(
                self.backend.fault_context_type,
                context,
                0,
                "fault.context.runtime.pointer",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(runtime_pointer, coroutine.executor)
            .map_err(builder_error)?;
        self.fault_context = Some(context);
        Ok(())
    }

    fn prepare_coroutine_dispatch(
        &mut self,
        entry: BlockId,
        entry_block: &loom_codegen_ir::Block,
    ) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "coroutine dispatch has no frame plan")
        })?;
        let layout = coroutine.layout.clone();
        let dispatch = coroutine.dispatch;
        let invalid_state = coroutine.invalid_state;
        let resume_blocks = coroutine.resume_blocks.clone();
        let frame = coroutine.frame;
        self.backend.builder.position_at_end(dispatch);
        let state_pointer = self
            .backend
            .builder
            .build_struct_gep(layout.frame, frame, 0, "coroutine.state.pointer")
            .map_err(builder_error)?;
        let state = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                state_pointer,
                "coroutine.state",
            )
            .map_err(builder_error)?
            .into_int_value();
        let start = self
            .backend
            .context
            .append_basic_block(self.function, "coroutine.start");
        let mut cases = Vec::with_capacity(1 + layout.suspensions.len());
        cases.push((self.backend.context.i64_type().const_zero(), start));
        for (suspension, block) in layout.suspensions.iter().zip(&resume_blocks) {
            cases.push((
                self.backend
                    .context
                    .i64_type()
                    .const_int(u64::from(suspension.state), false),
                *block,
            ));
        }
        self.backend
            .builder
            .build_switch(state, invalid_state, &cases)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(start);
        if entry_block.params().len() != layout.parameter_fields.len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "coroutine entry parameters do not match its frame layout",
            ));
        }
        for ((value_id, ty), field) in entry_block
            .params()
            .iter()
            .copied()
            .zip(self.source.signature().params().iter().copied())
            .zip(layout.parameter_fields.iter().copied())
        {
            let pointer = self
                .backend
                .builder
                .build_struct_gep(layout.frame, frame, field, "coroutine.parameter.pointer")
                .map_err(builder_error)?;
            let value = self
                .backend
                .builder
                .build_load(
                    self.backend.llvm_type(ty)?,
                    pointer,
                    &format!("v{}", value_id.raw()),
                )
                .map_err(builder_error)?;
            self.values[value_id.index()] = Some(value);
        }
        self.backend
            .builder
            .build_unconditional_branch(self.blocks[entry.index()])
            .map_err(builder_error)?;

        self.prepare_coroutine_cancel_dispatch()
    }

    fn prepare_coroutine_cancel_dispatch(&self) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "coroutine cancellation dispatch has no frame plan",
            )
        })?;
        self.backend
            .builder
            .position_at_end(coroutine.cancel_dispatch);
        let cancel_state_pointer = self
            .backend
            .builder
            .build_struct_gep(
                coroutine.layout.frame,
                coroutine.frame,
                0,
                "coroutine.cancel.state.pointer",
            )
            .map_err(builder_error)?;
        let cancel_state = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                cancel_state_pointer,
                "coroutine.cancel.state",
            )
            .map_err(builder_error)?
            .into_int_value();
        let mut cancel_cases = Vec::with_capacity(1 + coroutine.layout.suspensions.len());
        cancel_cases.push((
            self.backend.context.i64_type().const_zero(),
            coroutine.cancel_start,
        ));
        for (suspension, block) in coroutine
            .layout
            .suspensions
            .iter()
            .zip(&coroutine.cancel_blocks)
        {
            cancel_cases.push((
                self.backend
                    .context
                    .i64_type()
                    .const_int(u64::from(suspension.state), false),
                *block,
            ));
        }
        self.backend
            .builder
            .build_switch(cancel_state, coroutine.invalid_state, &cancel_cases)
            .map_err(builder_error)?;
        Ok(())
    }

    fn prepare_root_frame(&mut self) -> Result<(), CodegenError> {
        if self.root_plan.slots().is_empty() {
            return Ok(());
        }
        let entry = self.source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", self.source.id()),
            )
        })?;
        self.backend
            .builder
            .position_at_end(self.allocation_block(entry)?);
        let slots = self.root_plan.slots().to_vec();
        let slot_array = self.prepare_root_cells(entry, &slots)?;
        let descriptor = self.emit_root_descriptor(slots.len())?;
        self.link_root_frame(descriptor, slot_array)?;
        if self.coroutine.is_none() {
            self.blocks[entry.index()] = self.current_block()?;
        }
        Ok(())
    }

    fn prepare_resource_close_token_cells(&mut self) -> Result<(), CodegenError> {
        let entry = self.source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", self.source.id()),
            )
        })?;
        self.backend
            .builder
            .position_at_end(self.allocation_block(entry)?);
        for instruction in self.source.instructions() {
            if !matches!(instruction.kind(), InstructionKind::ResourceClose { .. }) {
                continue;
            }
            let cell = self
                .backend
                .builder
                .build_alloca(
                    self.backend.context.i64_type(),
                    &format!("resource.close.i{}.token.cell", instruction.id().raw()),
                )
                .map_err(builder_error)?;
            self.resource_close_token_cells[instruction.id().index()] = Some(cell);
        }
        Ok(())
    }

    fn prepare_managed_output_cells(&mut self) -> Result<(), CodegenError> {
        let entry = self.source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", self.source.id()),
            )
        })?;
        self.backend
            .builder
            .position_at_end(self.allocation_block(entry)?);
        for instruction in self.source.instructions() {
            if !matches!(
                instruction.kind(),
                InstructionKind::TextConcat { .. }
                    | InstructionKind::TextGet { .. }
                    | InstructionKind::TextFromUtf8Units { .. }
                    | InstructionKind::ProcessArgumentAt { .. }
                    | InstructionKind::ProcessEnvironment { .. }
                    | InstructionKind::PathJoin { .. }
                    | InstructionKind::BytesAppend { .. }
                    | InstructionKind::BytesDecodeUtf8 { .. }
                    | InstructionKind::FloatFormat { .. }
                    | InstructionKind::JsonFormat { .. }
            ) {
                continue;
            }
            let cell = self
                .backend
                .builder
                .build_alloca(
                    self.backend.ptr_type,
                    &format!("managed.output.i{}", instruction.id().raw()),
                )
                .map_err(builder_error)?;
            self.managed_output_cells[instruction.id().index()] = Some(cell);
        }
        Ok(())
    }

    fn managed_output_cell(
        &self,
        instruction: InstructionId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.managed_output_cells
            .get(instruction.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("collecting managed instruction {instruction} has no output cell"),
                )
            })
    }

    fn prepare_json_input_cells(&mut self) -> Result<(), CodegenError> {
        let entry = self.source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", self.source.id()),
            )
        })?;
        self.backend
            .builder
            .position_at_end(self.allocation_block(entry)?);
        for instruction in self.source.instructions() {
            let json = match instruction.kind() {
                InstructionKind::JsonFormat { json, .. } => *json,
                _ => continue,
            };
            let json_ty = self
                .source
                .value(json)
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", format!("JSON operand {json} is missing"))
                })?
                .ty();
            let cell = self
                .backend
                .builder
                .build_alloca(
                    self.backend.llvm_type(json_ty)?,
                    &format!("json.input.i{}", instruction.id().raw()),
                )
                .map_err(builder_error)?;
            self.json_input_cells[instruction.id().index()] = Some(cell);
        }
        Ok(())
    }

    fn json_input_cell(
        &self,
        instruction: InstructionId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.json_input_cells
            .get(instruction.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("JSON formatting instruction {instruction} has no input cell"),
                )
            })
    }

    fn prepare_float_parse_output_cells(&mut self) -> Result<(), CodegenError> {
        let entry = self.source.entry().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no entry block", self.source.id()),
            )
        })?;
        self.backend
            .builder
            .position_at_end(self.allocation_block(entry)?);
        for instruction in self.source.instructions() {
            if !matches!(instruction.kind(), InstructionKind::FloatParseStatus { .. }) {
                continue;
            }
            let cell = self
                .backend
                .builder
                .build_alloca(
                    self.backend.context.f64_type(),
                    &format!("parse.float.output.i{}", instruction.id().raw()),
                )
                .map_err(builder_error)?;
            self.float_parse_output_cells[instruction.id().index()] = Some(cell);
        }
        Ok(())
    }

    fn float_parse_output_cell(
        &self,
        instruction: InstructionId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.float_parse_output_cells
            .get(instruction.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("Float parse instruction {instruction} has no output cell"),
                )
            })
    }

    fn prepare_root_cells(
        &mut self,
        entry: BlockId,
        slots: &[ManagedRootSlot],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        for (index, slot) in slots.iter().enumerate() {
            let projection = Self::managed_projection_name(slot.projection());
            let name = if projection.is_empty() {
                format!("managed.root.v{}", slot.value().raw())
            } else {
                format!("managed.root.v{}.{projection}", slot.value().raw())
            };
            let cell = self
                .backend
                .builder
                .build_alloca(self.backend.ptr_type, &name)
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(cell, self.backend.ptr_type.const_null())
                .map_err(builder_error)?;
            self.root_cells[index] = Some(cell);
        }
        if self.coroutine.is_none() {
            let entry_values = slots
                .iter()
                .map(ManagedRootSlot::value)
                .collect::<BTreeSet<_>>();
            for value in entry_values {
                if matches!(
                    self.source
                        .value(value)
                        .map(loom_codegen_ir::Value::definition),
                    Some(ValueDefinition::BlockParameter { block, .. }) if block == entry
                ) {
                    let parameter = self.values[value.index()].ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("entry managed parameter {value} has no LLVM value"),
                        )
                    })?;
                    self.publish_root_value(value, parameter)?;
                }
            }
        }

        let slot_fields = vec![self.backend.ptr_type.into(); slots.len()];
        let slot_array_type = self.backend.context.struct_type(&slot_fields, false);
        let slot_array = self
            .backend
            .builder
            .build_alloca(slot_array_type, "managed.root.slots")
            .map_err(builder_error)?;
        for (slot_index, _slot) in slots.iter().enumerate() {
            let index = u32::try_from(slot_index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many managed roots"))?;
            let field = self
                .backend
                .builder
                .build_struct_gep(slot_array_type, slot_array, index, "managed.root.slot")
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(
                    field,
                    self.root_cells[slot_index].ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "managed-root cell is missing")
                    })?,
                )
                .map_err(builder_error)?;
        }
        Ok(slot_array)
    }

    fn emit_root_descriptor(&self, slot_count: usize) -> Result<PointerValue<'ctx>, CodegenError> {
        let bitmap_values = self
            .root_plan
            .bitmaps()
            .iter()
            .map(|word| self.backend.context.i64_type().const_int(*word, false))
            .collect::<Vec<_>>();
        let bitmap_length = u32::try_from(bitmap_values.len()).map_err(|_| {
            CodegenError::new("ProgramTooLarge", "typed root bitmap exceeds LLVM limits")
        })?;
        let bitmap_type = self.backend.context.i64_type().array_type(bitmap_length);
        let bitmap = self.backend.module.add_global(
            bitmap_type,
            None,
            &self.backend.unique("managed.root.bitmaps"),
        );
        bitmap.set_initializer(&self.backend.context.i64_type().const_array(&bitmap_values));
        bitmap.set_constant(true);
        bitmap.set_linkage(Linkage::Private);
        bitmap.set_unnamed_address(UnnamedAddress::Global);

        let descriptor_type = self.backend.context.struct_type(
            &[
                self.backend.context.i32_type().into(),
                self.backend.context.i32_type().into(),
                self.backend.context.i64_type().into(),
                self.backend.context.i64_type().into(),
                self.backend.context.i64_type().into(),
                self.backend.ptr_type.into(),
            ],
            false,
        );
        let descriptor = self.backend.module.add_global(
            descriptor_type,
            None,
            &self.backend.unique("managed.root.descriptor"),
        );
        let slot_count = u64::try_from(slot_count)
            .map_err(|_| CodegenError::new("ProgramTooLarge", "too many managed roots"))?;
        let state_count = u64::try_from(self.root_plan.state_count())
            .map_err(|_| CodegenError::new("ProgramTooLarge", "too many managed-root states"))?;
        let bitmap_words = u64::try_from(self.root_plan.bitmap_words()).map_err(|_| {
            CodegenError::new("ProgramTooLarge", "managed-root bitmap row is too wide")
        })?;
        descriptor.set_initializer(
            &descriptor_type.const_named_struct(&[
                self.backend
                    .context
                    .i32_type()
                    .const_int(u64::from(TYPED_SHADOW_STACK_ABI_VERSION), false)
                    .into(),
                self.backend.context.i32_type().const_zero().into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(slot_count, false)
                    .into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(state_count, false)
                    .into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(bitmap_words, false)
                    .into(),
                bitmap.as_pointer_value().into(),
            ]),
        );
        descriptor.set_constant(true);
        descriptor.set_linkage(Linkage::Private);
        descriptor.set_unnamed_address(UnnamedAddress::Global);
        Ok(descriptor.as_pointer_value())
    }

    fn link_root_frame(
        &mut self,
        descriptor: PointerValue<'ctx>,
        slot_array: PointerValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let frame_type = self.backend.context.struct_type(
            &[
                self.backend.context.i32_type().into(),
                self.backend.context.i32_type().into(),
                self.backend.context.i64_type().into(),
                self.backend.ptr_type.into(),
                self.backend.ptr_type.into(),
                self.backend.ptr_type.into(),
            ],
            false,
        );
        let frame = self
            .backend
            .builder
            .build_alloca(frame_type, "managed.root.frame")
            .map_err(builder_error)?;
        let fields: [BasicValueEnum<'ctx>; 6] = [
            self.backend
                .context
                .i32_type()
                .const_int(u64::from(TYPED_SHADOW_STACK_ABI_VERSION), false)
                .into(),
            self.backend.context.i32_type().const_zero().into(),
            self.backend.context.i64_type().const_zero().into(),
            descriptor.into(),
            slot_array.into(),
            self.backend.ptr_type.const_null().into(),
        ];
        for (index, value) in fields.into_iter().enumerate() {
            let index = u32::try_from(index).expect("six root-frame fields fit u32");
            let field = self
                .backend
                .builder
                .build_struct_gep(frame_type, frame, index, "managed.root.frame.field")
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(field, value)
                .map_err(builder_error)?;
            if index == 2 {
                self.root_state = Some(field);
            }
        }
        self.root_frame = Some(frame);
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_root_push(),
            &[frame.into()],
            "managed.root.push",
        )?;
        self.backend
            .require_zero_status(status, "managed.root.push")?;
        Ok(())
    }

    fn allocation_block(&self, entry: BlockId) -> Result<BasicBlock<'ctx>, CodegenError> {
        if let Some(coroutine) = &self.coroutine {
            Ok(coroutine.prologue)
        } else {
            self.block(entry)
        }
    }

    fn publish_block_parameters(&self, block: BlockId) -> Result<(), CodegenError> {
        let block = self
            .source
            .block(block)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "managed-root block disappeared"))?;
        for value in block.params().iter().copied() {
            if self
                .root_slot_ranges
                .get(value.index())
                .copied()
                .flatten()
                .is_none()
            {
                continue;
            }
            let raw = self.values[value.index()].ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("managed block parameter {value} has no LLVM value"),
                )
            })?;
            self.publish_root_value(value, raw)?;
        }
        Ok(())
    }

    fn publish_root_value(
        &self,
        value: ValueId,
        raw: BasicValueEnum<'ctx>,
    ) -> Result<(), CodegenError> {
        let Some((start, end)) = self.root_slot_ranges.get(value.index()).copied().flatten() else {
            return Ok(());
        };
        for index in start..end {
            let slot = self.root_plan.slots().get(index).ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "managed-root slot disappeared")
            })?;
            if slot.value() != value {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    format!("managed-root range for {value} contains {}", slot.value()),
                ));
            }
            let ty = self
                .source
                .value(value)
                .ok_or_else(|| {
                    CodegenError::new(
                        "LlvmAbiDefect",
                        format!("managed-root value {value} disappeared"),
                    )
                })?
                .ty();
            let (pointer, active) = self.project_managed_root(raw, ty, slot.projection())?;
            let pointer = if let Some(active) = active {
                self.backend
                    .builder
                    .build_select(
                        active,
                        pointer,
                        self.backend.ptr_type.const_null(),
                        "managed.root.active.pointer",
                    )
                    .map_err(builder_error)?
                    .into_pointer_value()
            } else {
                pointer
            };
            let cell = self
                .root_cells
                .get(index)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "managed-root cell is missing")
                })?;
            self.backend
                .builder
                .build_store(cell, pointer)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "typed product and sum projection keep tag guards and physical decoding in one auditable traversal"
    )]
    fn project_managed_root(
        &self,
        mut value: BasicValueEnum<'ctx>,
        mut ty: ValueTypeId,
        projection: &[ManagedRootProjection],
    ) -> Result<(PointerValue<'ctx>, Option<IntValue<'ctx>>), CodegenError> {
        let representations = self.backend.artifact.representations();
        let mut active = None;
        for step in projection {
            let value_type = representations.value_type(ty).ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}"))
            })?;
            match step {
                ManagedRootProjection::ProductField(field) => {
                    let Repr::Product(product) = representations
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
                            format!(
                                "managed-root projection {projection:?} uses a product step on {ty}"
                            ),
                        ));
                    };
                    let product = representations.product(product).ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("missing product representation for {ty}"),
                        )
                    })?;
                    ty = product
                        .fields()
                        .get(usize::try_from(*field).map_err(|_| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "managed product field is too wide",
                            )
                        })?)
                        .copied()
                        .ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                format!("managed product projection field {field} is out of range"),
                            )
                        })?;
                    value = self
                        .backend
                        .builder
                        .build_extract_value(
                            value.into_struct_value(),
                            *field,
                            "managed.root.product.extract",
                        )
                        .map_err(builder_error)?;
                }
                ManagedRootProjection::SumVariantField { variant, field } => {
                    let sum_ty = ty;
                    let sum = self.backend.sum_repr(ty)?;
                    let variant_index = usize::try_from(*variant).map_err(|_| {
                        CodegenError::new("ProgramTooLarge", "managed sum variant is too wide")
                    })?;
                    ty = sum
                        .variants()
                        .get(variant_index)
                        .and_then(|payload| {
                            usize::try_from(*field)
                                .ok()
                                .and_then(|field| payload.fields().get(field))
                        })
                        .copied()
                        .ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                format!(
                                    "managed sum projection variant {variant} field {field} is out of range"
                                ),
                            )
                        })?;
                    let layout = self.backend.sum_layout(sum_ty)?;
                    let payload_type =
                        layout.payloads.get(variant_index).copied().ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                format!("managed sum projection variant {variant} disappeared"),
                            )
                        })?;
                    let payload = match layout.tag {
                        SumTagRepr::Tagless => {
                            if *variant != 0 {
                                return Err(CodegenError::new(
                                    "LlvmAbiDefect",
                                    "tagless managed sum projection names a nonzero variant",
                                ));
                            }
                            value.into_struct_value()
                        }
                        SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                            let aggregate = value.into_struct_value();
                            let tag = self
                                .backend
                                .builder
                                .build_extract_value(aggregate, 0, "managed.root.sum.tag")
                                .map_err(builder_error)?
                                .into_int_value();
                            let variant_active = self
                                .backend
                                .builder
                                .build_int_compare(
                                    IntPredicate::EQ,
                                    tag,
                                    tag.get_type().const_int(u64::from(*variant), false),
                                    "managed.root.sum.variant.active",
                                )
                                .map_err(builder_error)?;
                            let combined = if let Some(parent_active) = active {
                                self.backend
                                    .builder
                                    .build_and(
                                        parent_active,
                                        variant_active,
                                        "managed.root.sum.path.active",
                                    )
                                    .map_err(builder_error)?
                            } else {
                                variant_active
                            };
                            active = Some(combined);
                            let carrier_type = layout.carrier.ok_or_else(|| {
                                CodegenError::new(
                                    "LlvmAbiDefect",
                                    "managed sum projection has no payload carrier",
                                )
                            })?;
                            let carrier = self
                                .backend
                                .builder
                                .build_extract_value(aggregate, 1, "managed.root.sum.carrier")
                                .map_err(builder_error)?;
                            // Never decode arbitrary carrier bits for an inactive or
                            // malformed tag. Inactive candidates unpack an all-zero
                            // carrier and are published as null below.
                            let safe_carrier = self
                                .backend
                                .builder
                                .build_select(
                                    combined,
                                    carrier,
                                    carrier_type.const_zero().into(),
                                    "managed.root.sum.safe.carrier",
                                )
                                .map_err(builder_error)?;
                            self.unpack_sum_carrier(
                                safe_carrier,
                                carrier_type,
                                payload_type,
                                layout.payload_byte_offset(variant_index)?,
                            )?
                        }
                    };
                    value = self
                        .backend
                        .builder
                        .build_extract_value(payload, *field, "managed.root.sum.field")
                        .map_err(builder_error)?;
                }
            }
        }
        let value_type = representations
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        if !matches!(
            representations.repr(value_type.repr()),
            Some(Repr::ManagedPointer)
        ) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("managed-root projection {projection:?} does not end at a managed pointer"),
            ));
        }
        let BasicValueEnum::PointerValue(pointer) = value else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("managed-root projection {projection:?} is not an LLVM pointer"),
            ));
        };
        Ok((pointer, active))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "recursive aggregate reconstruction keeps active-variant selection beside each typed projection case"
    )]
    fn rebuild_projected_value(
        &self,
        aggregate: BasicValueEnum<'ctx>,
        ty: ValueTypeId,
        projection: &[ManagedRootProjection],
        replacement: PointerValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let representations = self.backend.artifact.representations();
        let value_type = representations
            .value_type(ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing LCIR type {ty}")))?;
        if projection.is_empty() {
            if !matches!(
                representations.repr(value_type.repr()),
                Some(Repr::ManagedPointer)
            ) || !matches!(aggregate, BasicValueEnum::PointerValue(_))
            {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "empty managed-root projection targets a non-pointer",
                ));
            }
            return Ok(replacement.into());
        }
        let (step, remaining) = projection.split_first().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "managed-root projection disappeared")
        })?;
        match step {
            ManagedRootProjection::ProductField(field) => {
                let Repr::Product(product) = representations
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
                        format!(
                            "managed-root projection {projection:?} uses a product step on {ty}"
                        ),
                    ));
                };
                let child_ty = representations
                    .product(product)
                    .and_then(|product| {
                        usize::try_from(*field)
                            .ok()
                            .and_then(|field| product.fields().get(field))
                    })
                    .copied()
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("managed product projection field {field} is out of range"),
                        )
                    })?;
                let parent = aggregate.into_struct_value();
                let child = self
                    .backend
                    .builder
                    .build_extract_value(parent, *field, "managed.root.rebuild.product.extract")
                    .map_err(builder_error)?;
                let rebuilt =
                    self.rebuild_projected_value(child, child_ty, remaining, replacement)?;
                Ok(self
                    .backend
                    .builder
                    .build_insert_value(parent, rebuilt, *field, "managed.root.rebuild.product")
                    .map_err(builder_error)?
                    .into_struct_value()
                    .into())
            }
            ManagedRootProjection::SumVariantField { variant, field } => {
                let sum = self.backend.sum_repr(ty)?;
                let variant_index = usize::try_from(*variant).map_err(|_| {
                    CodegenError::new("ProgramTooLarge", "managed sum variant is too wide")
                })?;
                let child_ty = sum
                    .variants()
                    .get(variant_index)
                    .and_then(|payload| {
                        usize::try_from(*field)
                            .ok()
                            .and_then(|field| payload.fields().get(field))
                    })
                    .copied()
                    .ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!(
                                "managed sum projection variant {variant} field {field} is out of range"
                            ),
                        )
                    })?;
                let layout = self.backend.sum_layout(ty)?;
                let payload_type =
                    layout.payloads.get(variant_index).copied().ok_or_else(|| {
                        CodegenError::new(
                            "LlvmAbiDefect",
                            format!("managed sum projection variant {variant} disappeared"),
                        )
                    })?;
                match layout.tag {
                    SumTagRepr::Tagless => {
                        if *variant != 0 {
                            return Err(CodegenError::new(
                                "LlvmAbiDefect",
                                "tagless managed sum projection names a nonzero variant",
                            ));
                        }
                        let payload = aggregate.into_struct_value();
                        let child = self
                            .backend
                            .builder
                            .build_extract_value(payload, *field, "managed.root.rebuild.sum.field")
                            .map_err(builder_error)?;
                        let rebuilt =
                            self.rebuild_projected_value(child, child_ty, remaining, replacement)?;
                        Ok(self
                            .backend
                            .builder
                            .build_insert_value(
                                payload,
                                rebuilt,
                                *field,
                                "managed.root.rebuild.tagless",
                            )
                            .map_err(builder_error)?
                            .into_struct_value()
                            .into())
                    }
                    SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                        let physical = aggregate.into_struct_value();
                        let tag = self
                            .backend
                            .builder
                            .build_extract_value(physical, 0, "managed.root.rebuild.sum.tag")
                            .map_err(builder_error)?
                            .into_int_value();
                        let active = self
                            .backend
                            .builder
                            .build_int_compare(
                                IntPredicate::EQ,
                                tag,
                                tag.get_type().const_int(u64::from(*variant), false),
                                "managed.root.rebuild.sum.active",
                            )
                            .map_err(builder_error)?;
                        let carrier_type = layout.carrier.ok_or_else(|| {
                            CodegenError::new(
                                "LlvmAbiDefect",
                                "managed sum projection has no payload carrier",
                            )
                        })?;
                        let carrier = self
                            .backend
                            .builder
                            .build_extract_value(physical, 1, "managed.root.rebuild.sum.carrier")
                            .map_err(builder_error)?;
                        let safe_carrier = self
                            .backend
                            .builder
                            .build_select(
                                active,
                                carrier,
                                carrier_type.const_zero().into(),
                                "managed.root.rebuild.sum.safe.carrier",
                            )
                            .map_err(builder_error)?;
                        let variant_index = usize::try_from(*variant).map_err(|_| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "managed sum rebuild variant is too wide",
                            )
                        })?;
                        let payload = self.unpack_sum_carrier(
                            safe_carrier,
                            carrier_type,
                            payload_type,
                            layout.payload_byte_offset(variant_index)?,
                        )?;
                        let child = self
                            .backend
                            .builder
                            .build_extract_value(payload, *field, "managed.root.rebuild.sum.field")
                            .map_err(builder_error)?;
                        let child =
                            self.rebuild_projected_value(child, child_ty, remaining, replacement)?;
                        let payload = self
                            .backend
                            .builder
                            .build_insert_value(
                                payload,
                                child,
                                *field,
                                "managed.root.rebuild.sum.payload",
                            )
                            .map_err(builder_error)?
                            .into_struct_value();
                        let carrier = self.pack_sum_carrier(
                            payload,
                            payload_type,
                            carrier_type,
                            layout.payload_byte_offset(variant_index)?,
                        )?;
                        let rebuilt = self
                            .backend
                            .builder
                            .build_insert_value(physical, carrier, 1, "managed.root.rebuild.sum")
                            .map_err(builder_error)?
                            .into_struct_value();
                        self.backend
                            .builder
                            .build_select(
                                active,
                                rebuilt,
                                physical,
                                "managed.root.rebuild.active.sum",
                            )
                            .map_err(builder_error)
                    }
                }
            }
        }
    }

    fn managed_projection_name(projection: &[ManagedRootProjection]) -> String {
        let mut previous_product = false;
        projection
            .iter()
            .map(|step| {
                let name = match step {
                    ManagedRootProjection::ProductField(field) if previous_product => {
                        field.to_string()
                    }
                    ManagedRootProjection::ProductField(field) => format!("p{field}"),
                    ManagedRootProjection::SumVariantField { variant, field } => {
                        format!("s{variant}f{field}")
                    }
                };
                previous_product = matches!(step, ManagedRootProjection::ProductField(_));
                name
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    fn direct_root_cell(&self, value: ValueId) -> Result<Option<PointerValue<'ctx>>, CodegenError> {
        let Some((start, end)) = self.root_slot_ranges.get(value.index()).copied().flatten() else {
            return Ok(None);
        };
        let slot =
            self.root_plan.slots().get(start).ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "managed-root slot disappeared")
            })?;
        if end != start.saturating_add(1) || slot.value() != value || !slot.projection().is_empty()
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("direct managed value {value} has a non-direct root-slot range"),
            ));
        }
        self.root_cells
            .get(start)
            .copied()
            .flatten()
            .map(Some)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "managed-root cell is missing"))
    }

    fn publish_root_state(&self, site: ManagedSafepoint) -> Result<(), CodegenError> {
        let Some(pointer) = self.root_state else {
            return Ok(());
        };
        let state = self.root_plan.state(site).ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("collecting LCIR site {site:?} has no root state"),
            )
        })?;
        self.backend
            .builder
            .build_store(
                pointer,
                self.backend.context.i64_type().const_int(state, false),
            )
            .map_err(builder_error)?;
        Ok(())
    }

    fn pop_root_frame(&self) -> Result<(), CodegenError> {
        let Some(frame) = self.root_frame else {
            return Ok(());
        };
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_root_pop(),
            &[frame.into()],
            "managed.root.pop",
        )?;
        self.backend.require_zero_status(status, "managed.root.pop")
    }

    fn record_value(
        &mut self,
        id: ValueId,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), CodegenError> {
        self.values[id.index()] = Some(value);
        self.publish_root_value(id, value)?;
        Ok(())
    }

    fn compile(mut self) -> Result<(), CodegenError> {
        self.prepare_resource_close_token_cells()?;
        self.prepare_managed_output_cells()?;
        self.prepare_json_input_cells()?;
        self.prepare_float_parse_output_cells()?;
        self.prepare_root_frame()?;
        self.finish_coroutine_prologue()?;
        self.emit_coroutine_resume_blocks()?;
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
            self.publish_block_parameters(block_id)?;
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
            self.emit_terminator(block.id(), terminator)?;
        }
        if self.coroutine.is_none()
            && let Some(debug) = &self.backend.debug
        {
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
        if self.coroutine.is_none()
            && let Some(debug) = &self.backend.debug
        {
            debug.set_location(
                self.backend.context,
                &self.backend.builder,
                self.function,
                origin,
            )?;
        }
        Ok(())
    }

    fn finish_coroutine_prologue(&self) -> Result<(), CodegenError> {
        let Some(coroutine) = &self.coroutine else {
            return Ok(());
        };
        if self.root_frame.is_none() {
            self.backend.builder.position_at_end(coroutine.prologue);
        }
        let requested = call_int(
            &self.backend.builder,
            self.backend.typed_task_is_cancel_requested(),
            &[coroutine.task.into()],
            "coroutine.cancel.requested",
        )?;
        let cancelling = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::NE,
                requested,
                self.backend.context.i32_type().const_zero(),
                "coroutine.cancelling",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(cancelling, coroutine.cancel_dispatch, coroutine.dispatch)
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_coroutine_resume_blocks(&self) -> Result<(), CodegenError> {
        let Some(coroutine) = &self.coroutine else {
            return Ok(());
        };
        let plan = self.source.coroutine().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "coroutine callback has no checked plan")
        })?;
        if plan.suspensions().len() != coroutine.layout.suspensions.len()
            || plan.suspensions().len() != coroutine.resume_blocks.len()
            || plan.suspensions().len() != coroutine.cancel_blocks.len()
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "coroutine plan, frame layout, resume dispatch, and cancellation dispatch disagree",
            ));
        }

        for (((plan_row, layout_row), resume), cancel_resume) in plan
            .suspensions()
            .iter()
            .zip(&coroutine.layout.suspensions)
            .zip(&coroutine.resume_blocks)
            .zip(&coroutine.cancel_blocks)
        {
            let targets = self.await_for_state(plan_row.state())?;
            if layout_row.state != plan_row.state()
                || layout_row.child_fields.len() != plan_row.awaited().len()
                || layout_row.live_fields.len() != plan_row.live().len()
                || targets.mode != plan_row.mode()
            {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    format!(
                        "coroutine state {} disagrees with its frame layout",
                        plan_row.state()
                    ),
                ));
            }
            self.backend.builder.position_at_end(*resume);
            self.emit_coroutine_resume_state(
                plan_row,
                layout_row,
                &targets.normal,
                &targets.fault,
                &targets.cancel,
                targets.origin,
            )?;

            self.backend.builder.position_at_end(*cancel_resume);
            self.emit_coroutine_live_branch(
                plan_row,
                layout_row,
                targets.cancel.block,
                "coroutine.cancel.live",
            )?;
        }

        self.backend.builder.position_at_end(coroutine.cancel_start);
        self.emit_coroutine_step_return(TASK_CANCELLED)?;
        self.backend
            .builder
            .position_at_end(coroutine.invalid_state);
        self.emit_coroutine_step_return(TASK_FAULTED)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one shared resume edge keeps scheduled and immediate-ready await completion on the same checked frame/result path"
    )]
    fn emit_coroutine_resume_state(
        &self,
        plan_row: &CoroutineSuspension,
        layout_row: &CoroutineSuspensionLayout,
        normal: &ResultTarget,
        fault: &UnwindTarget,
        cancel: &BlockTarget,
        origin: Origin,
    ) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "coroutine resume has no active frame")
        })?;
        if layout_row.state != plan_row.state()
            || layout_row.child_fields.len() != plan_row.awaited().len()
            || layout_row.live_fields.len() != plan_row.live().len()
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "coroutine state {} disagrees with its frame layout",
                    plan_row.state()
                ),
            ));
        }
        let step = call_int(
            &self.backend.builder,
            self.backend.task_join_step(),
            &[coroutine.task.into()],
            "task.await.step",
        )?;
        let completed = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.completed");
        let faulted = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.faulted");
        let cancelled = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.cancelled");
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.invalid_step");
        self.backend
            .builder
            .build_switch(
                step,
                invalid,
                &[
                    (
                        self.backend
                            .context
                            .i32_type()
                            .const_int(TASK_COMPLETED as u64, false),
                        completed,
                    ),
                    (
                        self.backend
                            .context
                            .i32_type()
                            .const_int(TASK_FAULTED as u64, false),
                        faulted,
                    ),
                    (
                        self.backend
                            .context
                            .i32_type()
                            .const_int(TASK_CANCELLED as u64, false),
                        cancelled,
                    ),
                ],
            )
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(faulted);
        match plan_row.mode() {
            AwaitMode::All | AwaitMode::Settled | AwaitMode::Race => {
                self.activate_coroutine_fault()?;
            }
            AwaitMode::Any => self.activate_coroutine_any_fault(origin)?,
        }
        self.emit_coroutine_live_branch(
            plan_row,
            layout_row,
            fault.block,
            "task.await.fault.live",
        )?;
        self.backend.builder.position_at_end(cancelled);
        self.emit_coroutine_live_branch(
            plan_row,
            layout_row,
            cancel.block,
            "task.await.cancel.live",
        )?;
        self.backend.builder.position_at_end(invalid);
        self.emit_coroutine_step_return(TASK_FAULTED)?;

        self.backend.builder.position_at_end(completed);
        match plan_row.mode() {
            AwaitMode::Any => {
                return self.emit_coroutine_any_result(plan_row, layout_row, normal);
            }
            AwaitMode::Race => {
                return self.emit_coroutine_race_handle(plan_row, layout_row, normal);
            }
            AwaitMode::Settled => {
                return self.emit_coroutine_settled_handles(plan_row, layout_row, normal);
            }
            AwaitMode::All => {}
        }
        let mut values = Vec::with_capacity(
            layout_row
                .child_fields
                .len()
                .saturating_add(layout_row.live_fields.len()),
        );
        for ((field, output), index) in layout_row
            .child_fields
            .iter()
            .copied()
            .zip(plan_row.awaited().iter().copied())
            .zip(0_u32..)
        {
            let child_pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    &format!("task.await.child.{index}.pointer"),
                )
                .map_err(builder_error)?;
            let child = self
                .backend
                .builder
                .build_load(
                    self.backend.ptr_type,
                    child_pointer,
                    &format!("task.await.child.{index}"),
                )
                .map_err(builder_error)?
                .into_pointer_value();
            values.push(self.take_typed_task_result(
                child,
                output,
                &format!("task.await.result.{index}"),
            )?);
        }
        values.extend(self.load_coroutine_live_values(plan_row, layout_row, "task.await.live")?);
        let predecessor = self.current_block()?;
        self.add_basic_incoming(normal.block, &values, predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(normal.block)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_coroutine_any_result(
        &self,
        plan_row: &CoroutineSuspension,
        layout_row: &CoroutineSuspensionLayout,
        normal: &ResultTarget,
    ) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Task.any resume has no active frame")
        })?;
        if plan_row.mode() != AwaitMode::Any
            || plan_row.awaited().is_empty()
            || layout_row.child_fields.len() != plan_row.awaited().len()
            || plan_row
                .awaited()
                .iter()
                .any(|output| output != &plan_row.awaited()[0])
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "Task.any state {} has an invalid checked child row",
                    plan_row.state()
                ),
            ));
        }
        let winner = call_int(
            &self.backend.builder,
            self.backend.task_join_winner(),
            &[coroutine.task.into()],
            "task.await.any.winner",
        )?;
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.any.invalid_winner");
        let cases = layout_row
            .child_fields
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let index = u64::try_from(index).map_err(|_| {
                    CodegenError::new("ProgramTooLarge", "Task.any has too many fixed children")
                })?;
                Ok((
                    self.backend.context.i64_type().const_int(index, false),
                    self.backend
                        .context
                        .append_basic_block(self.function, &format!("task.await.any.{index}")),
                ))
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        self.backend
            .builder
            .build_switch(winner, invalid, &cases)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(invalid);
        self.emit_coroutine_step_return(TASK_FAULTED)?;

        for ((field, output), (_, case)) in layout_row
            .child_fields
            .iter()
            .copied()
            .zip(plan_row.awaited().iter().copied())
            .zip(cases)
        {
            self.backend.builder.position_at_end(case);
            let child_pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    "task.await.any.child.pointer",
                )
                .map_err(builder_error)?;
            let child = self
                .backend
                .builder
                .build_load(self.backend.ptr_type, child_pointer, "task.await.any.child")
                .map_err(builder_error)?
                .into_pointer_value();
            let mut values = Vec::with_capacity(1 + layout_row.live_fields.len());
            values.push(self.take_typed_task_result(child, output, "task.await.any.result")?);
            values.extend(self.load_coroutine_live_values(
                plan_row,
                layout_row,
                "task.await.any.live",
            )?);
            let predecessor = self.current_block()?;
            self.add_basic_incoming(normal.block, &values, predecessor)?;
            self.backend
                .builder
                .build_unconditional_branch(self.block(normal.block)?)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    fn emit_coroutine_settled_handles(
        &self,
        plan_row: &CoroutineSuspension,
        layout_row: &CoroutineSuspensionLayout,
        normal: &ResultTarget,
    ) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Task.settled resume has no active frame")
        })?;
        if plan_row.mode() != AwaitMode::Settled
            || plan_row.awaited().is_empty()
            || layout_row.child_fields.len() != plan_row.awaited().len()
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "Task.settled state {} has an invalid checked child row",
                    plan_row.state()
                ),
            ));
        }
        let mut values = Vec::with_capacity(
            layout_row
                .child_fields
                .len()
                .saturating_add(layout_row.live_fields.len()),
        );
        for (field, index) in layout_row.child_fields.iter().copied().zip(0_u32..) {
            let child_pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    &format!("task.await.settled.child.{index}.pointer"),
                )
                .map_err(builder_error)?;
            values.push(
                self.backend
                    .builder
                    .build_load(
                        self.backend.ptr_type,
                        child_pointer,
                        &format!("task.await.settled.child.{index}"),
                    )
                    .map_err(builder_error)?,
            );
        }
        values.extend(self.load_coroutine_live_values(
            plan_row,
            layout_row,
            "task.await.settled.live",
        )?);
        let predecessor = self.current_block()?;
        self.add_basic_incoming(normal.block, &values, predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(normal.block)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_coroutine_race_handle(
        &self,
        plan_row: &CoroutineSuspension,
        layout_row: &CoroutineSuspensionLayout,
        normal: &ResultTarget,
    ) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Task.race resume has no active frame")
        })?;
        if plan_row.mode() != AwaitMode::Race
            || plan_row.awaited().is_empty()
            || layout_row.child_fields.len() != plan_row.awaited().len()
            || plan_row
                .awaited()
                .iter()
                .any(|output| output != &plan_row.awaited()[0])
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "Task.race state {} has an invalid checked child row",
                    plan_row.state()
                ),
            ));
        }
        let winner = call_int(
            &self.backend.builder,
            self.backend.task_join_winner(),
            &[coroutine.task.into()],
            "task.await.race.winner",
        )?;
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.race.invalid_winner");
        let cases = layout_row
            .child_fields
            .iter()
            .enumerate()
            .map(|(index, _)| {
                let index = u64::try_from(index).map_err(|_| {
                    CodegenError::new("ProgramTooLarge", "Task.race has too many fixed children")
                })?;
                Ok((
                    self.backend.context.i64_type().const_int(index, false),
                    self.backend
                        .context
                        .append_basic_block(self.function, &format!("task.await.race.{index}")),
                ))
            })
            .collect::<Result<Vec<_>, CodegenError>>()?;
        self.backend
            .builder
            .build_switch(winner, invalid, &cases)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(invalid);
        self.emit_coroutine_step_return(TASK_FAULTED)?;

        for (field, (_, case)) in layout_row.child_fields.iter().copied().zip(cases) {
            self.backend.builder.position_at_end(case);
            let child_pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    "task.await.race.child.pointer",
                )
                .map_err(builder_error)?;
            let child = self
                .backend
                .builder
                .build_load(
                    self.backend.ptr_type,
                    child_pointer,
                    "task.await.race.child",
                )
                .map_err(builder_error)?;
            let mut values = Vec::with_capacity(1 + layout_row.live_fields.len());
            values.push(child);
            values.extend(self.load_coroutine_live_values(
                plan_row,
                layout_row,
                "task.await.race.live",
            )?);
            let predecessor = self.current_block()?;
            self.add_basic_incoming(normal.block, &values, predecessor)?;
            self.backend
                .builder
                .build_unconditional_branch(self.block(normal.block)?)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    fn activate_coroutine_fault(&self) -> Result<(), CodegenError> {
        let context = self.fault_context.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "awaited child fault has no coroutine fault context",
            )
        })?;
        let active = self
            .backend
            .builder
            .build_struct_gep(
                self.backend.fault_context_type,
                context,
                1,
                "task.await.fault.active.pointer",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(active, self.backend.context.bool_type().const_int(1, false))
            .map_err(builder_error)?;
        Ok(())
    }

    fn activate_coroutine_any_fault(&self, origin: Origin) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Task.any fault has no active coroutine")
        })?;
        let winner = call_int(
            &self.backend.builder,
            self.backend.task_join_winner(),
            &[coroutine.task.into()],
            "task.await.any.fault.winner",
        )?;
        let all_failed = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                winner,
                self.backend.context.i64_type().const_all_ones(),
                "task.await.any.all_failed",
            )
            .map_err(builder_error)?;
        let report = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.any.report_failed");
        let inherit = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.any.inherit_cleanup_fault");
        let continuation = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.any.fault_ready");
        self.backend
            .builder
            .build_conditional_branch(all_failed, report, inherit)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(report);
        self.emit_source_fault(FaultCode::TaskAnyFailed, origin)?;
        self.backend
            .builder
            .build_unconditional_branch(continuation)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(inherit);
        self.activate_coroutine_fault()?;
        self.backend
            .builder
            .build_unconditional_branch(continuation)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(continuation);
        Ok(())
    }

    fn load_coroutine_live_values(
        &self,
        plan_row: &CoroutineSuspension,
        layout_row: &CoroutineSuspensionLayout,
        prefix: &str,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "coroutine live reload has no active frame")
        })?;
        if layout_row.live_fields.len() != plan_row.live().len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "coroutine state {} live row disagrees with its frame layout",
                    plan_row.state()
                ),
            ));
        }
        plan_row
            .live()
            .iter()
            .copied()
            .zip(layout_row.live_fields.iter().copied())
            .zip(0_u32..)
            .map(|((ty, field), index)| {
                let pointer = self
                    .backend
                    .builder
                    .build_struct_gep(
                        coroutine.layout.frame,
                        coroutine.frame,
                        field,
                        &format!("{prefix}.{index}.pointer"),
                    )
                    .map_err(builder_error)?;
                self.backend
                    .builder
                    .build_load(
                        self.backend.llvm_type(ty)?,
                        pointer,
                        &format!("{prefix}.{index}"),
                    )
                    .map_err(builder_error)
            })
            .collect()
    }

    fn emit_coroutine_live_branch(
        &self,
        plan_row: &CoroutineSuspension,
        layout_row: &CoroutineSuspensionLayout,
        target: BlockId,
        prefix: &str,
    ) -> Result<(), CodegenError> {
        let values = self.load_coroutine_live_values(plan_row, layout_row, prefix)?;
        let predecessor = self.current_block()?;
        self.add_basic_incoming(target, &values, predecessor)?;
        self.backend
            .builder
            .build_unconditional_branch(self.block(target)?)
            .map_err(builder_error)?;
        Ok(())
    }

    fn await_for_state(&self, state: u32) -> Result<AwaitExitTargets, CodegenError> {
        self.source
            .blocks()
            .iter()
            .find_map(|block| {
                let terminator = block.terminator()?;
                match terminator.kind() {
                    TerminatorKind::AwaitTasks {
                        state: candidate,
                        mode,
                        normal,
                        fault,
                        cancel,
                        ..
                    } if *candidate == state => Some(AwaitExitTargets {
                        mode: *mode,
                        origin: terminator.origin(),
                        normal: normal.clone(),
                        fault: fault.clone(),
                        cancel: cancel.clone(),
                    }),
                    _ => None,
                }
            })
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("coroutine resume state {state} has no await terminator"),
                )
            })
    }

    fn take_typed_task_result(
        &self,
        child: PointerValue<'ctx>,
        output: ValueTypeId,
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let physical = self.backend.llvm_type(output)?;
        let size = self.backend.target_data.get_abi_size(&physical);
        let alignment = u64::from(self.backend.target_data.get_abi_alignment(&physical));
        let storage = (size != 0)
            .then(|| {
                self.backend
                    .builder
                    .build_alloca(physical, &format!("{name}.storage"))
                    .map_err(builder_error)
            })
            .transpose()?;
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_task_take_result(),
            &[
                child.into(),
                storage
                    .unwrap_or_else(|| self.backend.ptr_type.const_null())
                    .into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(size, false)
                    .into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(alignment, false)
                    .into(),
            ],
            &format!("{name}.take"),
        )?;
        self.backend
            .require_zero_status(status, &format!("{name}.take"))?;
        if let Some(storage) = storage {
            self.backend
                .builder
                .build_load(physical, storage, &format!("{name}.value"))
                .map_err(builder_error)
        } else {
            self.backend.zero(output)
        }
    }

    /// Consumes one terminal typed child and constructs the exact closed
    /// `TaskOutcome[T]` selected by its terminal state. The runtime owns the
    /// only allocating part of this boundary: copying a fault's code and
    /// message into two managed Text leaves while rooting the first across the
    /// second allocation. The resulting sum is ordinary LCIR SSA and is
    /// published into the function's precise root cells before any later
    /// collecting instruction executes.
    #[expect(
        clippy::too_many_lines,
        reason = "the checked outcome ABI is emitted as one auditable terminal-state switch"
    )]
    fn emit_task_outcome_take(
        &self,
        instruction: &Instruction,
        task: ValueId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let task_ty = self
            .source
            .value(task)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "TaskOutcome.take task is missing"))?
            .ty();
        let representations = self.backend.artifact.representations();
        let output = representations
            .value_type(task_ty)
            .and_then(|task| match task.semantic() {
                Type::Task(output) => representations.type_id(output),
                _ => None,
            })
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "TaskOutcome.take operand is not a canonical typed Task handle",
                )
            })?;
        let outcome = instruction
            .results()
            .first()
            .and_then(|result| self.source.value(*result))
            .map(loom_codegen_ir::Value::ty)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "TaskOutcome.take result is missing")
            })?;
        let fault = self
            .backend
            .sum_repr(outcome)?
            .variants()
            .get(usize::try_from(TASK_OUTCOME_FAULTED_VARIANT).map_err(|_| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "TaskOutcome Faulted variant overflows usize",
                )
            })?)
            .and_then(|variant| variant.fields().first())
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    "TaskOutcome.take result has no canonical Faulted payload",
                )
            })?;

        let output_physical = self.backend.llvm_type(output)?;
        let output_size = self.backend.target_data.get_abi_size(&output_physical);
        let output_alignment =
            u64::from(self.backend.target_data.get_abi_alignment(&output_physical));
        let output_storage = (output_size != 0)
            .then(|| {
                self.backend
                    .builder
                    .build_alloca(output_physical, "task.outcome.value.storage")
                    .map_err(builder_error)
            })
            .transpose()?;
        let code_output = self
            .backend
            .builder
            .build_alloca(self.backend.ptr_type, "task.outcome.code.output")
            .map_err(builder_error)?;
        let message_output = self
            .backend
            .builder
            .build_alloca(self.backend.ptr_type, "task.outcome.message.output")
            .map_err(builder_error)?;
        for cell in [code_output, message_output] {
            self.backend
                .builder
                .build_store(cell, self.backend.ptr_type.const_null())
                .map_err(builder_error)?;
        }

        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_task_take_outcome(),
            &[
                self.value(task)?.into_pointer_value().into(),
                output_storage
                    .unwrap_or_else(|| self.backend.ptr_type.const_null())
                    .into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(output_size, false)
                    .into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(output_alignment, false)
                    .into(),
                code_output.into(),
                message_output.into(),
            ],
            "task.outcome.take",
        )?;

        let completed = self
            .backend
            .context
            .append_basic_block(self.function, "task.outcome.completed");
        let faulted = self
            .backend
            .context
            .append_basic_block(self.function, "task.outcome.faulted");
        let cancelled = self
            .backend
            .context
            .append_basic_block(self.function, "task.outcome.cancelled");
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "task.outcome.invalid");
        let merge = self
            .backend
            .context
            .append_basic_block(self.function, "task.outcome.merge");
        self.backend
            .builder
            .build_switch(
                status,
                invalid,
                &[
                    (
                        self.backend
                            .context
                            .i32_type()
                            .const_int(TASK_COMPLETED as u64, false),
                        completed,
                    ),
                    (
                        self.backend
                            .context
                            .i32_type()
                            .const_int(TASK_FAULTED as u64, false),
                        faulted,
                    ),
                    (
                        self.backend
                            .context
                            .i32_type()
                            .const_int(TASK_CANCELLED as u64, false),
                        cancelled,
                    ),
                ],
            )
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(invalid);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.backend.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.backend
            .builder
            .build_call(trap, &[], "task.outcome.invalid.trap")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unreachable()
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(completed);
        let completed_payload = if let Some(storage) = output_storage {
            self.backend
                .builder
                .build_load(output_physical, storage, "task.outcome.completed.value")
                .map_err(builder_error)?
        } else {
            self.backend.zero(output)?
        };
        let completed_value = self.emit_sum_construct_values(
            outcome,
            TASK_OUTCOME_COMPLETED_VARIANT,
            &[completed_payload],
        )?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;
        let completed_predecessor = completed;

        self.backend.builder.position_at_end(faulted);
        let code = self
            .backend
            .builder
            .build_load(
                self.backend.ptr_type,
                code_output,
                "task.outcome.fault.code",
            )
            .map_err(builder_error)?;
        let message = self
            .backend
            .builder
            .build_load(
                self.backend.ptr_type,
                message_output,
                "task.outcome.fault.message",
            )
            .map_err(builder_error)?;
        let fault_type = self.backend.llvm_type(fault)?.into_struct_type();
        let fault_value = self
            .backend
            .builder
            .build_insert_value(
                fault_type.get_undef(),
                code,
                0,
                "task.outcome.fault.code.insert",
            )
            .map_err(builder_error)?
            .into_struct_value();
        let fault_value = self
            .backend
            .builder
            .build_insert_value(fault_value, message, 1, "task.outcome.fault.message.insert")
            .map_err(builder_error)?
            .into_struct_value();
        let faulted_value = self.emit_sum_construct_values(
            outcome,
            TASK_OUTCOME_FAULTED_VARIANT,
            &[fault_value.into()],
        )?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;
        let faulted_predecessor = faulted;

        self.backend.builder.position_at_end(cancelled);
        let cancelled_value =
            self.emit_sum_construct_values(outcome, TASK_OUTCOME_CANCELLED_VARIANT, &[])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;
        let cancelled_predecessor = cancelled;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(self.backend.llvm_type(outcome)?, "task.outcome.value")
            .map_err(builder_error)?;
        phi.add_incoming(&[
            (&completed_value, completed_predecessor),
            (&faulted_value, faulted_predecessor),
            (&cancelled_value, cancelled_predecessor),
        ]);
        Ok(phi.as_basic_value())
    }

    fn emit_coroutine_step_return(&self, step: i32) -> Result<(), CodegenError> {
        self.pop_root_frame()?;
        self.backend
            .builder
            .build_return(Some(
                &self
                    .backend
                    .context
                    .i32_type()
                    .const_int(u64::from(step.cast_unsigned()), false),
            ))
            .map_err(builder_error)?;
        Ok(())
    }

    /// Returns a deterministic CFG preorder rooted at the function entry.
    ///
    /// Checked LCIR constrains SSA uses by dominance, not by block-table
    /// insertion order. Every dominator is encountered before the blocks it
    /// dominates in a preorder rooted at entry, so all non-phi operands have an
    /// LLVM definition before they are consumed. The explicit stack also keeps
    /// large generated CFGs off the Rust call stack.
    #[allow(clippy::too_many_lines)]
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
                TerminatorKind::SumSwitch { cases, .. }
                | TerminatorKind::SumBorrowSwitch { cases, .. }
                | TerminatorKind::DynSwitch { cases, .. } => {
                    for case in cases.iter().rev() {
                        pending.push(case.block);
                    }
                }
                TerminatorKind::SumZipSwitch {
                    cases, mismatch, ..
                } => {
                    pending.push(mismatch.block);
                    for case in cases.iter().rev() {
                        pending.push(case.block);
                    }
                }
                TerminatorKind::CheckedIntNegate { normal, fault, .. }
                | TerminatorKind::CheckedIntBinary { normal, fault, .. }
                | TerminatorKind::TaskSleep { normal, fault, .. }
                | TerminatorKind::LogWrite { normal, fault, .. }
                | TerminatorKind::StdoutWrite { normal, fault, .. } => {
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
                TerminatorKind::AwaitTasks {
                    normal,
                    fault,
                    cancel,
                    ..
                } => {
                    pending.push(cancel.block);
                    pending.push(fault.block);
                    pending.push(normal.block);
                }
                TerminatorKind::Return(_)
                | TerminatorKind::Fault { .. }
                | TerminatorKind::ResumeFault
                | TerminatorKind::TaskCancelled => {}
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
            InstructionKind::TextEncodeUtf8 { text } => one(self.value(*text)?),
            InstructionKind::TextFromUtf8Units {
                units,
                ok_variant,
                error_variant,
                invalid_utf8_variant,
            } => one(self.emit_text_from_utf8_units(
                instruction,
                *units,
                *ok_variant,
                *error_variant,
                *invalid_utf8_variant,
            )?),
            InstructionKind::ProcessArgumentCount => {
                let count = call_int(
                    &self.backend.builder,
                    self.backend.runtime_process_argument_count_typed(),
                    &[],
                    "process.argument_count",
                )?;
                self.backend
                    .require_nonnegative_i64(count, "process.argument_count")?;
                one(count.into())
            }
            InstructionKind::ProcessArgumentAt { index } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "process argument selection has no result")
                })?;
                let index = self.int(*index)?;
                self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
                let output = if let Some(cell) = self.direct_root_cell(result)? {
                    cell
                } else {
                    self.managed_output_cell(instruction.id())?
                };
                self.backend
                    .builder
                    .build_store(output, self.backend.ptr_type.const_null())
                    .map_err(builder_error)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_process_argument_at_typed(),
                    &[index.into(), output.into()],
                    "process.argument_at.status",
                )?;
                self.backend
                    .require_zero_status(status, "process.argument_at")?;
                one(self
                    .backend
                    .builder
                    .build_load(self.backend.ptr_type, output, "process.argument_at.result")
                    .map_err(builder_error)?)
            }
            InstructionKind::ProcessEnvironment {
                name,
                missing_variant,
                found_variant,
            } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "process environment lookup has no result")
                })?;
                let ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", format!("missing result {result}"))
                    })?
                    .ty();
                let name = self.value(*name)?.into_pointer_value();
                self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
                let output = self.managed_output_cell(instruction.id())?;
                self.backend
                    .builder
                    .build_store(output, self.backend.ptr_type.const_null())
                    .map_err(builder_error)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_process_environment_typed(),
                    &[name.into(), output.into()],
                    "process.environment.status",
                )?;
                let found = self.backend.require_missing_or_found_status(
                    status,
                    PROCESS_ENVIRONMENT_TYPED_MISSING,
                    PROCESS_ENVIRONMENT_TYPED_FOUND,
                    "process.environment",
                )?;
                let value = self
                    .backend
                    .builder
                    .build_load(self.backend.ptr_type, output, "process.environment.value")
                    .map_err(builder_error)?;
                let missing_value = self.emit_sum_construct_values(ty, *missing_variant, &[])?;
                let found_value = self.emit_sum_construct_values(ty, *found_variant, &[value])?;
                one(self
                    .backend
                    .builder
                    .build_select(
                        found,
                        found_value,
                        missing_value,
                        "process.environment.option",
                    )
                    .map_err(builder_error)?)
            }
            InstructionKind::PathFromText {
                text,
                ok_variant,
                error_variant,
                contains_nul_variant,
            } => one(self.emit_path_from_text(
                instruction,
                *text,
                *ok_variant,
                *error_variant,
                *contains_nul_variant,
            )?),
            InstructionKind::PathAsText { path } => one(self
                .backend
                .builder
                .build_extract_value(self.value(*path)?.into_struct_value(), 0, "path.as_text")
                .map_err(builder_error)?),
            InstructionKind::PathJoin {
                base,
                child,
                ok_variant,
                error_variant,
                absolute_join_variant,
            } => one(self.emit_path_join(
                instruction,
                *base,
                *child,
                *ok_variant,
                *error_variant,
                *absolute_join_variant,
            )?),
            InstructionKind::BytesLength { bytes } => {
                let (_, length) = self
                    .backend
                    .text_parts(self.value(*bytes)?.into_pointer_value(), "bytes.length")?;
                one(length.into())
            }
            InstructionKind::BytesGet {
                bytes,
                index,
                missing_variant,
                found_variant,
            } => {
                let result =
                    instruction.results().first().copied().ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "Bytes.get has no result")
                    })?;
                let result_ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "Bytes.get result is missing")
                    })?
                    .ty();
                one(self.emit_bytes_get(
                    *bytes,
                    *index,
                    result_ty,
                    *missing_variant,
                    *found_variant,
                )?)
            }
            InstructionKind::BytesAppend { left, right } => {
                one(self.emit_bytes_append(instruction, *left, *right)?.into())
            }
            InstructionKind::BytesDecodeUtf8 {
                bytes,
                ok_variant,
                error_variant,
                invalid_utf8_variant,
            } => one(self.emit_bytes_decode_utf8(
                instruction,
                *bytes,
                *ok_variant,
                *error_variant,
                *invalid_utf8_variant,
            )?),
            InstructionKind::BytesCompare {
                predicate,
                left,
                right,
            } => one(self.emit_bytes_compare(*predicate, *left, *right)?.into()),
            InstructionKind::TaskCreate {
                coroutine,
                arguments,
            } => {
                let executor = self.executor_context()?;
                let mut arguments = self.call_arguments(arguments, Effects::NONE)?;
                let callee = self.backend.artifact.function(*coroutine).ok_or_else(|| {
                    CodegenError::new(
                        "InvalidFunctionReference",
                        format!("LCIR coroutine {coroutine} is missing"),
                    )
                })?;
                if callee
                    .coroutine()
                    .is_some_and(CoroutinePlan::carries_caller_span)
                {
                    let span = instruction.origin().span;
                    for coordinate in [span.file.0, span.range.start, span.range.end] {
                        arguments.push(
                            self.backend
                                .context
                                .i64_type()
                                .const_int(u64::from(coordinate), false)
                                .into(),
                        );
                    }
                }
                arguments.push(executor.into());
                one(call_pointer(
                    &self.backend.builder,
                    self.backend.function(*coroutine)?,
                    &arguments,
                    "task.create.child",
                )?
                .into())
            }
            InstructionKind::IoTaskCreate {
                operation,
                error_mode,
                arguments,
            } => one(self
                .emit_io_task_create(instruction, *operation, *error_mode, arguments)?
                .into()),
            InstructionKind::ResourceClose { kind, resource } => {
                self.emit_resource_close(instruction.id(), *kind, *resource)?
            }
            InstructionKind::TaskJoin { mode, tasks } => {
                one(self.emit_task_join(instruction, *mode, tasks)?.into())
            }
            InstructionKind::TaskJoinList { mode, tasks } => {
                one(self.emit_task_join_list(instruction, *mode, *tasks)?.into())
            }
            InstructionKind::TaskOutcomeTake { task } => {
                one(self.emit_task_outcome_take(instruction, *task)?)
            }
            InstructionKind::TextConcat { left, right } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "Text concat has no result")
                })?;
                let left = self.value(*left)?.into_pointer_value();
                let right = self.value(*right)?.into_pointer_value();
                self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
                let output = if let Some(cell) = self.direct_root_cell(result)? {
                    cell
                } else {
                    self.managed_output_cell(instruction.id())?
                };
                self.backend
                    .builder
                    .build_store(output, self.backend.ptr_type.const_null())
                    .map_err(builder_error)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_text_concat_typed(),
                    &[left.into(), right.into(), output.into()],
                    "text.concat.status",
                )?;
                self.backend.require_zero_status(status, "text.concat")?;
                one(self
                    .backend
                    .builder
                    .build_load(self.backend.ptr_type, output, "text.concat.result")
                    .map_err(builder_error)?)
            }
            InstructionKind::TextGet {
                text,
                index,
                missing_variant,
                found_variant,
            } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "Text selection has no result")
                })?;
                let ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", format!("missing result {result}"))
                    })?
                    .ty();
                let text = self.value(*text)?.into_pointer_value();
                let index = self.int(*index)?;
                self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
                let output = self.managed_output_cell(instruction.id())?;
                self.backend
                    .builder
                    .build_store(output, self.backend.ptr_type.const_null())
                    .map_err(builder_error)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_text_get_typed(),
                    &[text.into(), index.into(), output.into()],
                    "text.get.status",
                )?;
                let found = self.backend.require_text_get_status(status)?;
                let selected = self
                    .backend
                    .builder
                    .build_load(self.backend.ptr_type, output, "text.get.selected")
                    .map_err(builder_error)?;
                let missing_value = self.emit_sum_construct_values(ty, *missing_variant, &[])?;
                let found_value =
                    self.emit_sum_construct_values(ty, *found_variant, &[selected])?;
                one(self
                    .backend
                    .builder
                    .build_select(found, found_value, missing_value, "text.get.option")
                    .map_err(builder_error)?)
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
            InstructionKind::FloatParseStatus { text } => {
                one(self.emit_parse_float_status(instruction, *text)?)
            }
            InstructionKind::FloatFormat { value } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "Float formatting has no Text result")
                })?;
                self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
                let output = if let Some(cell) = self.direct_root_cell(result)? {
                    cell
                } else {
                    self.managed_output_cell(instruction.id())?
                };
                self.backend
                    .builder
                    .build_store(output, self.backend.ptr_type.const_null())
                    .map_err(builder_error)?;
                let status = call_int(
                    &self.backend.builder,
                    self.backend.runtime_format_float_typed(),
                    &[self.value(*value)?.into_float_value().into(), output.into()],
                    "format.float.status",
                )?;
                self.backend.require_zero_status(status, "format.float")?;
                one(self
                    .backend
                    .builder
                    .build_load(self.backend.ptr_type, output, "format.float.result")
                    .map_err(builder_error)?)
            }
            InstructionKind::IntToFloat { value } => one(self
                .backend
                .builder
                .build_signed_int_to_float(
                    self.int(*value)?,
                    self.backend.context.f64_type(),
                    "convert.int_to_float",
                )
                .map_err(builder_error)?
                .into()),
            InstructionKind::FloatToIntStatus { value } => {
                one(self.emit_float_to_int_status(instruction, *value)?)
            }
            InstructionKind::JsonFormat {
                json,
                ok_variant,
                error_variant,
                depth_limit_variant,
                non_finite_number_variant,
            } => one(self.emit_json_format(
                instruction,
                *json,
                *ok_variant,
                *error_variant,
                *depth_limit_variant,
                *non_finite_number_variant,
            )?),
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
            InstructionKind::ProductExtract { aggregate, field }
            | InstructionKind::ProductBorrow { aggregate, field } => one(self
                .backend
                .builder
                .build_extract_value(
                    self.value(*aggregate)?.into_struct_value(),
                    *field,
                    "product.extract",
                )
                .map_err(builder_error)?),
            InstructionKind::TaskCarrierProject { aggregate, path } => {
                one(self.emit_task_carrier_project(*aggregate, path)?)
            }
            InstructionKind::ProductSplit { aggregate } => {
                let aggregate = self.value(*aggregate)?.into_struct_value();
                (0..instruction.results().len())
                    .map(|index| {
                        let index = u32::try_from(index).map_err(|_| {
                            CodegenError::new("ProgramTooLarge", "too many product split results")
                        })?;
                        self.backend
                            .builder
                            .build_extract_value(aggregate, index, "product.split")
                            .map_err(builder_error)
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
            InstructionKind::ProductInsert {
                aggregate,
                field,
                value,
            }
            | InstructionKind::InvariantReceiverInsert {
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
            InstructionKind::TaskCarrierUpdate {
                aggregate,
                path,
                value,
            } => one(self.emit_task_carrier_update(*aggregate, path, *value)?),
            InstructionKind::TaskCarrierBorrow { value }
            | InstructionKind::RefineProven { value }
            | InstructionKind::Unrefine { value }
            | InstructionKind::UnrefineBorrow { value } => {
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
            InstructionKind::DynConstruct { variant, value } => one(self
                .emit_dyn_construct(instruction, *variant, *value)?
                .into()),
            InstructionKind::ListConstruct { elements } => {
                one(self.emit_list_construct(instruction, elements)?.into())
            }
            InstructionKind::ListAppend { list, value } => one(self
                .emit_list_append(instruction, *list, *value, false)?
                .into()),
            InstructionKind::ListAppendUnique { list, value } => one(self
                .emit_list_append(instruction, *list, *value, true)?
                .into()),
            InstructionKind::ListLength { list } => one(self.emit_list_length(*list)?.into()),
            InstructionKind::ListGet { list, index } => {
                let result =
                    instruction.results().first().copied().ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "List.get has no result")
                    })?;
                let result_ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "List.get result is missing")
                    })?
                    .ty();
                one(self.emit_list_get(*list, *index, result_ty)?)
            }
            InstructionKind::TextMapConstruct => one(self.backend.ptr_type.const_null().into()),
            InstructionKind::TextMapConstructEntries { entries } => {
                one(self.emit_text_map_construct_entries(instruction, *entries)?)
            }
            InstructionKind::TextMapInsert { map, key, value } => one(self
                .emit_text_map_insert(instruction, *map, *key, *value)?
                .into()),
            InstructionKind::TextMapLength { map } => one(self.emit_text_map_length(*map)?.into()),
            InstructionKind::TextMapContains { map, key } => {
                one(self.emit_text_map_contains(*map, *key)?.into())
            }
            InstructionKind::TextMapGet { map, key } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "TextMap.get has no result")
                })?;
                let result_ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "TextMap.get result is missing")
                    })?
                    .ty();
                one(self.emit_text_map_get(*map, *key, result_ty)?)
            }
            InstructionKind::TextMapRemove { map, key } => {
                one(self.emit_text_map_remove(instruction, *map, *key)?.into())
            }
            InstructionKind::TextMapEntryGet { map, index } => {
                let result = instruction.results().first().copied().ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "TextMap entry read has no result")
                })?;
                let result_ty = self
                    .source
                    .value(result)
                    .ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "TextMap entry read result is missing")
                    })?
                    .ty();
                one(self.emit_text_map_entry_get(*map, *index, result_ty)?)
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
                let callee_source = self.backend.artifact.function(*callee).ok_or_else(|| {
                    CodegenError::new(
                        "InvalidFunctionReference",
                        "direct LCIR callee is missing from its artifact",
                    )
                })?;
                if callee_source.effects().contains(Effects::MAY_COLLECT) {
                    self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
                }
                let arguments = self.call_arguments(arguments, callee_source.effects())?;
                let returned = call_basic(
                    &self.backend.builder,
                    self.backend.function(*callee)?,
                    &arguments,
                    "direct.call",
                )?;
                if callee_source.signature().inout_params().is_empty() {
                    one(returned)
                } else {
                    let returned = returned.into_struct_value();
                    (0..=callee_source.signature().inout_params().len())
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
        for (result, value) in instruction.results().iter().copied().zip(values) {
            self.record_value(result, value)?;
        }
        Ok(())
    }

    fn emit_bytes_append(
        &self,
        instruction: &Instruction,
        left: ValueId,
        right: ValueId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let result = instruction
            .results()
            .first()
            .copied()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Bytes.append has no result"))?;
        let left = self.value(left)?.into_pointer_value();
        let right = self.value(right)?.into_pointer_value();
        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let output = if let Some(cell) = self.direct_root_cell(result)? {
            cell
        } else {
            self.managed_output_cell(instruction.id())?
        };
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_bytes_append_typed(),
            &[left.into(), right.into(), output.into()],
            "bytes.append.status",
        )?;
        self.backend.require_zero_status(status, "bytes.append")?;
        self.backend
            .builder
            .build_load(self.backend.ptr_type, output, "bytes.append.result")
            .map_err(builder_error)
            .map(BasicValueEnum::into_pointer_value)
    }

    fn emit_path_from_text(
        &self,
        instruction: &Instruction,
        text: ValueId,
        ok_variant: u32,
        error_variant: u32,
        contains_nul_variant: u32,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result =
            instruction.results().first().copied().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Path.from_text has no result")
            })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Path.from_text result is missing"))?
            .ty();
        let path_ty = self.sum_variant_field_type(result_ty, ok_variant, 0)?;
        let error_ty = self.sum_variant_field_type(result_ty, error_variant, 0)?;
        let text = self.value(text)?;
        let (data, length) = self
            .backend
            .text_parts(text.into_pointer_value(), "path.from_text.value")?;
        let nul = self.backend.emit_text_literal("\0")?;
        let (nul_data, nul_length) = self.backend.text_parts(nul, "path.from_text.nul")?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_text_contains(),
            &[
                data.into(),
                length.into(),
                nul_data.into(),
                nul_length.into(),
            ],
            "path.from_text.status",
        )?;
        let valid = self
            .backend
            .require_zero_or_status(status, "path.from_text", 1)?;
        let path = self.emit_product_construct_values(path_ty, &[text], "path.from_text.path")?;
        let ok_value = self.emit_sum_construct_values(result_ty, ok_variant, &[path])?;
        let contains_nul = self.emit_sum_construct_values(error_ty, contains_nul_variant, &[])?;
        let error_value =
            self.emit_sum_construct_values(result_ty, error_variant, &[contains_nul])?;
        self.backend
            .builder
            .build_select(valid, ok_value, error_value, "path.from_text.result")
            .map_err(builder_error)
    }

    fn emit_path_join(
        &self,
        instruction: &Instruction,
        base: ValueId,
        child: ValueId,
        ok_variant: u32,
        error_variant: u32,
        absolute_join_variant: u32,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result = instruction
            .results()
            .first()
            .copied()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Path.join has no result"))?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "Path.join result is missing"))?
            .ty();
        let path_ty = self.sum_variant_field_type(result_ty, ok_variant, 0)?;
        let error_ty = self.sum_variant_field_type(result_ty, error_variant, 0)?;
        let base = self
            .backend
            .builder
            .build_extract_value(self.value(base)?.into_struct_value(), 0, "path.join.base")
            .map_err(builder_error)?
            .into_pointer_value();
        let child = self
            .backend
            .builder
            .build_extract_value(self.value(child)?.into_struct_value(), 0, "path.join.child")
            .map_err(builder_error)?
            .into_pointer_value();
        let output = self.managed_output_cell(instruction.id())?;
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_path_join_typed(),
            &[base.into(), child.into(), output.into()],
            "path.join.status",
        )?;
        let success =
            self.backend
                .require_zero_or_status(status, "path.join", PATH_JOIN_TYPED_ABSOLUTE)?;
        let joined = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, output, "path.join.text")
            .map_err(builder_error)?;
        let path = self.emit_product_construct_values(path_ty, &[joined], "path.join.path")?;
        let ok_value = self.emit_sum_construct_values(result_ty, ok_variant, &[path])?;
        let absolute = self.emit_sum_construct_values(error_ty, absolute_join_variant, &[])?;
        let error_value = self.emit_sum_construct_values(result_ty, error_variant, &[absolute])?;
        self.backend
            .builder
            .build_select(success, ok_value, error_value, "path.join.result")
            .map_err(builder_error)
    }

    fn emit_product_construct_values(
        &self,
        ty: ValueTypeId,
        fields: &[BasicValueEnum<'ctx>],
        name: &str,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let product = self.backend.llvm_type(ty)?.into_struct_type();
        if usize::try_from(product.count_fields()).ok() != Some(fields.len()) {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "product type {ty} has {} LLVM fields but {} values",
                    product.count_fields(),
                    fields.len()
                ),
            ));
        }
        let mut value = product.get_undef();
        for (index, field) in fields.iter().copied().enumerate() {
            let index = u32::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "too many product fields"))?;
            value = self
                .backend
                .builder
                .build_insert_value(value, field, index, name)
                .map_err(builder_error)?
                .into_struct_value();
        }
        Ok(value.into())
    }

    fn emit_task_carrier_project(
        &self,
        aggregate: ValueId,
        path: &[u32],
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let mut value = self.value(aggregate)?;
        if path.is_empty() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "Task carrier projection has an empty field path",
            ));
        }
        for field in path {
            value = self
                .backend
                .builder
                .build_extract_value(value.into_struct_value(), *field, "task_carrier.project")
                .map_err(builder_error)?;
        }
        Ok(value)
    }

    fn emit_task_carrier_update(
        &self,
        aggregate: ValueId,
        path: &[u32],
        replacement: ValueId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let Some((leaf, prefix)) = path.split_last() else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "Task carrier update has an empty field path",
            ));
        };
        let mut current = self.value(aggregate)?.into_struct_value();
        let mut parents: Vec<(StructValue<'ctx>, u32)> = Vec::with_capacity(prefix.len());
        for field in prefix {
            parents.push((current, *field));
            current = self
                .backend
                .builder
                .build_extract_value(current, *field, "task_carrier.update.extract")
                .map_err(builder_error)?
                .into_struct_value();
        }
        let mut rebuilt = self
            .backend
            .builder
            .build_insert_value(
                current,
                self.value(replacement)?,
                *leaf,
                "task_carrier.update.leaf",
            )
            .map_err(builder_error)?
            .into_struct_value();
        for (parent, field) in parents.into_iter().rev() {
            rebuilt = self
                .backend
                .builder
                .build_insert_value(parent, rebuilt, field, "task_carrier.update.parent")
                .map_err(builder_error)?
                .into_struct_value();
        }
        Ok(rebuilt.into())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "checked Float-to-Int lowering keeps its finite/range guards and status-pair construction in one auditable sequence"
    )]
    fn emit_float_to_int_status(
        &self,
        instruction: &Instruction,
        value: ValueId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result = instruction.results().first().copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "Float-to-Int conversion has no status-pair result",
            )
        })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Float-to-Int conversion result is missing")
            })?
            .ty();
        let value = self.value(value)?.into_float_value();
        let float = self.backend.context.f64_type();
        let integer = self.backend.context.i64_type();

        let finite_lower = self
            .backend
            .builder
            .build_float_compare(
                LlvmFloatPredicate::OGE,
                value,
                float.const_float(-f64::MAX),
                "convert.float_to_int.finite_lower",
            )
            .map_err(builder_error)?;
        let finite_upper = self
            .backend
            .builder
            .build_float_compare(
                LlvmFloatPredicate::OLE,
                value,
                float.const_float(f64::MAX),
                "convert.float_to_int.finite_upper",
            )
            .map_err(builder_error)?;
        let finite = self
            .backend
            .builder
            .build_and(finite_lower, finite_upper, "convert.float_to_int.finite")
            .map_err(builder_error)?;
        let in_lower_bound = self
            .backend
            .builder
            .build_float_compare(
                LlvmFloatPredicate::OGE,
                value,
                float.const_float(-9_223_372_036_854_775_808.0),
                "convert.float_to_int.lower",
            )
            .map_err(builder_error)?;
        let below_upper_bound = self
            .backend
            .builder
            .build_float_compare(
                LlvmFloatPredicate::OLT,
                value,
                float.const_float(9_223_372_036_854_775_808.0),
                "convert.float_to_int.upper",
            )
            .map_err(builder_error)?;
        let valid = self
            .backend
            .builder
            .build_and(
                in_lower_bound,
                below_upper_bound,
                "convert.float_to_int.valid",
            )
            .map_err(builder_error)?;

        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new(
                "LlvmBuilderFailed",
                "Float-to-Int conversion has no function",
            )
        })?;
        let success = self
            .backend
            .context
            .append_basic_block(function, "convert.float_to_int.success");
        let failure = self
            .backend
            .context
            .append_basic_block(function, "convert.float_to_int.failure");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "convert.float_to_int.merge");
        self.backend
            .builder
            .build_conditional_branch(valid, success, failure)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(success);
        let converted = self
            .backend
            .builder
            .build_float_to_signed_int(value, integer, "convert.float_to_int.value")
            .map_err(builder_error)?;
        let success_value = self.emit_product_construct_values(
            result_ty,
            &[converted.into(), integer.const_zero().into()],
            "convert.float_to_int.success_pair",
        )?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(failure);
        let failure_status = self
            .backend
            .builder
            .build_select(
                finite,
                integer.const_int(2, false),
                integer.const_int(1, false),
                "convert.float_to_int.failure_status",
            )
            .map_err(builder_error)?;
        let failure_value = self.emit_product_construct_values(
            result_ty,
            &[integer.const_zero().into(), failure_status],
            "convert.float_to_int.failure_pair",
        )?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(
                self.backend.llvm_type(result_ty)?,
                "convert.float_to_int.result",
            )
            .map_err(builder_error)?;
        phi.add_incoming(&[(&success_value, success), (&failure_value, failure)]);
        Ok(phi.as_basic_value())
    }

    fn emit_text_from_utf8_units(
        &self,
        instruction: &Instruction,
        units: ValueId,
        ok_variant: u32,
        error_variant: u32,
        invalid_utf8_variant: u32,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result = instruction.results().first().copied().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Text.from_utf8_units has no result")
        })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Text.from_utf8_units result is missing")
            })?
            .ty();
        let error_ty = self.sum_variant_field_type(result_ty, error_variant, 0)?;
        let units_ty = self.list_type_of_value(units)?;
        let layout = self.backend.list_layout(units_ty)?;
        if layout.element_stride != 8
            || !matches!(layout.element, BasicTypeEnum::IntType(element) if element.get_bit_width() == 64)
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "Text.from_utf8_units requires the canonical contiguous i64 List[Int] layout",
            ));
        }
        let object = self.value(units)?.into_pointer_value();
        let (data, length) = self.load_int_list_view(&layout, object)?;
        let output = self.managed_output_cell(instruction.id())?;
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_text_from_utf8_units_typed(),
            &[data.into(), length.into(), output.into()],
            "text.from_utf8_units.status",
        )?;
        let valid = self.backend.require_decode_text_status(
            status,
            "text.from_utf8_units",
            TEXT_FROM_UTF8_UNITS_TYPED_INVALID_UTF8,
        )?;
        let text = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, output, "text.from_utf8_units.text")
            .map_err(builder_error)?;
        let ok_value = self.emit_sum_construct_values(result_ty, ok_variant, &[text])?;
        let invalid_error = self.emit_sum_construct_values(error_ty, invalid_utf8_variant, &[])?;
        let error_value =
            self.emit_sum_construct_values(result_ty, error_variant, &[invalid_error])?;
        self.backend
            .builder
            .build_select(valid, ok_value, error_value, "text.from_utf8_units.result")
            .map_err(builder_error)
    }

    fn emit_bytes_decode_utf8(
        &self,
        instruction: &Instruction,
        bytes: ValueId,
        ok_variant: u32,
        error_variant: u32,
        invalid_utf8_variant: u32,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result =
            instruction.results().first().copied().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Bytes.decode_utf8 has no result")
            })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Bytes.decode_utf8 result is missing")
            })?
            .ty();
        let error_ty = self.sum_variant_field_type(result_ty, error_variant, 0)?;
        let output = self.managed_output_cell(instruction.id())?;
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_bytes_decode_utf8_typed(),
            &[
                self.value(bytes)?.into_pointer_value().into(),
                output.into(),
            ],
            "bytes.decode_utf8.status",
        )?;
        let valid = self.backend.require_bytes_decode_status(status)?;
        let text = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, output, "bytes.decode_utf8.text")
            .map_err(builder_error)?;
        let ok_value = self.emit_sum_construct_values(result_ty, ok_variant, &[text])?;
        let invalid_error = self.emit_sum_construct_values(error_ty, invalid_utf8_variant, &[])?;
        let error_value =
            self.emit_sum_construct_values(result_ty, error_variant, &[invalid_error])?;
        self.backend
            .builder
            .build_select(valid, ok_value, error_value, "bytes.decode_utf8.result")
            .map_err(builder_error)
    }

    #[expect(
        unsafe_code,
        reason = "the checked unsigned index is proven below the exact immutable Bytes payload length before forming one i8 GEP"
    )]
    fn emit_bytes_get(
        &self,
        bytes: ValueId,
        index: ValueId,
        result_ty: ValueTypeId,
        missing_variant: u32,
        found_variant: u32,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let (data, length) = self
            .backend
            .text_parts(self.value(bytes)?.into_pointer_value(), "bytes.get.source")?;
        let index = self.int(index)?;
        let source = self.current_block()?;
        let function = source
            .get_parent()
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "Bytes.get has no function"))?;
        let found = self
            .backend
            .context
            .append_basic_block(function, "bytes.get.found");
        let missing = self
            .backend
            .context
            .append_basic_block(function, "bytes.get.missing");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "bytes.get.merge");
        let in_bounds = self
            .backend
            .builder
            .build_int_compare(IntPredicate::ULT, index, length, "bytes.get.in_bounds")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(in_bounds, found, missing)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(missing);
        let missing_value = self.emit_sum_construct_values(result_ty, missing_variant, &[])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(found);
        // SAFETY: this block is reached only when the unsigned i64 `index` is
        // strictly below the validated payload length. `data` is the first
        // byte of that immutable allocation, so this one-element i8 GEP stays
        // within the same allocated object and retains its pointer provenance.
        let pointer = unsafe {
            self.backend
                .builder
                .build_gep(
                    self.backend.context.i8_type(),
                    data,
                    &[index],
                    "bytes.get.pointer",
                )
                .map_err(builder_error)?
        };
        let byte = self
            .backend
            .builder
            .build_load(self.backend.context.i8_type(), pointer, "bytes.get.byte")
            .map_err(builder_error)?
            .into_int_value();
        let byte = self
            .backend
            .builder
            .build_int_z_extend(byte, self.backend.context.i64_type(), "bytes.get.int")
            .map_err(builder_error)?;
        let found_value =
            self.emit_sum_construct_values(result_ty, found_variant, &[byte.into()])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(self.backend.llvm_type(result_ty)?, "bytes.get.result")
            .map_err(builder_error)?;
        phi.add_incoming(&[(&missing_value, missing), (&found_value, found)]);
        Ok(phi.as_basic_value())
    }

    fn emit_bytes_compare(
        &self,
        predicate: BoolPredicate,
        left: ValueId,
        right: ValueId,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let (left_data, left_length) = self
            .backend
            .text_parts(self.value(left)?.into_pointer_value(), "bytes.compare.left")?;
        let (right_data, right_length) = self.backend.text_parts(
            self.value(right)?.into_pointer_value(),
            "bytes.compare.right",
        )?;
        let same_length = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                left_length,
                right_length,
                "bytes.compare.same_length",
            )
            .map_err(builder_error)?;
        let left_is_shorter = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULE,
                left_length,
                right_length,
                "bytes.compare.left_is_shorter",
            )
            .map_err(builder_error)?;
        let compared_length = self
            .backend
            .builder
            .build_select(
                left_is_shorter,
                left_length,
                right_length,
                "bytes.compare.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let comparison = call_int(
            &self.backend.builder,
            self.backend.libc_memcmp(),
            &[left_data.into(), right_data.into(), compared_length.into()],
            "bytes.compare.memcmp",
        )?;
        let bytes_equal = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                comparison,
                self.backend.context.i32_type().const_zero(),
                "bytes.compare.equal_prefix",
            )
            .map_err(builder_error)?;
        let equal = self
            .backend
            .builder
            .build_and(same_length, bytes_equal, "bytes.compare.equal")
            .map_err(builder_error)?;
        match predicate {
            BoolPredicate::Equal => Ok(equal),
            BoolPredicate::NotEqual => self
                .backend
                .builder
                .build_not(equal, "bytes.compare.not_equal")
                .map_err(builder_error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_json_format(
        &self,
        instruction: &Instruction,
        json: ValueId,
        ok_variant: u32,
        error_variant: u32,
        depth_limit_variant: u32,
        non_finite_variant: u32,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result = instruction.results().first().copied().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "JSON formatting has no Result value")
        })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "JSON Result type is missing"))?
            .ty();
        let json_ty = self
            .source
            .value(json)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "JSON operand type is missing"))?
            .ty();
        let error_ty = self.sum_variant_field_type(result_ty, error_variant, 0)?;
        let json_storage = self.json_input_cell(instruction.id())?;
        self.backend
            .builder
            .build_store(json_storage, self.value(json)?)
            .map_err(builder_error)?;
        let output = self.managed_output_cell(instruction.id())?;
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let descriptor = self.backend.typed_json_layout_descriptor(json_ty)?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_json_format_typed(),
            &[json_storage.into(), descriptor.into(), output.into()],
            "json.format.status",
        )?;
        let (ok, _depth_limit, non_finite) = self.backend.require_json_format_status(status)?;
        let text = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, output, "json.format.text")
            .map_err(builder_error)?;
        let ok_value = self.emit_sum_construct_values(result_ty, ok_variant, &[text])?;
        let depth_error = self.emit_sum_construct_values(error_ty, depth_limit_variant, &[])?;
        let non_finite_error = self.emit_sum_construct_values(error_ty, non_finite_variant, &[])?;
        let selected_error = self
            .backend
            .builder
            .build_select(
                non_finite,
                non_finite_error,
                depth_error,
                "json.format.error",
            )
            .map_err(builder_error)?;
        let error_value =
            self.emit_sum_construct_values(result_ty, error_variant, &[selected_error])?;
        self.backend
            .builder
            .build_select(ok, ok_value, error_value, "json.format.result")
            .map_err(builder_error)
    }

    fn emit_parse_float_status(
        &self,
        instruction: &Instruction,
        text: ValueId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result = instruction.results().first().copied().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "Float parse instruction has no result")
        })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "Float parse result value is missing")
            })?
            .ty();
        let output = self.float_parse_output_cell(instruction.id())?;
        let runtime = self.backend.runtime_parse_float();
        let output_type = self.backend.context.f64_type();
        let name = "parse.float";
        self.backend
            .builder
            .build_store(output, output_type.const_zero())
            .map_err(builder_error)?;
        let (data, length) = self.backend.text_parts(
            self.value(text)?.into_pointer_value(),
            &format!("{name}.text"),
        )?;
        let status = call_int(
            &self.backend.builder,
            runtime,
            &[data.into(), length.into(), output.into()],
            &format!("{name}.status"),
        )?;
        self.backend.require_float_parse_status(status)?;
        let parsed = self
            .backend
            .builder
            .build_load(output_type, output, &format!("{name}.value"))
            .map_err(builder_error)?;
        let status = self
            .backend
            .builder
            .build_int_z_extend(
                status,
                self.backend.context.i64_type(),
                &format!("{name}.status.value"),
            )
            .map_err(builder_error)?;
        self.emit_product_construct_values(
            result_ty,
            &[parsed, status.into()],
            &format!("{name}.result"),
        )
    }

    fn sum_variant_field_type(
        &self,
        ty: ValueTypeId,
        variant: u32,
        field: usize,
    ) -> Result<ValueTypeId, CodegenError> {
        let representations = self.backend.artifact.representations();
        let value_type = representations.value_type(ty).ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", format!("missing sum value type {ty}"))
        })?;
        let Repr::Sum(sum) = representations
            .repr(value_type.repr())
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("missing sum value representation for {ty}"),
                )
            })?
        else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("value type {ty} is not represented as a sum"),
            ));
        };
        representations
            .sum(sum)
            .and_then(|sum| {
                usize::try_from(variant)
                    .ok()
                    .and_then(|variant| sum.variants().get(variant))
            })
            .and_then(|variant| variant.fields().get(field))
            .copied()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("sum value type {ty} has no variant {variant} field {field}"),
                )
            })
    }

    fn emit_sum_construct(
        &self,
        ty: ValueTypeId,
        variant: u32,
        payload: &[ValueId],
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let payload = payload
            .iter()
            .copied()
            .map(|value| self.value(value))
            .collect::<Result<Vec<_>, _>>()?;
        self.emit_sum_construct_values(ty, variant, &payload)
    }

    fn emit_sum_construct_values(
        &self,
        ty: ValueTypeId,
        variant: u32,
        payload: &[BasicValueEnum<'ctx>],
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
                .build_insert_value(payload_value, value, index, "sum.payload")
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
                let carrier = self.pack_sum_carrier(
                    payload_value,
                    payload_type,
                    carrier_type,
                    layout.payload_byte_offset(variant_index)?,
                )?;
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

    fn list_type_of_value(&self, value: ValueId) -> Result<ValueTypeId, CodegenError> {
        self.source
            .value(value)
            .map(loom_codegen_ir::Value::ty)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", format!("missing List value {value}"))
            })
    }

    fn list_field_pointer(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.backend
            .builder
            .build_struct_gep(layout.object, object, field, name)
            .map_err(builder_error)
    }

    fn list_element_pointer(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let data = self.list_field_pointer(layout, object, 2, &format!("{name}.data"))?;
        let address = self
            .backend
            .builder
            .build_ptr_to_int(
                data,
                self.backend.context.i64_type(),
                &format!("{name}.base"),
            )
            .map_err(builder_error)?;
        let offset = self
            .backend
            .builder
            .build_int_mul(
                index,
                self.backend
                    .context
                    .i64_type()
                    .const_int(layout.element_stride, false),
                &format!("{name}.offset"),
            )
            .map_err(builder_error)?;
        let address = self
            .backend
            .builder
            .build_int_add(address, offset, &format!("{name}.address"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_int_to_ptr(address, self.backend.ptr_type, name)
            .map_err(builder_error)
    }

    fn load_int_list_view(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let (length, _) = self.load_list_header(layout, object, "text.from_utf8_units.list")?;
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new(
                "LlvmBuilderFailed",
                "Text.from_utf8_units List view has no function",
            )
        })?;
        let empty = self
            .backend
            .context
            .append_basic_block(function, "text.from_utf8_units.data.empty");
        let present = self
            .backend
            .context
            .append_basic_block(function, "text.from_utf8_units.data.present");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "text.from_utf8_units.data.merge");
        let is_null = self
            .backend
            .builder
            .build_is_null(object, "text.from_utf8_units.list.is_null")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_null, empty, present)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let null = self.backend.ptr_type.const_null();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(present);
        let data = self.list_field_pointer(layout, object, 2, "text.from_utf8_units.list.data")?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let data_phi = self
            .backend
            .builder
            .build_phi(self.backend.ptr_type, "text.from_utf8_units.data")
            .map_err(builder_error)?;
        data_phi.add_incoming(&[(&null, empty), (&data, present)]);
        Ok((data_phi.as_basic_value().into_pointer_value(), length))
    }

    fn load_list_header(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let source = self.current_block()?;
        let function = source
            .get_parent()
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "List header has no function"))?;
        let empty = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.empty"));
        let present = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.present"));
        let merge = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.merge"));
        let is_null = self
            .backend
            .builder
            .build_is_null(object, &format!("{name}.is_null"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_null, empty, present)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let zero = self.backend.context.i64_type().const_zero();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(present);
        let length_pointer = self.list_field_pointer(layout, object, 0, "list.length.pointer")?;
        let capacity_pointer =
            self.list_field_pointer(layout, object, 1, "list.capacity.pointer")?;
        let length = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                length_pointer,
                "list.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let capacity = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                capacity_pointer,
                "list.capacity",
            )
            .map_err(builder_error)?
            .into_int_value();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let length_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.length"))
            .map_err(builder_error)?;
        length_phi.add_incoming(&[(&zero, empty), (&length, present)]);
        let capacity_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.capacity"))
            .map_err(builder_error)?;
        capacity_phi.add_incoming(&[(&zero, empty), (&capacity, present)]);
        Ok((
            length_phi.as_basic_value().into_int_value(),
            capacity_phi.as_basic_value().into_int_value(),
        ))
    }

    fn validate_static_list_capacity(
        layout: &ListLayout<'ctx>,
        capacity: u64,
    ) -> Result<(), CodegenError> {
        let allocation = layout
            .element_stride
            .checked_mul(capacity)
            .and_then(|bytes| bytes.checked_add(layout.fixed_size));
        let pointer_cells = u64::try_from(layout.pointer_offsets.len())
            .ok()
            .and_then(|count| count.checked_mul(capacity));
        if allocation.is_none_or(|bytes| bytes > GC_MAX_OBJECT_BYTES)
            || pointer_cells.is_none_or(|cells| cells > GC_MAX_REPEATED_POINTER_CELLS)
        {
            return Err(CodegenError::new(
                "ProgramTooLarge",
                "typed List literal exceeds repeated-allocation runtime limits",
            ));
        }
        Ok(())
    }

    fn allocate_list(
        &self,
        ty: ValueTypeId,
        layout: &ListLayout<'ctx>,
        capacity: IntValue<'ctx>,
        result: ValueId,
        site: ManagedSafepoint,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let output = if let Some(cell) = self.direct_root_cell(result)? {
            cell
        } else {
            self.backend
                .builder
                .build_alloca(self.backend.ptr_type, &format!("{name}.output"))
                .map_err(builder_error)?
        };
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(site)?;
        let descriptor = self.backend.list_descriptor(ty, layout)?;
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_repeated_alloc(),
            &[descriptor.into(), capacity.into(), output.into()],
            &format!("{name}.status"),
        )?;
        self.backend.require_zero_status(status, name)?;
        self.backend
            .builder
            .build_load(self.backend.ptr_type, output, &format!("{name}.result"))
            .map_err(builder_error)
            .map(BasicValueEnum::into_pointer_value)
    }

    fn store_list_header(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        length: IntValue<'ctx>,
        capacity: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let length_pointer = self.list_field_pointer(layout, object, 0, "list.store.length")?;
        let capacity_pointer = self.list_field_pointer(layout, object, 1, "list.store.capacity")?;
        self.backend
            .builder
            .build_store(length_pointer, length)
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(capacity_pointer, capacity)
            .map_err(builder_error)?;
        Ok(())
    }

    fn store_list_element(
        &self,
        layout: &ListLayout<'ctx>,
        object: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        value: BasicValueEnum<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        if self.backend.target_data.get_abi_size(&layout.element) == 0 {
            return Ok(());
        }
        let pointer = self.list_element_pointer(layout, object, index, name)?;
        self.backend
            .builder
            .build_store(pointer, value)
            .map_err(builder_error)?;
        Ok(())
    }

    fn emit_list_construct(
        &self,
        instruction: &Instruction,
        elements: &[ValueId],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let result =
            instruction.results().first().copied().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "List construction has no result")
            })?;
        let ty = self.list_type_of_value(result)?;
        if elements.is_empty() {
            return Ok(self.backend.ptr_type.const_null());
        }
        let length = u64::try_from(elements.len())
            .map_err(|_| CodegenError::new("ProgramTooLarge", "List literal is too large"))?;
        let capacity = length.checked_next_power_of_two().ok_or_else(|| {
            CodegenError::new("ProgramTooLarge", "List literal capacity overflowed")
        })?;
        let layout = self.backend.list_layout(ty)?;
        Self::validate_static_list_capacity(&layout, capacity)?;
        let length_value = self.backend.context.i64_type().const_int(length, false);
        let capacity_value = self.backend.context.i64_type().const_int(capacity, false);
        let object = self.allocate_list(
            ty,
            &layout,
            capacity_value,
            result,
            ManagedSafepoint::Instruction(instruction.id()),
            "list.construct",
        )?;
        self.store_list_header(&layout, object, length_value, capacity_value)?;
        for (index, element) in elements.iter().copied().enumerate() {
            let index = u64::try_from(index)
                .map_err(|_| CodegenError::new("ProgramTooLarge", "List index exceeds u64"))?;
            let index = self.backend.context.i64_type().const_int(index, false);
            // Reload only after the allocator: managed siblings and aliases may
            // all have moved at this safepoint.
            let value = self.value(element)?;
            self.store_list_element(&layout, object, index, value, "list.construct.element")?;
        }
        Ok(object)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "immutable append keeps capacity math, allocation, post-GC reload, copy, and publication in one audited sequence"
    )]
    fn emit_list_append(
        &self,
        instruction: &Instruction,
        list: ValueId,
        value: ValueId,
        unique: bool,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let result = instruction
            .results()
            .first()
            .copied()
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "List append has no result"))?;
        let ty = self.list_type_of_value(result)?;
        let layout = self.backend.list_layout(ty)?;
        let old_object = self.value(list)?.into_pointer_value();
        let (old_length, old_capacity) =
            self.load_list_header(&layout, old_object, "list.append.old")?;
        let one = self.backend.context.i64_type().const_int(1, false);
        let new_length = self
            .backend
            .builder
            .build_int_add(old_length, one, "list.append.new_length")
            .map_err(builder_error)?;
        let needs_growth = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                old_capacity,
                new_length,
                "list.append.needs_growth",
            )
            .map_err(builder_error)?;
        let doubled = self
            .backend
            .builder
            .build_int_mul(
                old_capacity,
                self.backend.context.i64_type().const_int(2, false),
                "list.append.doubled_capacity",
            )
            .map_err(builder_error)?;
        let doubled_or_one = self
            .backend
            .builder
            .build_select(
                self.backend
                    .builder
                    .build_int_compare(
                        IntPredicate::ULT,
                        doubled,
                        one,
                        "list.append.capacity_below_one",
                    )
                    .map_err(builder_error)?,
                one,
                doubled,
                "list.append.grown_capacity",
            )
            .map_err(builder_error)?
            .into_int_value();
        let new_capacity = self
            .backend
            .builder
            .build_select(
                needs_growth,
                doubled_or_one,
                old_capacity,
                "list.append.new_capacity",
            )
            .map_err(builder_error)?
            .into_int_value();
        if unique {
            let source = self.current_block()?;
            let function = source.get_parent().ok_or_else(|| {
                CodegenError::new("LlvmBuilderFailed", "List.append has no function")
            })?;
            let reuse = self
                .backend
                .context
                .append_basic_block(function, "list.append.unique.reuse");
            let grow = self
                .backend
                .context
                .append_basic_block(function, "list.append.unique.grow");
            let merge = self
                .backend
                .context
                .append_basic_block(function, "list.append.unique.merge");
            let non_null = self
                .backend
                .builder
                .build_is_not_null(old_object, "list.append.unique.non_null")
                .map_err(builder_error)?;
            let has_capacity = self
                .backend
                .builder
                .build_not(needs_growth, "list.append.unique.has_capacity")
                .map_err(builder_error)?;
            let can_reuse = self
                .backend
                .builder
                .build_and(non_null, has_capacity, "list.append.unique.can_reuse")
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_conditional_branch(can_reuse, reuse, grow)
                .map_err(builder_error)?;

            self.backend.builder.position_at_end(reuse);
            let appended = self.value(value)?;
            self.store_list_element(
                &layout,
                old_object,
                old_length,
                appended,
                "list.append.unique.element",
            )?;
            let length_pointer =
                self.list_field_pointer(&layout, old_object, 0, "list.append.unique.store_length")?;
            self.backend
                .builder
                .build_store(length_pointer, new_length)
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_unconditional_branch(merge)
                .map_err(builder_error)?;
            let reuse_end = self.current_block()?;

            self.backend.builder.position_at_end(grow);
            let object = self.emit_allocating_list_append(
                instruction,
                list,
                value,
                result,
                ty,
                &layout,
                old_length,
                new_length,
                new_capacity,
            )?;
            self.backend
                .builder
                .build_unconditional_branch(merge)
                .map_err(builder_error)?;
            let grow_end = self.current_block()?;

            self.backend.builder.position_at_end(merge);
            let object_phi = self
                .backend
                .builder
                .build_phi(self.backend.ptr_type, "list.append.unique.result")
                .map_err(builder_error)?;
            object_phi.add_incoming(&[(&old_object, reuse_end), (&object, grow_end)]);
            return Ok(object_phi.as_basic_value().into_pointer_value());
        }

        self.emit_allocating_list_append(
            instruction,
            list,
            value,
            result,
            ty,
            &layout,
            old_length,
            new_length,
            new_capacity,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_allocating_list_append(
        &self,
        instruction: &Instruction,
        list: ValueId,
        value: ValueId,
        result: ValueId,
        ty: ValueTypeId,
        layout: &ListLayout<'ctx>,
        old_length: IntValue<'ctx>,
        new_length: IntValue<'ctx>,
        new_capacity: IntValue<'ctx>,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let object = self.allocate_list(
            ty,
            layout,
            new_capacity,
            result,
            ManagedSafepoint::Instruction(instruction.id()),
            "list.append",
        )?;
        self.store_list_header(layout, object, new_length, new_capacity)?;

        // Reload the old base only after allocation. Selecting the fresh object
        // for an empty/null source keeps even zero-byte memcpy away from null.
        let old_object = self.value(list)?.into_pointer_value();
        let copy_source = self
            .backend
            .builder
            .build_select(
                self.backend
                    .builder
                    .build_is_null(old_object, "list.append.old.is_null")
                    .map_err(builder_error)?,
                object,
                old_object,
                "list.append.copy_source",
            )
            .map_err(builder_error)?
            .into_pointer_value();
        let source = self.list_field_pointer(layout, copy_source, 2, "list.append.source")?;
        let destination = self.list_field_pointer(layout, object, 2, "list.append.destination")?;
        let copy_bytes = self
            .backend
            .builder
            .build_int_mul(
                old_length,
                self.backend
                    .context
                    .i64_type()
                    .const_int(layout.element_stride, false),
                "list.append.copy_bytes",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_memcpy(
                destination,
                layout.element_align,
                source,
                layout.element_align,
                copy_bytes,
            )
            .map_err(builder_error)?;
        let appended = self.value(value)?;
        self.store_list_element(layout, object, old_length, appended, "list.append.element")?;
        Ok(object)
    }

    fn emit_list_length(&self, list: ValueId) -> Result<IntValue<'ctx>, CodegenError> {
        let ty = self.list_type_of_value(list)?;
        let layout = self.backend.list_layout(ty)?;
        let object = self.value(list)?.into_pointer_value();
        self.load_list_header(&layout, object, "list.length")
            .map(|(length, _)| length)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "bounds checking and exact Option construction form one control-flow operation with no runtime helper"
    )]
    fn emit_list_get(
        &self,
        list: ValueId,
        index: ValueId,
        result_ty: ValueTypeId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let list_ty = self.list_type_of_value(list)?;
        let layout = self.backend.list_layout(list_ty)?;
        let object = self.value(list)?.into_pointer_value();
        let index = self.int(index)?;
        let source = self.current_block()?;
        let function = source
            .get_parent()
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "List.get has no function"))?;
        let bounds = self
            .backend
            .context
            .append_basic_block(function, "list.get.bounds");
        let some = self
            .backend
            .context
            .append_basic_block(function, "list.get.some");
        let none = self
            .backend
            .context
            .append_basic_block(function, "list.get.none");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "list.get.merge");
        let not_null = self
            .backend
            .builder
            .build_is_not_null(object, "list.get.not_null")
            .map_err(builder_error)?;
        let nonnegative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                index,
                self.backend.context.i64_type().const_zero(),
                "list.get.nonnegative",
            )
            .map_err(builder_error)?;
        let can_check = self
            .backend
            .builder
            .build_and(not_null, nonnegative, "list.get.can_check")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(can_check, bounds, none)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(bounds);
        let length_pointer =
            self.list_field_pointer(&layout, object, 0, "list.get.length.pointer")?;
        let length = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                length_pointer,
                "list.get.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let in_bounds = self
            .backend
            .builder
            .build_int_compare(IntPredicate::ULT, index, length, "list.get.in_bounds")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(in_bounds, some, none)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(none);
        let none_value = self.emit_sum_construct_values(result_ty, 0, &[])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(some);
        let element = if self.backend.target_data.get_abi_size(&layout.element) == 0 {
            match layout.element {
                BasicTypeEnum::ArrayType(ty) => ty.const_zero().into(),
                BasicTypeEnum::FloatType(ty) => ty.const_zero().into(),
                BasicTypeEnum::IntType(ty) => ty.const_zero().into(),
                BasicTypeEnum::PointerType(ty) => ty.const_null().into(),
                BasicTypeEnum::StructType(ty) => ty.const_zero().into(),
                BasicTypeEnum::VectorType(ty) => ty.const_zero().into(),
                BasicTypeEnum::ScalableVectorType(ty) => ty.const_zero().into(),
            }
        } else {
            let pointer = self.list_element_pointer(&layout, object, index, "list.get.element")?;
            self.backend
                .builder
                .build_load(layout.element, pointer, "list.get.value")
                .map_err(builder_error)?
        };
        let some_value = self.emit_sum_construct_values(result_ty, 1, &[element])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(self.backend.llvm_type(result_ty)?, "list.get.result")
            .map_err(builder_error)?;
        phi.add_incoming(&[(&none_value, none), (&some_value, some)]);
        Ok(phi.as_basic_value())
    }

    fn text_map_type_of_value(&self, value: ValueId) -> Result<ValueTypeId, CodegenError> {
        self.source
            .value(value)
            .map(loom_codegen_ir::Value::ty)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", format!("missing TextMap value {value}"))
            })
    }

    fn text_map_field_pointer(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.backend
            .builder
            .build_struct_gep(layout.object, object, field, name)
            .map_err(builder_error)
    }

    fn text_map_entry_pointer(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let data = self.text_map_field_pointer(layout, object, 1, &format!("{name}.data"))?;
        let base = self
            .backend
            .builder
            .build_ptr_to_int(
                data,
                self.backend.context.i64_type(),
                &format!("{name}.base"),
            )
            .map_err(builder_error)?;
        let offset = self
            .backend
            .builder
            .build_int_mul(
                index,
                self.backend
                    .context
                    .i64_type()
                    .const_int(layout.entry_stride, false),
                &format!("{name}.offset"),
            )
            .map_err(builder_error)?;
        let address = self
            .backend
            .builder
            .build_int_add(base, offset, &format!("{name}.address"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_int_to_ptr(address, self.backend.ptr_type, name)
            .map_err(builder_error)
    }

    fn text_map_entry_field_pointer(
        &self,
        layout: &TextMapLayout<'ctx>,
        entry: PointerValue<'ctx>,
        field: u32,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        self.backend
            .builder
            .build_struct_gep(layout.entry, entry, field, name)
            .map_err(builder_error)
    }

    fn load_text_map_length(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "TextMap header has no function")
        })?;
        let empty = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.empty"));
        let present = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.present"));
        let merge = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.merge"));
        let is_null = self
            .backend
            .builder
            .build_is_null(object, &format!("{name}.is_null"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_null, empty, present)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let zero = self.backend.context.i64_type().const_zero();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(present);
        let pointer = self.text_map_field_pointer(layout, object, 0, "text_map.length.pointer")?;
        let length = self
            .backend
            .builder
            .build_load(self.backend.context.i64_type(), pointer, "text_map.length")
            .map_err(builder_error)?
            .into_int_value();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.length"))
            .map_err(builder_error)?;
        phi.add_incoming(&[(&zero, empty), (&length, present)]);
        Ok(phi.as_basic_value().into_int_value())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the null-safe TextMap branch and exact cross-target field-layout proof form one typed ABI projection"
    )]
    fn typed_log_fields(
        &self,
        fields: ValueId,
    ) -> Result<(PointerValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let ty = self.text_map_type_of_value(fields)?;
        let value_ty = self.backend.text_map_value_type(ty)?;
        let value_type = self
            .backend
            .artifact
            .representations()
            .value_type(value_ty)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("typed log TextMap {ty} has no value type {value_ty}"),
                )
            })?;
        let layout = self.backend.text_map_layout(ty)?;
        let key_offset = self
            .backend
            .target_data
            .offset_of_element(&layout.entry, 0)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed log key offset is missing"))?;
        let value_offset = self
            .backend
            .target_data
            .offset_of_element(&layout.entry, 1)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "typed log value offset is missing")
            })?;
        let direct_text = matches!(
            layout.value,
            BasicTypeEnum::PointerType(pointer) if pointer == self.backend.ptr_type
        );
        if value_type.semantic() != &Type::Text
            || !direct_text
            || layout.entry_stride != TYPED_LOG_FIELD_SIZE
            || u64::from(layout.entry_align) != TYPED_LOG_FIELD_ALIGNMENT
            || key_offset != TYPED_LOG_FIELD_KEY_OFFSET
            || value_offset != TYPED_LOG_FIELD_VALUE_OFFSET
        {
            return Err(CodegenError::new(
                "LcirTypedLogAbiMismatch",
                format!(
                    "TextMap type {ty} has typed-log value/layout {:?}/{}/{}/{key_offset}/{value_offset}, expected Text/pointer/{TYPED_LOG_FIELD_SIZE}/{TYPED_LOG_FIELD_KEY_OFFSET}/{TYPED_LOG_FIELD_VALUE_OFFSET} with alignment {TYPED_LOG_FIELD_ALIGNMENT}",
                    value_type.semantic(),
                    layout.entry_stride,
                    layout.entry_align,
                ),
            ));
        }

        let object = self.value(fields)?.into_pointer_value();
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "typed logging has no active function")
        })?;
        let empty = self
            .backend
            .context
            .append_basic_block(function, "log.fields.empty");
        let present = self
            .backend
            .context
            .append_basic_block(function, "log.fields.present");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "log.fields.merge");
        let is_null = self
            .backend
            .builder
            .build_is_null(object, "log.fields.is_null")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_null, empty, present)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let empty_pointer = self.backend.ptr_type.const_null();
        let empty_count = self.backend.context.i64_type().const_zero();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(present);
        let length_pointer =
            self.text_map_field_pointer(&layout, object, 0, "log.fields.length.pointer")?;
        let count = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                length_pointer,
                "log.fields.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let entries = self.text_map_field_pointer(&layout, object, 1, "log.fields.entries")?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let pointer_phi = self
            .backend
            .builder
            .build_phi(self.backend.ptr_type, "log.fields.pointer")
            .map_err(builder_error)?;
        pointer_phi.add_incoming(&[(&empty_pointer, empty), (&entries, present)]);
        let count_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), "log.fields.count")
            .map_err(builder_error)?;
        count_phi.add_incoming(&[(&empty_count, empty), (&count, present)]);
        Ok((
            pointer_phi.as_basic_value().into_pointer_value(),
            count_phi.as_basic_value().into_int_value(),
        ))
    }

    fn compare_text_keys(
        &self,
        left: PointerValue<'ctx>,
        right: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let (left_data, left_length) = self.backend.text_parts(left, &format!("{name}.left"))?;
        let (right_data, right_length) =
            self.backend.text_parts(right, &format!("{name}.right"))?;
        let left_shorter = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                left_length,
                right_length,
                &format!("{name}.left_shorter"),
            )
            .map_err(builder_error)?;
        let common_length = self
            .backend
            .builder
            .build_select(
                left_shorter,
                left_length,
                right_length,
                &format!("{name}.common_length"),
            )
            .map_err(builder_error)?
            .into_int_value();
        let ordering = call_int(
            &self.backend.builder,
            self.backend.libc_memcmp(),
            &[left_data.into(), right_data.into(), common_length.into()],
            &format!("{name}.memcmp"),
        )?;
        let bytes_equal = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                ordering,
                self.backend.context.i32_type().const_zero(),
                &format!("{name}.bytes_equal"),
            )
            .map_err(builder_error)?;
        let same_length = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                left_length,
                right_length,
                &format!("{name}.same_length"),
            )
            .map_err(builder_error)?;
        let equal = self
            .backend
            .builder
            .build_and(bytes_equal, same_length, &format!("{name}.equal"))
            .map_err(builder_error)?;
        let bytes_greater = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SGT,
                ordering,
                self.backend.context.i32_type().const_zero(),
                &format!("{name}.bytes_greater"),
            )
            .map_err(builder_error)?;
        let longer_prefix = self
            .backend
            .builder
            .build_and(
                bytes_equal,
                self.backend
                    .builder
                    .build_int_compare(
                        IntPredicate::UGT,
                        left_length,
                        right_length,
                        &format!("{name}.longer"),
                    )
                    .map_err(builder_error)?,
                &format!("{name}.longer_prefix"),
            )
            .map_err(builder_error)?;
        let greater = self
            .backend
            .builder
            .build_or(bytes_greater, longer_prefix, &format!("{name}.greater"))
            .map_err(builder_error)?;
        Ok((equal, greater))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the canonical sorted lookup loop returns both replacement and insertion position without allocating"
    )]
    fn locate_text_map_key(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        length: IntValue<'ctx>,
        key: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "TextMap lookup has no function")
        })?;
        let header = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.header"));
        let inspect = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.inspect"));
        let unequal = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.unequal"));
        let next = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.next"));
        let found = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.found"));
        let absent = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.absent"));
        let merge = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.merge"));
        self.backend
            .builder
            .build_unconditional_branch(header)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(header);
        let index_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.index"))
            .map_err(builder_error)?;
        let zero = self.backend.context.i64_type().const_zero();
        index_phi.add_incoming(&[(&zero, source)]);
        let index = index_phi.as_basic_value().into_int_value();
        let in_bounds = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                index,
                length,
                &format!("{name}.in_bounds"),
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(in_bounds, inspect, absent)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(inspect);
        let entry = self.text_map_entry_pointer(layout, object, index, "text_map.lookup.entry")?;
        let key_pointer =
            self.text_map_entry_field_pointer(layout, entry, 0, "text_map.lookup.key.pointer")?;
        let candidate = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, key_pointer, "text_map.lookup.key")
            .map_err(builder_error)?
            .into_pointer_value();
        let (equal, greater) = self.compare_text_keys(candidate, key, "text_map.lookup.compare")?;
        self.backend
            .builder
            .build_conditional_branch(equal, found, unequal)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(unequal);
        self.backend
            .builder
            .build_conditional_branch(greater, absent, next)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(next);
        let successor = self
            .backend
            .builder
            .build_int_add(
                index,
                self.backend.context.i64_type().const_int(1, false),
                &format!("{name}.successor"),
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unconditional_branch(header)
            .map_err(builder_error)?;
        index_phi.add_incoming(&[(&successor, next)]);

        self.backend.builder.position_at_end(found);
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(absent);
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let position = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.position"))
            .map_err(builder_error)?;
        position.add_incoming(&[(&index, found), (&index, absent)]);
        let present = self
            .backend
            .builder
            .build_phi(self.backend.context.bool_type(), &format!("{name}.present"))
            .map_err(builder_error)?;
        present.add_incoming(&[
            (&self.backend.context.bool_type().const_int(1, false), found),
            (&self.backend.context.bool_type().const_zero(), absent),
        ]);
        Ok((
            position.as_basic_value().into_int_value(),
            present.as_basic_value().into_int_value(),
        ))
    }

    fn allocate_text_map(
        &self,
        ty: ValueTypeId,
        layout: &TextMapLayout<'ctx>,
        capacity: IntValue<'ctx>,
        result: ValueId,
        site: ManagedSafepoint,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let output = if let Some(cell) = self.direct_root_cell(result)? {
            cell
        } else {
            self.backend
                .builder
                .build_alloca(self.backend.ptr_type, &format!("{name}.output"))
                .map_err(builder_error)?
        };
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(site)?;
        let descriptor = self.backend.text_map_descriptor(ty, layout)?;
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_repeated_alloc(),
            &[descriptor.into(), capacity.into(), output.into()],
            &format!("{name}.status"),
        )?;
        self.backend.require_zero_status(status, name)?;
        self.backend
            .builder
            .build_load(self.backend.ptr_type, output, &format!("{name}.result"))
            .map_err(builder_error)
            .map(BasicValueEnum::into_pointer_value)
    }

    fn allocate_text_map_bulk(
        &self,
        ty: ValueTypeId,
        layout: &TextMapLayout<'ctx>,
        length: IntValue<'ctx>,
        site: ManagedSafepoint,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let output = self
            .backend
            .builder
            .build_alloca(self.backend.ptr_type, &format!("{name}.output"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(site)?;
        let descriptor = self.backend.text_map_descriptor(ty, layout)?;
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_repeated_alloc(),
            &[descriptor.into(), length.into(), output.into()],
            &format!("{name}.status"),
        )?;
        self.backend.require_zero_status(status, name)?;
        self.backend
            .builder
            .build_load(self.backend.ptr_type, output, &format!("{name}.result"))
            .map_err(builder_error)
            .map(BasicValueEnum::into_pointer_value)
    }

    fn load_text_map_entry_key(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let entry = self.text_map_entry_pointer(layout, object, index, name)?;
        let key =
            self.text_map_entry_field_pointer(layout, entry, 0, &format!("{name}.key.pointer"))?;
        self.backend
            .builder
            .build_load(self.backend.ptr_type, key, &format!("{name}.key"))
            .map_err(builder_error)
            .map(BasicValueEnum::into_pointer_value)
    }

    fn swap_text_map_entries(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let left_pointer =
            self.text_map_entry_pointer(layout, object, left, &format!("{name}.left"))?;
        let right_pointer =
            self.text_map_entry_pointer(layout, object, right, &format!("{name}.right"))?;
        let left_value = self
            .backend
            .builder
            .build_load(layout.entry, left_pointer, &format!("{name}.left.value"))
            .map_err(builder_error)?;
        let right_value = self
            .backend
            .builder
            .build_load(layout.entry, right_pointer, &format!("{name}.right.value"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(left_pointer, right_value)
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(right_pointer, left_value)
            .map_err(builder_error)?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one iterative sift-down keeps the no-safepoint heap certificate and its CFG explicit"
    )]
    fn sift_down_text_map(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        initial_root: IntValue<'ctx>,
        limit: IntValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "TextMap heapsort has no function")
        })?;
        let header = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.header"));
        let inspect = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.inspect"));
        let compare_right = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.compare_right"));
        let decide = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.decide"));
        let swap = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.swap"));
        let done = self
            .backend
            .context
            .append_basic_block(function, &format!("{name}.done"));
        self.backend
            .builder
            .build_unconditional_branch(header)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(header);
        let root_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.root"))
            .map_err(builder_error)?;
        root_phi.add_incoming(&[(&initial_root, source)]);
        let root = root_phi.as_basic_value().into_int_value();
        let two = self.backend.context.i64_type().const_int(2, false);
        let one = self.backend.context.i64_type().const_int(1, false);
        let left = self
            .backend
            .builder
            .build_int_add(
                self.backend
                    .builder
                    .build_int_mul(root, two, &format!("{name}.root_twice"))
                    .map_err(builder_error)?,
                one,
                &format!("{name}.left"),
            )
            .map_err(builder_error)?;
        let has_left = self
            .backend
            .builder
            .build_int_compare(IntPredicate::ULT, left, limit, &format!("{name}.has_left"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(has_left, inspect, done)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(inspect);
        let left_key =
            self.load_text_map_entry_key(layout, object, left, &format!("{name}.left"))?;
        let root_key =
            self.load_text_map_entry_key(layout, object, root, &format!("{name}.root"))?;
        let (_, left_greater) =
            self.compare_text_keys(left_key, root_key, &format!("{name}.compare_left"))?;
        let candidate = self
            .backend
            .builder
            .build_select(left_greater, left, root, &format!("{name}.candidate"))
            .map_err(builder_error)?
            .into_int_value();
        let right = self
            .backend
            .builder
            .build_int_add(left, one, &format!("{name}.right"))
            .map_err(builder_error)?;
        let has_right = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                right,
                limit,
                &format!("{name}.has_right"),
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(has_right, compare_right, decide)
            .map_err(builder_error)?;
        let no_right = self.current_block()?;

        self.backend.builder.position_at_end(compare_right);
        let right_key =
            self.load_text_map_entry_key(layout, object, right, &format!("{name}.right"))?;
        let candidate_key =
            self.load_text_map_entry_key(layout, object, candidate, &format!("{name}.candidate"))?;
        let (_, right_greater) = self.compare_text_keys(
            right_key,
            candidate_key,
            &format!("{name}.compare_right_key"),
        )?;
        let right_candidate = self
            .backend
            .builder
            .build_select(
                right_greater,
                right,
                candidate,
                &format!("{name}.right_candidate"),
            )
            .map_err(builder_error)?
            .into_int_value();
        self.backend
            .builder
            .build_unconditional_branch(decide)
            .map_err(builder_error)?;
        let with_right = self.current_block()?;

        self.backend.builder.position_at_end(decide);
        let candidate_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), &format!("{name}.selected"))
            .map_err(builder_error)?;
        candidate_phi.add_incoming(&[(&candidate, no_right), (&right_candidate, with_right)]);
        let selected = candidate_phi.as_basic_value().into_int_value();
        let unchanged = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                selected,
                root,
                &format!("{name}.unchanged"),
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(unchanged, done, swap)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(swap);
        self.swap_text_map_entries(layout, object, root, selected, &format!("{name}.swap"))?;
        self.backend
            .builder
            .build_unconditional_branch(header)
            .map_err(builder_error)?;
        let swapped = self.current_block()?;
        root_phi.add_incoming(&[(&selected, swapped)]);
        self.backend.builder.position_at_end(done);
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one no-safepoint heapsort routine keeps heap construction and extraction CFG certificates auditable together"
    )]
    fn sort_text_map_entries(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        length: IntValue<'ctx>,
    ) -> Result<(), CodegenError> {
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "TextMap heapsort has no function")
        })?;
        let heap_header = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.heap.header");
        let heap_body = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.heap.body");
        let sort_header = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.sort.header");
        let sort_body = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.sort.body");
        let done = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.sort.done");
        let half = self
            .backend
            .builder
            .build_int_unsigned_div(
                length,
                self.backend.context.i64_type().const_int(2, false),
                "text_map.bulk.half",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unconditional_branch(heap_header)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(heap_header);
        let heap_index = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), "text_map.bulk.heap.index")
            .map_err(builder_error)?;
        heap_index.add_incoming(&[(&half, source)]);
        let heap_index_value = heap_index.as_basic_value().into_int_value();
        let heap_more = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::UGT,
                heap_index_value,
                self.backend.context.i64_type().const_zero(),
                "text_map.bulk.heap.more",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(heap_more, heap_body, sort_header)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(heap_body);
        let root = self
            .backend
            .builder
            .build_int_sub(
                heap_index_value,
                self.backend.context.i64_type().const_int(1, false),
                "text_map.bulk.heap.root",
            )
            .map_err(builder_error)?;
        self.sift_down_text_map(layout, object, root, length, "text_map.bulk.heap.sift")?;
        self.backend
            .builder
            .build_unconditional_branch(heap_header)
            .map_err(builder_error)?;
        let heap_end = self.current_block()?;
        heap_index.add_incoming(&[(&root, heap_end)]);

        self.backend.builder.position_at_end(sort_header);
        let end_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), "text_map.bulk.sort.end")
            .map_err(builder_error)?;
        end_phi.add_incoming(&[(&length, heap_header)]);
        let end = end_phi.as_basic_value().into_int_value();
        let more = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::UGT,
                end,
                self.backend.context.i64_type().const_int(1, false),
                "text_map.bulk.sort.more",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(more, sort_body, done)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(sort_body);
        let next_end = self
            .backend
            .builder
            .build_int_sub(
                end,
                self.backend.context.i64_type().const_int(1, false),
                "text_map.bulk.sort.next_end",
            )
            .map_err(builder_error)?;
        self.swap_text_map_entries(
            layout,
            object,
            self.backend.context.i64_type().const_zero(),
            next_end,
            "text_map.bulk.sort.swap",
        )?;
        self.sift_down_text_map(
            layout,
            object,
            self.backend.context.i64_type().const_zero(),
            next_end,
            "text_map.bulk.sort.sift",
        )?;
        self.backend
            .builder
            .build_unconditional_branch(sort_header)
            .map_err(builder_error)?;
        let sort_end = self.current_block()?;
        end_phi.add_incoming(&[(&next_end, sort_end)]);
        self.backend.builder.position_at_end(done);
        Ok(())
    }

    fn store_text_map_entry(
        &self,
        layout: &TextMapLayout<'ctx>,
        object: PointerValue<'ctx>,
        index: IntValue<'ctx>,
        key: PointerValue<'ctx>,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), CodegenError> {
        let entry = self.text_map_entry_pointer(layout, object, index, "text_map.store.entry")?;
        let key_pointer =
            self.text_map_entry_field_pointer(layout, entry, 0, "text_map.store.key.pointer")?;
        self.backend
            .builder
            .build_store(key_pointer, key)
            .map_err(builder_error)?;
        if self.backend.target_data.get_abi_size(&layout.value) != 0 {
            let value_pointer = self.text_map_entry_field_pointer(
                layout,
                entry,
                1,
                "text_map.store.value.pointer",
            )?;
            self.backend
                .builder
                .build_store(value_pointer, value)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "bulk construction keeps its single allocation, in-place heapsort, duplicate scan, and exact Result CFG in one auditable boundary"
    )]
    fn emit_text_map_construct_entries(
        &self,
        instruction: &Instruction,
        entries: ValueId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result = instruction.results().first().copied().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "TextMap bulk construction has no result")
        })?;
        let result_ty = self
            .source
            .value(result)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "bulk result is missing"))?
            .ty();
        let map_ty = self.sum_variant_field_type(result_ty, 0, 0)?;
        let error_ty = self.sum_variant_field_type(result_ty, 1, 0)?;
        let text_ty = self
            .backend
            .artifact
            .representations()
            .type_id(&Type::Text)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "canonical Text is missing"))?;
        if error_ty != text_ty {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "TextMap bulk construction error payload is not canonical Text",
            ));
        }
        let list_ty = self.list_type_of_value(entries)?;
        let list_layout = self.backend.list_layout(list_ty)?;
        let map_layout = self.backend.text_map_layout(map_ty)?;
        if list_layout.element != BasicTypeEnum::from(map_layout.entry)
            || list_layout.element_stride != map_layout.entry_stride
            || list_layout.element_align != map_layout.entry_align
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "List[(Text, V)] and TextMap[V] entry layouts disagree",
            ));
        }

        let list = self.value(entries)?.into_pointer_value();
        let (length, _) = self.load_list_header(&list_layout, list, "text_map.bulk.list")?;
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new(
                "LlvmBuilderFailed",
                "TextMap bulk construction has no function",
            )
        })?;
        let empty = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.empty");
        let nonempty = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.nonempty");
        let scan_header = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.scan.header");
        let scan_compare = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.scan.compare");
        let scan_advance = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.scan.advance");
        let success = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.success");
        let duplicate = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.duplicate");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "text_map.bulk.merge");
        let is_empty = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                length,
                self.backend.context.i64_type().const_zero(),
                "text_map.bulk.is_empty",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_empty, empty, nonempty)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let empty_map = self.backend.ptr_type.const_null();
        let empty_result = self.emit_sum_construct_values(result_ty, 0, &[empty_map.into()])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(nonempty);
        let object = self.allocate_text_map_bulk(
            map_ty,
            &map_layout,
            length,
            ManagedSafepoint::Instruction(instruction.id()),
            "text_map.bulk.allocate",
        )?;
        let length_pointer =
            self.text_map_field_pointer(&map_layout, object, 0, "text_map.bulk.length.pointer")?;
        self.backend
            .builder
            .build_store(length_pointer, length)
            .map_err(builder_error)?;
        let relocated_list = self.value(entries)?.into_pointer_value();
        let source_data =
            self.list_field_pointer(&list_layout, relocated_list, 2, "text_map.bulk.source")?;
        let destination_data =
            self.text_map_field_pointer(&map_layout, object, 1, "text_map.bulk.destination")?;
        let copy_bytes = self
            .backend
            .builder
            .build_int_mul(
                length,
                self.backend
                    .context
                    .i64_type()
                    .const_int(map_layout.entry_stride, false),
                "text_map.bulk.copy_bytes",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_memcpy(
                destination_data,
                map_layout.entry_align,
                source_data,
                list_layout.element_align,
                copy_bytes,
            )
            .map_err(builder_error)?;
        self.sort_text_map_entries(&map_layout, object, length)?;
        self.backend
            .builder
            .build_unconditional_branch(scan_header)
            .map_err(builder_error)?;
        let sorted = self.current_block()?;

        self.backend.builder.position_at_end(scan_header);
        let index_phi = self
            .backend
            .builder
            .build_phi(self.backend.context.i64_type(), "text_map.bulk.scan.index")
            .map_err(builder_error)?;
        let one = self.backend.context.i64_type().const_int(1, false);
        index_phi.add_incoming(&[(&one, sorted)]);
        let index = index_phi.as_basic_value().into_int_value();
        let remains = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                index,
                length,
                "text_map.bulk.scan.remains",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(remains, scan_compare, success)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(scan_compare);
        let previous_index = self
            .backend
            .builder
            .build_int_sub(index, one, "text_map.bulk.scan.previous")
            .map_err(builder_error)?;
        let previous_key = self.load_text_map_entry_key(
            &map_layout,
            object,
            previous_index,
            "text_map.bulk.scan.previous",
        )?;
        let current_key =
            self.load_text_map_entry_key(&map_layout, object, index, "text_map.bulk.scan.current")?;
        let (equal, _) =
            self.compare_text_keys(previous_key, current_key, "text_map.bulk.scan.compare_key")?;
        self.backend
            .builder
            .build_conditional_branch(equal, duplicate, scan_advance)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(scan_advance);
        let next = self
            .backend
            .builder
            .build_int_add(index, one, "text_map.bulk.scan.next")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unconditional_branch(scan_header)
            .map_err(builder_error)?;
        let advanced = self.current_block()?;
        index_phi.add_incoming(&[(&next, advanced)]);

        self.backend.builder.position_at_end(success);
        let success_result = self.emit_sum_construct_values(result_ty, 0, &[object.into()])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(duplicate);
        let duplicate_result =
            self.emit_sum_construct_values(result_ty, 1, &[current_key.into()])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let result_phi = self
            .backend
            .builder
            .build_phi(self.backend.llvm_type(result_ty)?, "text_map.bulk.result")
            .map_err(builder_error)?;
        result_phi.add_incoming(&[
            (&empty_result, empty),
            (&success_result, success),
            (&duplicate_result, duplicate),
        ]);
        Ok(result_phi.as_basic_value())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "functional TextMap insertion keeps sorted lookup, exact allocation, relocation reloads, and two bounded copies together"
    )]
    fn emit_text_map_insert(
        &self,
        instruction: &Instruction,
        map: ValueId,
        key: ValueId,
        value: ValueId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let result =
            instruction.results().first().copied().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "TextMap insert has no result")
            })?;
        let ty = self.text_map_type_of_value(result)?;
        let layout = self.backend.text_map_layout(ty)?;
        let old_object = self.value(map)?.into_pointer_value();
        let old_length = self.load_text_map_length(&layout, old_object, "text_map.insert.old")?;
        let key_pointer = self.value(key)?.into_pointer_value();
        let (position, found) = self.locate_text_map_key(
            &layout,
            old_object,
            old_length,
            key_pointer,
            "text_map.insert.locate",
        )?;
        let one = self.backend.context.i64_type().const_int(1, false);
        let grown_length = self
            .backend
            .builder
            .build_int_add(old_length, one, "text_map.insert.grown_length")
            .map_err(builder_error)?;
        let new_length = self
            .backend
            .builder
            .build_select(
                found,
                old_length,
                grown_length,
                "text_map.insert.new_length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let object = self.allocate_text_map(
            ty,
            &layout,
            new_length,
            result,
            ManagedSafepoint::Instruction(instruction.id()),
            "text_map.insert",
        )?;
        let length_pointer =
            self.text_map_field_pointer(&layout, object, 0, "text_map.insert.length.pointer")?;
        self.backend
            .builder
            .build_store(length_pointer, new_length)
            .map_err(builder_error)?;

        let relocated_old = self.value(map)?.into_pointer_value();
        let copy_source = self
            .backend
            .builder
            .build_select(
                self.backend
                    .builder
                    .build_is_null(relocated_old, "text_map.insert.old.is_null")
                    .map_err(builder_error)?,
                object,
                relocated_old,
                "text_map.insert.copy_source",
            )
            .map_err(builder_error)?
            .into_pointer_value();
        let source_data =
            self.text_map_field_pointer(&layout, copy_source, 1, "text_map.insert.source")?;
        let destination_data =
            self.text_map_field_pointer(&layout, object, 1, "text_map.insert.destination")?;
        let stride = self
            .backend
            .context
            .i64_type()
            .const_int(layout.entry_stride, false);
        let prefix_bytes = self
            .backend
            .builder
            .build_int_mul(position, stride, "text_map.insert.prefix_bytes")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_memcpy(
                destination_data,
                layout.entry_align,
                source_data,
                layout.entry_align,
                prefix_bytes,
            )
            .map_err(builder_error)?;

        let found_offset = self
            .backend
            .builder
            .build_int_z_extend(
                found,
                self.backend.context.i64_type(),
                "text_map.insert.found",
            )
            .map_err(builder_error)?;
        let suffix_source_index = self
            .backend
            .builder
            .build_int_add(
                position,
                found_offset,
                "text_map.insert.suffix_source_index",
            )
            .map_err(builder_error)?;
        let suffix_destination_index = self
            .backend
            .builder
            .build_int_add(position, one, "text_map.insert.suffix_destination_index")
            .map_err(builder_error)?;
        let suffix_count = self
            .backend
            .builder
            .build_int_sub(
                old_length,
                suffix_source_index,
                "text_map.insert.suffix_count",
            )
            .map_err(builder_error)?;
        let suffix_bytes = self
            .backend
            .builder
            .build_int_mul(suffix_count, stride, "text_map.insert.suffix_bytes")
            .map_err(builder_error)?;
        let suffix_source = self.text_map_entry_pointer(
            &layout,
            copy_source,
            suffix_source_index,
            "text_map.insert.suffix_source",
        )?;
        let suffix_destination = self.text_map_entry_pointer(
            &layout,
            object,
            suffix_destination_index,
            "text_map.insert.suffix_destination",
        )?;
        self.backend
            .builder
            .build_memcpy(
                suffix_destination,
                layout.entry_align,
                suffix_source,
                layout.entry_align,
                suffix_bytes,
            )
            .map_err(builder_error)?;
        self.store_text_map_entry(
            &layout,
            object,
            position,
            self.value(key)?.into_pointer_value(),
            self.value(value)?,
        )?;
        Ok(object)
    }

    fn emit_text_map_length(&self, map: ValueId) -> Result<IntValue<'ctx>, CodegenError> {
        let ty = self.text_map_type_of_value(map)?;
        let layout = self.backend.text_map_layout(ty)?;
        let object = self.value(map)?.into_pointer_value();
        self.load_text_map_length(&layout, object, "text_map.length")
    }

    fn emit_text_map_contains(
        &self,
        map: ValueId,
        key: ValueId,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        let ty = self.text_map_type_of_value(map)?;
        let layout = self.backend.text_map_layout(ty)?;
        let object = self.value(map)?.into_pointer_value();
        let length = self.load_text_map_length(&layout, object, "text_map.contains")?;
        self.locate_text_map_key(
            &layout,
            object,
            length,
            self.value(key)?.into_pointer_value(),
            "text_map.contains.locate",
        )
        .map(|(_, found)| found)
    }

    fn emit_text_map_get(
        &self,
        map: ValueId,
        key: ValueId,
        result_ty: ValueTypeId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let ty = self.text_map_type_of_value(map)?;
        let layout = self.backend.text_map_layout(ty)?;
        let object = self.value(map)?.into_pointer_value();
        let length = self.load_text_map_length(&layout, object, "text_map.get")?;
        let (position, found) = self.locate_text_map_key(
            &layout,
            object,
            length,
            self.value(key)?.into_pointer_value(),
            "text_map.get.locate",
        )?;
        let source = self.current_block()?;
        let function = source
            .get_parent()
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "TextMap.get has no function"))?;
        let some = self
            .backend
            .context
            .append_basic_block(function, "text_map.get.some");
        let none = self
            .backend
            .context
            .append_basic_block(function, "text_map.get.none");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "text_map.get.merge");
        self.backend
            .builder
            .build_conditional_branch(found, some, none)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(none);
        let none_value = self.emit_sum_construct_values(result_ty, 0, &[])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(some);
        let value = if self.backend.target_data.get_abi_size(&layout.value) == 0 {
            match layout.value {
                BasicTypeEnum::ArrayType(ty) => ty.const_zero().into(),
                BasicTypeEnum::FloatType(ty) => ty.const_zero().into(),
                BasicTypeEnum::IntType(ty) => ty.const_zero().into(),
                BasicTypeEnum::PointerType(ty) => ty.const_null().into(),
                BasicTypeEnum::StructType(ty) => ty.const_zero().into(),
                BasicTypeEnum::VectorType(ty) => ty.const_zero().into(),
                BasicTypeEnum::ScalableVectorType(ty) => ty.const_zero().into(),
            }
        } else {
            let entry =
                self.text_map_entry_pointer(&layout, object, position, "text_map.get.entry")?;
            let pointer =
                self.text_map_entry_field_pointer(&layout, entry, 1, "text_map.get.value.pointer")?;
            self.backend
                .builder
                .build_load(layout.value, pointer, "text_map.get.value")
                .map_err(builder_error)?
        };
        let some_value = self.emit_sum_construct_values(result_ty, 1, &[value])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(self.backend.llvm_type(result_ty)?, "text_map.get.result")
            .map_err(builder_error)?;
        phi.add_incoming(&[(&none_value, none), (&some_value, some)]);
        Ok(phi.as_basic_value())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "functional removal keeps its conditional allocation, relocated source, and two exact typed copies in one auditable boundary"
    )]
    fn emit_text_map_remove(
        &self,
        instruction: &Instruction,
        map: ValueId,
        key: ValueId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let result =
            instruction.results().first().copied().ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "TextMap remove has no result")
            })?;
        let ty = self.text_map_type_of_value(result)?;
        let layout = self.backend.text_map_layout(ty)?;
        let old_object = self.value(map)?.into_pointer_value();
        let old_length = self.load_text_map_length(&layout, old_object, "text_map.remove.old")?;
        let (position, found) = self.locate_text_map_key(
            &layout,
            old_object,
            old_length,
            self.value(key)?.into_pointer_value(),
            "text_map.remove.locate",
        )?;
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "TextMap.remove has no function")
        })?;
        let absent = self
            .backend
            .context
            .append_basic_block(function, "text_map.remove.absent");
        let present = self
            .backend
            .context
            .append_basic_block(function, "text_map.remove.present");
        let empty = self
            .backend
            .context
            .append_basic_block(function, "text_map.remove.empty");
        let allocate = self
            .backend
            .context
            .append_basic_block(function, "text_map.remove.allocate");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "text_map.remove.merge");
        self.backend
            .builder
            .build_conditional_branch(found, present, absent)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(absent);
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(present);
        let one = self.backend.context.i64_type().const_int(1, false);
        let new_length = self
            .backend
            .builder
            .build_int_sub(old_length, one, "text_map.remove.new_length")
            .map_err(builder_error)?;
        let becomes_empty = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                new_length,
                self.backend.context.i64_type().const_zero(),
                "text_map.remove.becomes_empty",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(becomes_empty, empty, allocate)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let empty_value = self.backend.ptr_type.const_null();
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(allocate);
        let object = self.allocate_text_map(
            ty,
            &layout,
            new_length,
            result,
            ManagedSafepoint::Instruction(instruction.id()),
            "text_map.remove",
        )?;
        let length_pointer =
            self.text_map_field_pointer(&layout, object, 0, "text_map.remove.length.pointer")?;
        self.backend
            .builder
            .build_store(length_pointer, new_length)
            .map_err(builder_error)?;
        let relocated_old = self.value(map)?.into_pointer_value();
        let source_data =
            self.text_map_field_pointer(&layout, relocated_old, 1, "text_map.remove.source")?;
        let destination_data =
            self.text_map_field_pointer(&layout, object, 1, "text_map.remove.destination")?;
        let stride = self
            .backend
            .context
            .i64_type()
            .const_int(layout.entry_stride, false);
        let prefix_bytes = self
            .backend
            .builder
            .build_int_mul(position, stride, "text_map.remove.prefix_bytes")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_memcpy(
                destination_data,
                layout.entry_align,
                source_data,
                layout.entry_align,
                prefix_bytes,
            )
            .map_err(builder_error)?;

        let suffix_source_index = self
            .backend
            .builder
            .build_int_add(position, one, "text_map.remove.suffix_source_index")
            .map_err(builder_error)?;
        let suffix_count = self
            .backend
            .builder
            .build_int_sub(new_length, position, "text_map.remove.suffix_count")
            .map_err(builder_error)?;
        let suffix_bytes = self
            .backend
            .builder
            .build_int_mul(suffix_count, stride, "text_map.remove.suffix_bytes")
            .map_err(builder_error)?;
        let suffix_source = self.text_map_entry_pointer(
            &layout,
            relocated_old,
            suffix_source_index,
            "text_map.remove.suffix_source",
        )?;
        let suffix_destination = self.text_map_entry_pointer(
            &layout,
            object,
            position,
            "text_map.remove.suffix_destination",
        )?;
        self.backend
            .builder
            .build_memcpy(
                suffix_destination,
                layout.entry_align,
                suffix_source,
                layout.entry_align,
                suffix_bytes,
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;
        let allocated = self.current_block()?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(self.backend.ptr_type, "text_map.remove.result")
            .map_err(builder_error)?;
        phi.add_incoming(&[
            (&old_object, absent),
            (&empty_value, empty),
            (&object, allocated),
        ]);
        Ok(phi.as_basic_value().into_pointer_value())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "bounds checking and exact Option[(Text, V)] construction form one nonallocating compiler-private operation"
    )]
    fn emit_text_map_entry_get(
        &self,
        map: ValueId,
        index: ValueId,
        result_ty: ValueTypeId,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let ty = self.text_map_type_of_value(map)?;
        let layout = self.backend.text_map_layout(ty)?;
        let object = self.value(map)?.into_pointer_value();
        let index = self.int(index)?;
        let source = self.current_block()?;
        let function = source.get_parent().ok_or_else(|| {
            CodegenError::new("LlvmBuilderFailed", "TextMap entry read has no function")
        })?;
        let bounds = self
            .backend
            .context
            .append_basic_block(function, "text_map.entry_get.bounds");
        let some = self
            .backend
            .context
            .append_basic_block(function, "text_map.entry_get.some");
        let none = self
            .backend
            .context
            .append_basic_block(function, "text_map.entry_get.none");
        let merge = self
            .backend
            .context
            .append_basic_block(function, "text_map.entry_get.merge");
        let not_null = self
            .backend
            .builder
            .build_is_not_null(object, "text_map.entry_get.not_null")
            .map_err(builder_error)?;
        let nonnegative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SGE,
                index,
                self.backend.context.i64_type().const_zero(),
                "text_map.entry_get.nonnegative",
            )
            .map_err(builder_error)?;
        let can_check = self
            .backend
            .builder
            .build_and(not_null, nonnegative, "text_map.entry_get.can_check")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(can_check, bounds, none)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(bounds);
        let length_pointer =
            self.text_map_field_pointer(&layout, object, 0, "text_map.entry_get.length.pointer")?;
        let length = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                length_pointer,
                "text_map.entry_get.length",
            )
            .map_err(builder_error)?
            .into_int_value();
        let in_bounds = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                index,
                length,
                "text_map.entry_get.in_bounds",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(in_bounds, some, none)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(none);
        let none_value = self.emit_sum_construct_values(result_ty, 0, &[])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(some);
        let entry =
            self.text_map_entry_pointer(&layout, object, index, "text_map.entry_get.entry")?;
        let value = self
            .backend
            .builder
            .build_load(layout.entry, entry, "text_map.entry_get.value")
            .map_err(builder_error)?;
        let some_value = self.emit_sum_construct_values(result_ty, 1, &[value])?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let phi = self
            .backend
            .builder
            .build_phi(
                self.backend.llvm_type(result_ty)?,
                "text_map.entry_get.result",
            )
            .map_err(builder_error)?;
        phi.add_incoming(&[(&none_value, none), (&some_value, some)]);
        Ok(phi.as_basic_value())
    }

    fn pack_sum_carrier(
        &self,
        payload: inkwell::values::StructValue<'ctx>,
        payload_type: StructType<'ctx>,
        carrier_type: StructType<'ctx>,
        byte_offset: u64,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        self.backend.charge_sum_carrier_emission_work(
            self.backend.target_data.get_abi_size(&payload_type),
        )?;
        if Self::carrier_byte_len(carrier_type)? == 0 {
            return Ok(carrier_type.const_zero().into());
        }
        self.ensure_sum_carrier_byte_order()?;
        let byte_array_type = carrier_type
            .get_field_type_at_index(1)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "sum carrier has no byte array"))?
            .into_array_type();
        let byte_array = self.pack_value_bytes(
            byte_array_type.const_zero(),
            payload.into(),
            payload_type.into(),
            byte_offset,
        )?;
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
        byte_offset: u64,
    ) -> Result<inkwell::values::StructValue<'ctx>, CodegenError> {
        self.backend.charge_sum_carrier_emission_work(
            self.backend.target_data.get_abi_size(&payload_type),
        )?;
        if Self::carrier_byte_len(carrier_type)? == 0 {
            return Ok(payload_type.const_zero());
        }
        self.ensure_sum_carrier_byte_order()?;
        let byte_array = self
            .backend
            .builder
            .build_extract_value(carrier.into_struct_value(), 1, "sum.unpack.carrier.bytes")
            .map_err(builder_error)?
            .into_array_value();
        Ok(self
            .unpack_value_bytes(byte_array, payload_type.into(), byte_offset)?
            .into_struct_value())
    }

    fn ensure_sum_carrier_byte_order(&self) -> Result<(), CodegenError> {
        if self.backend.target_data.get_byte_ordering() != ByteOrdering::LittleEndian {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "direct LCIR sum carriers currently require a little-endian target",
            ));
        }
        Ok(())
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

    fn sum_carrier_byte_index(
        bytes: ArrayValue<'ctx>,
        byte_offset: u64,
    ) -> Result<u32, CodegenError> {
        let index = u32::try_from(byte_offset).map_err(|_| {
            CodegenError::new("ProgramTooLarge", "sum carrier byte offset is too large")
        })?;
        if index >= bytes.get_type().len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "sum payload exceeds its physical carrier",
            ));
        }
        Ok(index)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "all recursively supported LLVM aggregate kinds remain visible in one bounded byte packing routine"
    )]
    fn pack_value_bytes(
        &self,
        bytes: ArrayValue<'ctx>,
        value: BasicValueEnum<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        byte_offset: u64,
    ) -> Result<ArrayValue<'ctx>, CodegenError> {
        match ty {
            BasicTypeEnum::IntType(ty) => {
                let value = value.into_int_value();
                let bit_width = ty.get_bit_width();
                let scalar_bytes = bit_width.div_ceil(8);
                let mut packed = bytes;
                for scalar_index in 0..scalar_bytes {
                    let shifted = if scalar_index == 0 {
                        value
                    } else {
                        self.backend
                            .builder
                            .build_right_shift(
                                value,
                                ty.const_int(
                                    Self::sum_carrier_shift(u64::from(scalar_index))?,
                                    false,
                                ),
                                false,
                                "sum.pack.byte.shift",
                            )
                            .map_err(builder_error)?
                    };
                    let byte = match bit_width.cmp(&8) {
                        std::cmp::Ordering::Less => self
                            .backend
                            .builder
                            .build_int_z_extend(
                                shifted,
                                self.backend.context.i8_type(),
                                "sum.pack.byte.extend",
                            )
                            .map_err(builder_error)?,
                        std::cmp::Ordering::Equal => shifted,
                        std::cmp::Ordering::Greater => self
                            .backend
                            .builder
                            .build_int_truncate(
                                shifted,
                                self.backend.context.i8_type(),
                                "sum.pack.byte",
                            )
                            .map_err(builder_error)?,
                    };
                    let destination = byte_offset
                        .checked_add(u64::from(scalar_index))
                        .ok_or_else(|| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "sum carrier byte offset overflowed",
                            )
                        })?;
                    let destination = Self::sum_carrier_byte_index(packed, destination)?;
                    packed = self
                        .backend
                        .builder
                        .build_insert_value(packed, byte, destination, "sum.pack.carrier.byte")
                        .map_err(builder_error)?
                        .into_array_value();
                }
                Ok(packed)
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
                    .map_err(builder_error)?
                    .into_int_value();
                self.pack_value_bytes(bytes, bits.into(), int_type.into(), byte_offset)
            }
            BasicTypeEnum::StructType(ty) => {
                let value = value.into_struct_value();
                let mut packed = bytes;
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
                    packed = self.pack_value_bytes(packed, field, field_type, offset)?;
                }
                Ok(packed)
            }
            BasicTypeEnum::ArrayType(ty) => {
                let value = value.into_array_value();
                let element_type = ty.get_element_type();
                let stride = self.backend.target_data.get_abi_size(&element_type);
                let mut packed = bytes;
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
                    packed = self.pack_value_bytes(packed, element, element_type, offset)?;
                }
                Ok(packed)
            }
            BasicTypeEnum::PointerType(ty) => {
                let int_type = self.sum_pointer_int_type(ty)?;
                let bits = self
                    .backend
                    .builder
                    .build_ptr_to_int(value.into_pointer_value(), int_type, "sum.pack.pointer")
                    .map_err(builder_error)?;
                self.pack_value_bytes(bytes, bits.into(), int_type.into(), byte_offset)
            }
            BasicTypeEnum::VectorType(_) | BasicTypeEnum::ScalableVectorType(_) => {
                Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "unsupported physical value in a direct LCIR sum carrier",
                ))
            }
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "all recursively supported LLVM aggregate kinds remain visible in one bounded byte unpacking routine"
    )]
    fn unpack_value_bytes(
        &self,
        bytes: ArrayValue<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        byte_offset: u64,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match ty {
            BasicTypeEnum::IntType(ty) => {
                let bit_width = ty.get_bit_width();
                let scalar_bytes = bit_width.div_ceil(8);
                let storage_type = if bit_width < 8 {
                    self.backend.context.i8_type()
                } else {
                    ty
                };
                let mut value = storage_type.const_zero();
                for scalar_index in 0..scalar_bytes {
                    let source = byte_offset
                        .checked_add(u64::from(scalar_index))
                        .ok_or_else(|| {
                            CodegenError::new(
                                "ProgramTooLarge",
                                "sum carrier byte offset overflowed",
                            )
                        })?;
                    let source = Self::sum_carrier_byte_index(bytes, source)?;
                    let byte = self
                        .backend
                        .builder
                        .build_extract_value(bytes, source, "sum.unpack.carrier.byte")
                        .map_err(builder_error)?
                        .into_int_value();
                    let byte = if storage_type.get_bit_width() == 8 {
                        byte
                    } else {
                        self.backend
                            .builder
                            .build_int_z_extend(byte, storage_type, "sum.unpack.byte.extend")
                            .map_err(builder_error)?
                    };
                    let shifted = if scalar_index == 0 {
                        byte
                    } else {
                        self.backend
                            .builder
                            .build_left_shift(
                                byte,
                                storage_type.const_int(
                                    Self::sum_carrier_shift(u64::from(scalar_index))?,
                                    false,
                                ),
                                "sum.unpack.byte.shift",
                            )
                            .map_err(builder_error)?
                    };
                    value = self
                        .backend
                        .builder
                        .build_or(value, shifted, "sum.unpack.byte.merge")
                        .map_err(builder_error)?;
                }
                Ok(if storage_type == ty {
                    value.into()
                } else {
                    self.backend
                        .builder
                        .build_int_truncate(value, ty, "sum.unpack.int")
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
                    .unpack_value_bytes(bytes, int_type.into(), byte_offset)?
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
                    let field = self.unpack_value_bytes(bytes, field_type, offset)?;
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
                    let element = self.unpack_value_bytes(bytes, element_type, offset)?;
                    value = self
                        .backend
                        .builder
                        .build_insert_value(value, element, index, "sum.unpack.element")
                        .map_err(builder_error)?
                        .into_array_value();
                }
                Ok(value.into())
            }
            BasicTypeEnum::PointerType(ty) => {
                let int_type = self.sum_pointer_int_type(ty)?;
                let bits = self
                    .unpack_value_bytes(bytes, int_type.into(), byte_offset)?
                    .into_int_value();
                self.backend
                    .builder
                    .build_int_to_ptr(bits, ty, "sum.unpack.pointer")
                    .map(Into::into)
                    .map_err(builder_error)
            }
            BasicTypeEnum::VectorType(_) | BasicTypeEnum::ScalableVectorType(_) => {
                Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "unsupported physical value in a direct LCIR sum carrier",
                ))
            }
        }
    }

    fn sum_pointer_int_type(
        &self,
        pointer: inkwell::types::PointerType<'ctx>,
    ) -> Result<IntType<'ctx>, CodegenError> {
        let llvm_bits = self.backend.target_data.get_bit_size(&pointer);
        let llvm_bytes = self.backend.target_data.get_abi_size(&pointer);
        let llvm_alignment = u64::from(self.backend.target_data.get_abi_alignment(&pointer));
        let lcir_bits = u64::from(
            self.backend
                .artifact
                .representations()
                .target()
                .pointer_bits(),
        );
        if llvm_bits != lcir_bits
            || llvm_bits != 64
            || llvm_bytes != 8
            || llvm_alignment != TEXT_OBJECT_ALIGNMENT
        {
            return Err(CodegenError::new(
                "LcirTextAbiMismatch",
                format!(
                    "managed LCIR sum pointers require an exact 64-bit, 8-byte, {TEXT_OBJECT_ALIGNMENT}-aligned target layout; got LLVM bits/bytes/alignment {llvm_bits}/{llvm_bytes}/{llvm_alignment} and LCIR bits {lcir_bits}"
                ),
            ));
        }
        Ok(self.backend.context.i64_type())
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
    fn emit_terminator(
        &mut self,
        block: BlockId,
        terminator: &Terminator,
    ) -> Result<(), CodegenError> {
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
            TerminatorKind::SumSwitch { scrutinee, cases }
            | TerminatorKind::SumBorrowSwitch { scrutinee, cases } => {
                self.emit_sum_switch(*scrutinee, cases)
            }
            TerminatorKind::SumZipSwitch {
                left,
                right,
                cases,
                mismatch,
            } => self.emit_sum_zip_switch(*left, *right, cases, mismatch),
            TerminatorKind::DynSwitch { scrutinee, cases } => {
                self.emit_dyn_switch(*scrutinee, cases)
            }
            TerminatorKind::Return(value) => {
                self.emit_return(self.value(*value)?, terminator.writebacks())
            }
            TerminatorKind::TaskSleep {
                milliseconds,
                normal,
                fault,
            } => self.emit_task_sleep(*milliseconds, terminator.origin(), normal, fault),
            TerminatorKind::AwaitTasks {
                state,
                mode,
                tasks,
                normal,
                fault,
                cancel,
            } => self.emit_await_tasks(
                *state,
                *mode,
                tasks,
                terminator.origin(),
                normal,
                fault,
                cancel,
            ),
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
            } => self.emit_invoke(block, *callee, arguments, normal, unwind),
            TerminatorKind::LogWrite {
                level,
                message,
                fields,
                normal,
                fault,
            } => self.emit_log_write(
                *level,
                *message,
                *fields,
                terminator.origin(),
                normal,
                fault,
            ),
            TerminatorKind::StdoutWrite {
                text,
                normal,
                fault,
            } => self.emit_stdout_write(*text, terminator.origin(), normal, fault),
            TerminatorKind::Assert {
                condition,
                metadata,
                success,
                fault,
            } => self.emit_assert(
                self.int(*condition)?,
                metadata,
                terminator.origin(),
                success,
                fault,
            ),
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
            TerminatorKind::TaskCancelled => {
                if self.coroutine.is_none() {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        "task.cancelled reached a non-coroutine emitter",
                    ));
                }
                self.emit_coroutine_step_return(TASK_CANCELLED)
            }
        }
    }

    fn emit_dyn_construct(
        &self,
        instruction: &Instruction,
        variant: u32,
        value: ValueId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let result = instruction.results().first().copied().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "dynamic construction has no result")
        })?;
        let view = self
            .source
            .value(result)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "dynamic result disappeared"))?
            .ty();
        let layout = self.backend.dynamic_candidate_layout(view, variant)?;
        let descriptor = self.backend.dynamic_descriptor(view, variant, &layout)?;
        let output = if let Some(cell) = self.direct_root_cell(result)? {
            cell
        } else {
            self.backend
                .builder
                .build_alloca(self.backend.ptr_type, "dyn.construct.output")
                .map_err(builder_error)?
        };
        self.backend
            .builder
            .build_store(output, self.backend.ptr_type.const_null())
            .map_err(builder_error)?;
        self.publish_root_state(ManagedSafepoint::Instruction(instruction.id()))?;
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_alloc(),
            &[
                descriptor.into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(layout.size, false)
                    .into(),
                output.into(),
            ],
            "dyn.construct.status",
        )?;
        self.backend.require_zero_status(status, "dyn.construct")?;
        let object = self
            .backend
            .builder
            .build_load(self.backend.ptr_type, output, "dyn.construct.object")
            .map_err(builder_error)?
            .into_pointer_value();
        let tag_pointer = self
            .backend
            .builder
            .build_struct_gep(layout.object, object, 0, "dyn.construct.tag.pointer")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(
                tag_pointer,
                self.backend
                    .context
                    .i32_type()
                    .const_int(u64::from(variant), false),
            )
            .map_err(builder_error)?;
        if self.backend.target_data.get_abi_size(&layout.payload) != 0 {
            let payload_pointer = self
                .backend
                .builder
                .build_struct_gep(layout.object, object, 1, "dyn.construct.payload.pointer")
                .map_err(builder_error)?;
            let payload = self.value(value)?;
            self.backend
                .builder
                .build_store(payload_pointer, payload)
                .map_err(builder_error)?;
        }
        Ok(object)
    }

    fn emit_dyn_switch(
        &self,
        scrutinee: ValueId,
        cases: &[loom_codegen_ir::SumCase],
    ) -> Result<(), CodegenError> {
        let view = self
            .source
            .value(scrutinee)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "dynamic scrutinee disappeared"))?
            .ty();
        let object = self.value(scrutinee)?.into_pointer_value();
        let tag = self
            .backend
            .builder
            .build_load(self.backend.context.i32_type(), object, "dyn.switch.tag")
            .map_err(builder_error)?
            .into_int_value();
        let edges = cases
            .iter()
            .map(|case| {
                self.backend
                    .context
                    .append_basic_block(self.function, &format!("dyn.case.{}", case.variant))
            })
            .collect::<Vec<_>>();
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "dyn.switch.invalid");
        let llvm_cases = cases
            .iter()
            .zip(&edges)
            .map(|(case, edge)| {
                (
                    self.backend
                        .context
                        .i32_type()
                        .const_int(u64::from(case.variant), false),
                    *edge,
                )
            })
            .collect::<Vec<_>>();
        self.backend
            .builder
            .build_switch(tag, invalid, &llvm_cases)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(invalid);
        self.backend
            .builder
            .build_unreachable()
            .map_err(builder_error)?;

        for (case, edge) in cases.iter().zip(edges) {
            self.backend.builder.position_at_end(edge);
            let layout = self.backend.dynamic_candidate_layout(view, case.variant)?;
            let candidate = self.backend.dynamic_candidate_type(view, case.variant)?;
            let payload = if self.backend.target_data.get_abi_size(&layout.payload) == 0 {
                self.backend.zero(candidate)?
            } else {
                let pointer = self
                    .backend
                    .builder
                    .build_struct_gep(layout.object, object, 1, "dyn.switch.payload.pointer")
                    .map_err(builder_error)?;
                self.backend
                    .builder
                    .build_load(layout.payload, pointer, "dyn.switch.payload")
                    .map_err(builder_error)?
            };
            let predecessor = self.current_block()?;
            self.add_implicit_incoming(case.block, &[payload], &case.arguments, predecessor)?;
            self.backend
                .builder
                .build_unconditional_branch(self.block(case.block)?)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exact typed-I/O factory edge stages its closed request shape and publishes the runtime-owned leaf Task"
    )]
    fn emit_io_task_create(
        &self,
        instruction: &Instruction,
        operation: IoTaskOperation,
        error_mode: IoTaskErrorMode,
        arguments: &[ValueId],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let layout = self.backend.io_task_layout(instruction.id())?;
        if layout.operation != operation || layout.error_mode != error_mode {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed I/O instruction operation or error mode does not match its descriptor",
            ));
        }
        let invalid_resource = self
            .backend
            .context
            .i64_type()
            .const_int(TYPED_IO_INVALID_RESOURCE_TOKEN, false);
        let mut resource_token = invalid_resource;
        let mut argument_data = self.backend.ptr_type.const_null();
        let mut argument_length = self.backend.context.i64_type().const_zero();
        let mut auxiliary = self.backend.context.i64_type().const_zero();
        let argument = |index: usize| {
            arguments.get(index).copied().ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("typed I/O operation is missing argument {index}"),
                )
            })
        };
        match operation {
            IoTaskOperation::FileOpenRead | IoTaskOperation::FileCreate => {
                let text = self.io_text_pointer(argument(0)?)?;
                (argument_data, argument_length) = self.backend.text_parts(text, "io.path")?;
            }
            IoTaskOperation::FileReadText | IoTaskOperation::SocketReadText => {
                resource_token = self.io_resource_token(argument(0)?)?;
            }
            IoTaskOperation::FileWriteText | IoTaskOperation::SocketWriteText => {
                resource_token = self.io_resource_token(argument(0)?)?;
                let text = self.io_text_pointer(argument(1)?)?;
                (argument_data, argument_length) =
                    self.backend.text_parts(text, "io.write_text")?;
            }
            IoTaskOperation::SocketConnect => {
                let text = self.io_text_pointer(argument(0)?)?;
                (argument_data, argument_length) = self.backend.text_parts(text, "io.host")?;
                auxiliary = self.int(argument(1)?)?;
            }
        }
        let expected_count = match operation {
            IoTaskOperation::FileOpenRead
            | IoTaskOperation::FileCreate
            | IoTaskOperation::FileReadText
            | IoTaskOperation::SocketReadText => 1,
            IoTaskOperation::FileWriteText
            | IoTaskOperation::SocketConnect
            | IoTaskOperation::SocketWriteText => 2,
        };
        if arguments.len() != expected_count {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "typed I/O operation has {} arguments, expected {expected_count}",
                    arguments.len()
                ),
            ));
        }
        let operation = match operation {
            IoTaskOperation::FileOpenRead => TYPED_IO_OPERATION_FILE_OPEN_READ,
            IoTaskOperation::FileCreate => TYPED_IO_OPERATION_FILE_CREATE,
            IoTaskOperation::FileReadText => TYPED_IO_OPERATION_FILE_READ_TEXT,
            IoTaskOperation::FileWriteText => TYPED_IO_OPERATION_FILE_WRITE_TEXT,
            IoTaskOperation::SocketConnect => TYPED_IO_OPERATION_SOCKET_CONNECT,
            IoTaskOperation::SocketReadText => TYPED_IO_OPERATION_SOCKET_READ_TEXT,
            IoTaskOperation::SocketWriteText => TYPED_IO_OPERATION_SOCKET_WRITE_TEXT,
        };
        let request_type = self.backend.typed_io_request_type();
        let byte_view_type = request_type
            .get_field_type_at_index(3)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O byte view is missing"))?
            .into_struct_type();
        let byte_view = self
            .backend
            .builder
            .build_insert_value(
                byte_view_type.const_zero(),
                argument_data,
                0,
                "io.request.argument.data",
            )
            .map_err(builder_error)?
            .into_struct_value();
        let byte_view = self
            .backend
            .builder
            .build_insert_value(byte_view, argument_length, 1, "io.request.argument.length")
            .map_err(builder_error)?
            .into_struct_value();
        let mut request = request_type.const_zero();
        let fields: [(u32, BasicValueEnum<'ctx>); 5] = [
            (
                0,
                self.backend
                    .context
                    .i32_type()
                    .const_int(u64::from(TYPED_IO_ABI_VERSION), false)
                    .into(),
            ),
            (
                1,
                self.backend
                    .context
                    .i32_type()
                    .const_int(u64::from(operation), false)
                    .into(),
            ),
            (2, resource_token.into()),
            (3, byte_view.into()),
            (4, auxiliary.into()),
        ];
        for (field, value) in fields {
            request = self
                .backend
                .builder
                .build_insert_value(request, value, field, "io.request.field")
                .map_err(builder_error)?
                .into_struct_value();
        }
        let request_pointer = self
            .backend
            .builder
            .build_alloca(request_type, "io.request")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(request_pointer, request)
            .map_err(builder_error)?;
        let task = call_pointer(
            &self.backend.builder,
            self.backend.typed_io_task_create(),
            &[
                self.executor_context()?.into(),
                layout.descriptor.into(),
                request_pointer.into(),
            ],
            "io.task.create",
        )?;
        self.backend.require_nonnull(task, "io.task.create")?;
        Ok(task)
    }

    fn io_text_pointer(&self, value: ValueId) -> Result<PointerValue<'ctx>, CodegenError> {
        let ty = self
            .source
            .value(value)
            .map(loom_codegen_ir::Value::ty)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O Text value is missing"))?;
        let semantic = self
            .backend
            .artifact
            .representations()
            .value_type(ty)
            .map(loom_codegen_ir::ValueType::semantic)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed I/O Text type is missing"))?;
        if semantic == &Type::Text {
            return Ok(self.value(value)?.into_pointer_value());
        }
        let product = self.value(value)?.into_struct_value();
        Ok(self
            .backend
            .builder
            .build_extract_value(product, 0, "io.path.text")
            .map_err(builder_error)?
            .into_pointer_value())
    }

    fn io_resource_token(&self, value: ValueId) -> Result<IntValue<'ctx>, CodegenError> {
        Ok(self
            .backend
            .builder
            .build_extract_value(
                self.value(value)?.into_struct_value(),
                0,
                "io.resource.token",
            )
            .map_err(builder_error)?
            .into_int_value())
    }

    fn create_task_join_frame(
        &self,
        layout: &TaskJoinLayout<'ctx>,
        executor: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(PointerValue<'ctx>, PointerValue<'ctx>), CodegenError> {
        let composite = call_pointer(
            &self.backend.builder,
            self.backend.typed_task_create(),
            &[executor.into(), layout.descriptor.into()],
            &format!("{name}.create"),
        )?;
        self.backend
            .require_nonnull(composite, &format!("{name}.create"))?;
        let frame = call_pointer(
            &self.backend.builder,
            self.backend.typed_task_frame(),
            &[composite.into()],
            &format!("{name}.frame"),
        )?;
        self.backend
            .require_nonnull(frame, &format!("{name}.frame"))?;
        let state = self
            .backend
            .builder
            .build_struct_gep(layout.frame, frame, 0, &format!("{name}.frame.state"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(state, self.backend.context.i64_type().const_zero())
            .map_err(builder_error)?;
        Ok((composite, frame))
    }

    fn initialize_task_join(
        &self,
        composite: PointerValue<'ctx>,
        name: &str,
    ) -> Result<(), CodegenError> {
        let initialized = call_int(
            &self.backend.builder,
            self.backend.typed_task_initialize(),
            &[
                composite.into(),
                self.backend.context.i64_type().const_zero().into(),
            ],
            name,
        )?;
        self.backend.require_zero_status(initialized, name)
    }

    fn finish_task_join_publish(
        &self,
        executor: PointerValue<'ctx>,
        composite: PointerValue<'ctx>,
        status: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let success = self
            .backend
            .context
            .append_basic_block(self.function, &format!("{name}.ok"));
        let failure = self
            .backend
            .context
            .append_basic_block(self.function, &format!("{name}.failed"));
        let ok = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.backend.context.i32_type().const_zero(),
                &format!("{name}.status.ok"),
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(ok, success, failure)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(failure);
        let _aborted = call_int(
            &self.backend.builder,
            self.backend.typed_task_abort_unpublished(),
            &[executor.into(), composite.into()],
            &format!("{name}.abort_unpublished"),
        )?;
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.backend.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.backend
            .builder
            .build_call(trap, &[], &format!("{name}.trap"))
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unreachable()
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(success);
        Ok(composite)
    }

    fn emit_task_join(
        &self,
        instruction: &Instruction,
        mode: AwaitMode,
        tasks: &[ValueId],
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let executor = self.executor_context()?;
        let layout = self.backend.task_join_layout(instruction.id())?;
        let TaskJoinInputs::Fixed { child_fields } = &layout.inputs else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "typed fixed Task join {} resolved to a runtime-width layout",
                    instruction.id()
                ),
            ));
        };
        if layout.mode != mode
            || tasks.is_empty()
            || tasks.len() != child_fields.len()
            || tasks.len() != layout.output_types.len()
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "typed Task join {} disagrees with its generated shape",
                    instruction.id()
                ),
            ));
        }
        let children = tasks
            .iter()
            .copied()
            .map(|task| self.value(task).map(BasicValueEnum::into_pointer_value))
            .collect::<Result<Vec<_>, _>>()?;
        let (composite, frame) = self.create_task_join_frame(layout, executor, "task.join")?;
        for ((child, field), index) in children
            .iter()
            .copied()
            .zip(child_fields.iter().copied())
            .zip(0_u32..)
        {
            let pointer = self
                .backend
                .builder
                .build_struct_gep(
                    layout.frame,
                    frame,
                    field,
                    &format!("task.join.frame.child.{index}"),
                )
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(pointer, child)
                .map_err(builder_error)?;
        }
        self.initialize_task_join(composite, "task.join.initialize")?;

        let count = self
            .backend
            .context
            .i64_type()
            .const_int(children.len() as u64, false);
        let child_array = self
            .backend
            .builder
            .build_array_alloca(self.backend.ptr_type, count, "task.join.children")
            .map_err(builder_error)?;
        for (index, child) in children.iter().copied().enumerate() {
            let pointer = self.task_pointer_array_element(
                child_array,
                u64::try_from(index).map_err(|_| {
                    CodegenError::new("ProgramTooLarge", "typed Task join has too many children")
                })?,
                &format!("task.join.children.{index}"),
            )?;
            self.backend
                .builder
                .build_store(pointer, child)
                .map_err(builder_error)?;
        }
        let published = call_int(
            &self.backend.builder,
            self.backend.typed_task_publish_adopting(),
            &[
                executor.into(),
                composite.into(),
                child_array.into(),
                count.into(),
            ],
            "task.join.publish_adopting",
        )?;
        self.finish_task_join_publish(executor, composite, published, "task.join.publish_adopting")
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one runtime-width constructor initializes the exact rooted frame and selects ordinary versus directly adopting publication atomically"
    )]
    fn emit_task_join_list(
        &self,
        instruction: &Instruction,
        mode: AwaitMode,
        tasks: ValueId,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let executor = self.executor_context()?;
        let layout = self.backend.task_join_layout(instruction.id())?;
        let TaskJoinInputs::Dynamic {
            source_field,
            source_type,
            source_layout,
            output_type,
        } = &layout.inputs
        else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "typed Task join List {} resolved to a fixed-width layout",
                    instruction.id()
                ),
            ));
        };
        let actual_source_type = self
            .source
            .value(tasks)
            .map(loom_codegen_ir::Value::ty)
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("typed Task join List source {tasks} is missing"),
                )
            })?;
        if layout.mode != mode
            || actual_source_type != *source_type
            || layout.output_types.as_slice() != [*output_type]
            || !source_layout.pointer_offsets.is_empty()
            || source_layout.element != BasicTypeEnum::from(self.backend.ptr_type)
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!(
                    "typed Task join List {} disagrees with its exact generated shape",
                    instruction.id()
                ),
            ));
        }
        let source = self.value(tasks)?.into_pointer_value();
        let (composite, frame) =
            self.create_task_join_frame(layout, executor, "task.join.dynamic")?;
        let source_pointer = self
            .backend
            .builder
            .build_struct_gep(
                layout.frame,
                frame,
                *source_field,
                "task.join.dynamic.frame.source",
            )
            .map_err(builder_error)?;
        // The managed source is installed before the task can be published;
        // root state zero makes this frame cell authoritative to moving GC.
        self.backend
            .builder
            .build_store(source_pointer, source)
            .map_err(builder_error)?;
        let result_pointer = self
            .backend
            .builder
            .build_struct_gep(
                layout.frame,
                frame,
                layout.result_field,
                "task.join.dynamic.frame.result",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(result_pointer, self.backend.zero(layout.result_type)?)
            .map_err(builder_error)?;
        self.initialize_task_join(composite, "task.join.dynamic.initialize")?;

        let length = self.backend.load_dynamic_task_join_length(
            layout,
            frame,
            "task.join.dynamic.publish.source",
        )?;
        let empty = self
            .backend
            .context
            .append_basic_block(self.function, "task.join.dynamic.publish.empty");
        let nonempty = self
            .backend
            .context
            .append_basic_block(self.function, "task.join.dynamic.publish.nonempty");
        let merge = self
            .backend
            .context
            .append_basic_block(self.function, "task.join.dynamic.publish.merge");
        let is_empty = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                length,
                self.backend.context.i64_type().const_zero(),
                "task.join.dynamic.publish.is_empty",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(is_empty, empty, nonempty)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(empty);
        let empty_status = call_int(
            &self.backend.builder,
            self.backend.typed_task_publish(),
            &[executor.into(), composite.into()],
            "task.join.dynamic.publish",
        )?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(nonempty);
        // Do not copy the runtime-width child row to the native stack. The
        // List layout is exactly a contiguous array of stable LoomTask*.
        let relocated_source = self.backend.dynamic_task_join_source(
            layout,
            frame,
            "task.join.dynamic.publish.source.reload",
        )?;
        let child_data = self.backend.task_join_list_field(
            source_layout,
            relocated_source,
            2,
            "task.join.dynamic.publish.children",
        )?;
        let adopting_status = call_int(
            &self.backend.builder,
            self.backend.typed_task_publish_adopting(),
            &[
                executor.into(),
                composite.into(),
                child_data.into(),
                length.into(),
            ],
            "task.join.dynamic.publish_adopting",
        )?;
        self.backend
            .builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(merge);
        let published = self
            .backend
            .builder
            .build_phi(
                self.backend.context.i32_type(),
                "task.join.dynamic.publish.status",
            )
            .map_err(builder_error)?;
        published.add_incoming(&[(&empty_status, empty), (&adopting_status, nonempty)]);
        self.finish_task_join_publish(
            executor,
            composite,
            published.as_basic_value().into_int_value(),
            "task.join.dynamic.publish",
        )
    }

    #[expect(
        unsafe_code,
        reason = "Inkwell requires an audited pointee/index proof for the temporary contiguous child-pointer array"
    )]
    fn task_pointer_array_element(
        &self,
        array: PointerValue<'ctx>,
        index: u64,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        // SAFETY: `array` is the base returned by `build_array_alloca(ptr, N)`
        // in `emit_task_join`, and every caller proves `index < N` by
        // iterating the same checked nonempty child slice.
        unsafe {
            self.backend
                .builder
                .build_gep(
                    self.backend.ptr_type,
                    array,
                    &[self.backend.context.i64_type().const_int(index, false)],
                    name,
                )
                .map_err(builder_error)
        }
    }

    #[expect(
        clippy::too_many_arguments,
        clippy::too_many_lines,
        reason = "one await edge atomically stores its child and exact live row, registers the structured join, publishes the frame state, and returns pending"
    )]
    fn emit_await_tasks(
        &self,
        state: u32,
        mode: AwaitMode,
        tasks: &[ValueId],
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
        cancel: &BlockTarget,
    ) -> Result<(), CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new("LlvmAbiDefect", "await terminator has no coroutine frame")
        })?;
        let suspension = coroutine
            .layout
            .suspensions
            .iter()
            .find(|suspension| suspension.state == state)
            .cloned()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("await state {state} has no physical frame row"),
                )
            })?;
        let plan_row = self
            .source
            .coroutine()
            .and_then(|plan| {
                plan.suspensions()
                    .iter()
                    .find(|suspension| suspension.state() == state)
            })
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("await state {state} has no checked coroutine row"),
                )
            })?;
        if suspension.child_fields.len() != tasks.len()
            || suspension.child_fields.len() != plan_row.awaited().len()
            || suspension.live_fields.len() != normal.arguments.len()
            || plan_row.mode() != mode
            || normal.arguments.as_ref() != fault.arguments.as_ref()
            || normal.arguments.as_ref() != cancel.arguments.as_ref()
        {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("await state {state} disagrees with its physical frame row"),
            ));
        }

        let children = tasks
            .iter()
            .copied()
            .map(|task| self.value(task).map(BasicValueEnum::into_pointer_value))
            .collect::<Result<Vec<_>, _>>()?;
        for ((child, field), index) in children
            .iter()
            .copied()
            .zip(suspension.child_fields.iter().copied())
            .zip(0_u32..)
        {
            let child_pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    &format!("task.await.child.{index}.pointer"),
                )
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(child_pointer, child)
                .map_err(builder_error)?;
        }
        for ((argument, field), index) in normal
            .arguments
            .iter()
            .copied()
            .zip(suspension.live_fields.iter().copied())
            .zip(0_u32..)
        {
            let pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    &format!("task.await.live.{index}.pointer"),
                )
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(pointer, self.value(argument)?)
                .map_err(builder_error)?;
        }

        let runtime_mode = match mode {
            AwaitMode::All => TASK_JOIN_ALL,
            AwaitMode::Settled => TASK_JOIN_SETTLED,
            AwaitMode::Any => TASK_JOIN_ANY,
            AwaitMode::Race => TASK_JOIN_RACE,
        };
        let prepared = call_int(
            &self.backend.builder,
            self.backend.task_prepare_join(),
            &[
                coroutine.executor.into(),
                coroutine.task.into(),
                self.backend
                    .context
                    .i32_type()
                    .const_int(u64::from(runtime_mode), false)
                    .into(),
            ],
            "task.await.prepare",
        )?;
        self.backend
            .require_zero_status(prepared, "task.await.prepare")?;
        for (index, child) in children.iter().copied().enumerate() {
            let name = format!("task.await.add_child.{index}");
            let added = call_int(
                &self.backend.builder,
                self.backend.task_add_join_child(),
                &[
                    coroutine.executor.into(),
                    coroutine.task.into(),
                    child.into(),
                ],
                &name,
            )?;
            self.backend.require_zero_status(added, &name)?;
        }

        let state_pointer = self
            .backend
            .builder
            .build_struct_gep(
                coroutine.layout.frame,
                coroutine.frame,
                0,
                "task.await.state.pointer",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_store(
                state_pointer,
                self.backend
                    .context
                    .i64_type()
                    .const_int(u64::from(state), false),
            )
            .map_err(builder_error)?;
        let rooted = call_int(
            &self.backend.builder,
            self.backend.typed_task_set_root_state(),
            &[
                coroutine.task.into(),
                self.backend
                    .context
                    .i64_type()
                    .const_int(u64::from(state), false)
                    .into(),
            ],
            "task.await.root_state",
        )?;
        self.backend
            .require_zero_status(rooted, "task.await.root_state")?;
        let suspended = call_int(
            &self.backend.builder,
            self.backend.task_suspend_join(),
            &[coroutine.executor.into(), coroutine.task.into()],
            "task.await.suspend",
        )?;
        let pending = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.return.pending");
        let ready = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.immediate.ready");
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "task.await.invalid.suspend");
        self.backend
            .builder
            .build_switch(
                suspended,
                invalid,
                &[
                    (self.backend.context.i32_type().const_zero(), ready),
                    (self.backend.context.i32_type().const_int(1, false), pending),
                ],
            )
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(pending);
        self.emit_coroutine_step_return(TASK_PENDING)?;
        self.backend.builder.position_at_end(invalid);
        self.emit_coroutine_step_return(TASK_FAULTED)?;
        self.backend.builder.position_at_end(ready);
        self.emit_coroutine_resume_state(plan_row, &suspension, normal, fault, cancel, origin)
    }

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

        let (tag, carrier) = self.sum_switch_value_parts(value, &layout, "sum.switch")?;
        match layout.tag {
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
            }
            SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                self.emit_sum_tag_switch(
                    tag.ok_or_else(|| {
                        CodegenError::new("LlvmAbiDefect", "tagged sum switch has no tag")
                    })?,
                    cases,
                    &edges,
                    "sum.switch",
                )?;
            }
        }

        for (case, edge) in cases.iter().zip(edges) {
            self.backend.builder.position_at_end(edge);
            let implicit = self.sum_switch_payload_fields(
                ty,
                value,
                carrier,
                &layout,
                case.variant,
                "sum.switch",
            )?;
            let predecessor = self.current_block()?;
            self.add_implicit_incoming(case.block, &implicit, &case.arguments, predecessor)?;
            self.backend
                .builder
                .build_unconditional_branch(self.block(case.block)?)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "paired sum dispatch keeps the single tag comparison, selected payload decoding, and phi construction together"
    )]
    fn emit_sum_zip_switch(
        &self,
        left: ValueId,
        right: ValueId,
        cases: &[loom_codegen_ir::SumCase],
        mismatch: &BlockTarget,
    ) -> Result<(), CodegenError> {
        let left_ty = self
            .source
            .value(left)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", format!("missing left sum {left}")))?
            .ty();
        let right_ty = self
            .source
            .value(right)
            .ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", format!("missing right sum {right}"))
            })?
            .ty();
        if left_ty != right_ty {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                format!("sum zip switch operands have different types {left_ty} and {right_ty}"),
            ));
        }

        let layout = self.backend.sum_layout(left_ty)?;
        let left_value = self.value(left)?;
        let right_value = self.value(right)?;
        let (left_tag, left_carrier) =
            self.sum_switch_value_parts(left_value, &layout, "sum.zip.left")?;
        let (right_tag, right_carrier) =
            self.sum_switch_value_parts(right_value, &layout, "sum.zip.right")?;
        let edges = cases
            .iter()
            .map(|case| {
                self.backend
                    .context
                    .append_basic_block(self.function, &format!("sum.zip.case.{}", case.variant))
            })
            .collect::<Vec<_>>();

        match layout.tag {
            SumTagRepr::Tagless => {
                let [edge] = edges.as_slice() else {
                    return Err(CodegenError::new(
                        "LlvmAbiDefect",
                        format!("tagless sum zip switch for {left_ty} does not have one case"),
                    ));
                };
                self.backend
                    .builder
                    .build_unconditional_branch(*edge)
                    .map_err(builder_error)?;
            }
            SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                let left_tag = left_tag.ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "tagged sum zip switch has no left tag")
                })?;
                let right_tag = right_tag.ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "tagged sum zip switch has no right tag")
                })?;
                let matching = self
                    .backend
                    .context
                    .append_basic_block(self.function, "sum.zip.matching");
                let mismatch_edge = self
                    .backend
                    .context
                    .append_basic_block(self.function, "sum.zip.mismatch");
                let tags_equal = self
                    .backend
                    .builder
                    .build_int_compare(IntPredicate::EQ, left_tag, right_tag, "sum.zip.tags.equal")
                    .map_err(builder_error)?;
                self.backend
                    .builder
                    .build_conditional_branch(tags_equal, matching, mismatch_edge)
                    .map_err(builder_error)?;

                self.backend.builder.position_at_end(mismatch_edge);
                self.branch(mismatch)?;

                self.backend.builder.position_at_end(matching);
                self.emit_sum_tag_switch(left_tag, cases, &edges, "sum.zip")?;
            }
        }

        for (case, edge) in cases.iter().zip(edges) {
            self.backend.builder.position_at_end(edge);
            let mut implicit = self.sum_switch_payload_fields(
                left_ty,
                left_value,
                left_carrier,
                &layout,
                case.variant,
                "sum.zip.left",
            )?;
            implicit.extend(self.sum_switch_payload_fields(
                right_ty,
                right_value,
                right_carrier,
                &layout,
                case.variant,
                "sum.zip.right",
            )?);
            let predecessor = self.current_block()?;
            self.add_implicit_incoming(case.block, &implicit, &case.arguments, predecessor)?;
            self.backend
                .builder
                .build_unconditional_branch(self.block(case.block)?)
                .map_err(builder_error)?;
        }
        Ok(())
    }

    fn sum_switch_value_parts(
        &self,
        value: BasicValueEnum<'ctx>,
        layout: &SumLayout<'ctx>,
        name: &str,
    ) -> Result<(Option<IntValue<'ctx>>, Option<BasicValueEnum<'ctx>>), CodegenError> {
        match layout.tag {
            SumTagRepr::Tagless => Ok((None, None)),
            SumTagRepr::I8 | SumTagRepr::I16 | SumTagRepr::I32 => {
                if layout.carrier.is_some() {
                    let aggregate = value.into_struct_value();
                    let tag = self
                        .backend
                        .builder
                        .build_extract_value(aggregate, 0, &format!("{name}.tag"))
                        .map_err(builder_error)?
                        .into_int_value();
                    let carrier = self
                        .backend
                        .builder
                        .build_extract_value(aggregate, 1, &format!("{name}.carrier"))
                        .map_err(builder_error)?;
                    Ok((Some(tag), Some(carrier)))
                } else {
                    Ok((Some(value.into_int_value()), None))
                }
            }
        }
    }

    fn emit_sum_tag_switch(
        &self,
        tag: IntValue<'ctx>,
        cases: &[loom_codegen_ir::SumCase],
        edges: &[BasicBlock<'ctx>],
        name: &str,
    ) -> Result<(), CodegenError> {
        if cases.len() != edges.len() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "sum switch case and edge counts differ",
            ));
        }
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, &format!("{name}.invalid"));
        let llvm_cases = cases
            .iter()
            .zip(edges)
            .map(|(case, edge)| {
                (
                    tag.get_type().const_int(u64::from(case.variant), false),
                    *edge,
                )
            })
            .collect::<Vec<_>>();
        self.backend
            .builder
            .build_switch(tag, invalid, &llvm_cases)
            .map_err(builder_error)?;
        self.backend.builder.position_at_end(invalid);
        self.backend
            .builder
            .build_unreachable()
            .map_err(builder_error)?;
        Ok(())
    }

    fn sum_switch_payload_fields(
        &self,
        ty: ValueTypeId,
        value: BasicValueEnum<'ctx>,
        carrier: Option<BasicValueEnum<'ctx>>,
        layout: &SumLayout<'ctx>,
        variant: u32,
        name: &str,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CodegenError> {
        let variant_index = usize::try_from(variant)
            .map_err(|_| CodegenError::new("ProgramTooLarge", "sum case variant is too wide"))?;
        let payload_type = layout.payloads.get(variant_index).copied().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("sum type {ty} has no case variant {variant}"),
            )
        })?;
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
                        payload_type,
                        layout.payload_byte_offset(variant_index)?,
                    )?
                } else {
                    payload_type.const_zero()
                }
            }
        };
        (0..payload_type.count_fields())
            .map(|field| {
                self.backend
                    .builder
                    .build_extract_value(payload, field, &format!("{name}.field"))
                    .map_err(builder_error)
            })
            .collect()
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

    fn emit_task_sleep(
        &mut self,
        milliseconds: ValueId,
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let executor = self.executor_context()?;
        let milliseconds = self.int(milliseconds)?;
        let check_duration = self
            .backend
            .context
            .append_basic_block(self.function, "task.sleep.check.duration");
        let invalid_duration = self
            .backend
            .context
            .append_basic_block(self.function, "task.sleep.invalid.duration");
        let check_deadline = self
            .backend
            .context
            .append_basic_block(self.function, "task.sleep.check.deadline");
        let overflow = self
            .backend
            .context
            .append_basic_block(self.function, "task.sleep.overflow");
        let create = self
            .backend
            .context
            .append_basic_block(self.function, "task.sleep.create");
        let negative = self
            .backend
            .builder
            .build_int_compare(
                IntPredicate::SLT,
                milliseconds,
                self.backend.context.i64_type().const_zero(),
                "task.sleep.duration.negative",
            )
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_conditional_branch(negative, invalid_duration, check_duration)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(invalid_duration);
        self.emit_source_fault(FaultCode::InvalidSleepDuration, origin)?;
        self.unwind_branch(fault)?;

        self.backend.builder.position_at_end(check_duration);
        let million = self.backend.context.i64_type().const_int(1_000_000, false);
        let (nanoseconds, duration_overflow) =
            self.checked_intrinsic("llvm.smul.with.overflow", milliseconds, million)?;
        self.backend
            .builder
            .build_conditional_branch(duration_overflow, overflow, check_deadline)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(check_deadline);
        let now = call_int(
            &self.backend.builder,
            self.backend.wait_now_ns(),
            &[],
            "task.sleep.now",
        )?;
        let (deadline, deadline_overflow) =
            self.checked_intrinsic("llvm.uadd.with.overflow", now, nanoseconds)?;
        self.backend
            .builder
            .build_conditional_branch(deadline_overflow, overflow, create)
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(overflow);
        self.emit_source_fault(FaultCode::SleepDurationOverflow, origin)?;
        self.unwind_branch(fault)?;

        self.backend.builder.position_at_end(create);
        let task = call_pointer(
            &self.backend.builder,
            self.backend.typed_timer_task_create(),
            &[executor.into(), deadline.into()],
            "task.sleep.create",
        )?;
        self.backend.require_nonnull(task, "task.sleep.create")?;
        self.result_branch(normal, task.into())
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
        block: BlockId,
        callee: InstanceId,
        arguments: &[ValueId],
        normal: &ResultTarget,
        unwind: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let callee_source = self.backend.artifact.function(callee).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                "invoked LCIR callee is missing from its artifact",
            )
        })?;
        if callee_source.effects().contains(Effects::MAY_COLLECT) {
            self.publish_root_state(ManagedSafepoint::Terminator(block))?;
        }
        let arguments = self.call_arguments(arguments, callee_source.effects())?;
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
        metadata: &FaultMetadata,
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
        match metadata {
            FaultMetadata::Runtime(code) => self.emit_source_fault(*code, origin)?,
            FaultMetadata::Contract(metadata) => self.emit_contract_fault(metadata)?,
        }
        self.unwind_branch(fault)
    }

    fn emit_resource_close(
        &mut self,
        instruction: InstructionId,
        kind: ResourceKind,
        resource: ValueId,
    ) -> Result<Vec<BasicValueEnum<'ctx>>, CodegenError> {
        let resource = self.value(resource)?.into_struct_value();
        let token = self
            .backend
            .builder
            .build_extract_value(resource, 0, "resource.close.token")
            .map_err(builder_error)?
            .into_int_value();
        let token_cell = self
            .resource_close_token_cells
            .get(instruction.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!(
                        "typed resource cleanup instruction {instruction} has no entry scratch cell"
                    ),
                )
            })?;
        self.backend
            .builder
            .build_store(token_cell, token)
            .map_err(builder_error)?;
        let executor = self.executor_context()?;
        let kind = match kind {
            ResourceKind::File => TYPED_RESOURCE_KIND_FILE,
            ResourceKind::Socket => TYPED_RESOURCE_KIND_SOCKET,
        };
        let status = call_int(
            &self.backend.builder,
            self.backend.typed_resource_close(),
            &[
                executor.into(),
                self.backend
                    .context
                    .i32_type()
                    .const_int(u64::from(kind), false)
                    .into(),
                token_cell.into(),
            ],
            "resource.close.status",
        )?;
        self.backend.require_exact_status(
            status,
            TYPED_RESOURCE_CLOSE_OK.cast_unsigned(),
            "resource.close",
        )?;
        let closed_token = self
            .backend
            .builder
            .build_load(
                self.backend.context.i64_type(),
                token_cell,
                "resource.close.writeback.token",
            )
            .map_err(builder_error)?;
        let closed_resource = self
            .backend
            .builder
            .build_insert_value(resource, closed_token, 0, "resource.close.writeback")
            .map_err(builder_error)?
            .into_struct_value();
        Ok(vec![
            self.backend.unit_type.const_zero().into(),
            closed_resource.into(),
        ])
    }

    fn emit_log_write(
        &mut self,
        level: ValueId,
        message: ValueId,
        fields: ValueId,
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let level_ty = self
            .source
            .value(level)
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "typed log level is missing"))?
            .ty();
        let level_layout = self.backend.sum_layout(level_ty)?;
        if level_layout.tag != SumTagRepr::I8 || level_layout.carrier.is_some() {
            return Err(CodegenError::new(
                "LcirTypedLogAbiMismatch",
                format!("typed log level {level_ty} is not a payload-free i8 sum"),
            ));
        }
        let level = self.value(level)?.into_int_value();
        let level = self
            .backend
            .builder
            .build_int_z_extend(level, self.backend.context.i32_type(), "log.level")
            .map_err(builder_error)?;
        let message = self.value(message)?.into_pointer_value();
        let (fields, field_count) = self.typed_log_fields(fields)?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_log_write_typed(),
            &[
                level.into(),
                message.into(),
                fields.into(),
                field_count.into(),
            ],
            "log.write",
        )?;

        let succeeded = self
            .backend
            .context
            .append_basic_block(self.function, "log.write.succeeded");
        let write_failed = self
            .backend
            .context
            .append_basic_block(self.function, "log.write.failed");
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "log.write.invalid_status");
        let status_type = self.backend.context.i32_type();
        let ok = status_type.const_int(
            u64::try_from(TYPED_LOG_OK).expect("typed log success status is non-negative"),
            false,
        );
        let failed = status_type.const_int(
            u64::try_from(TYPED_LOG_WRITE_FAILED)
                .expect("typed log write-failed status is non-negative"),
            false,
        );
        self.backend
            .builder
            .build_switch(status, invalid, &[(ok, succeeded), (failed, write_failed)])
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(invalid);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.backend.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.backend
            .builder
            .build_call(trap, &[], "log.write.invalid_status.trap")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unreachable()
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(write_failed);
        self.emit_source_fault(FaultCode::LogWrite, origin)?;
        self.unwind_branch(fault)?;

        self.backend.builder.position_at_end(succeeded);
        self.result_branch(normal, self.backend.unit_type.const_zero().into())
    }

    fn emit_stdout_write(
        &mut self,
        text: ValueId,
        origin: Origin,
        normal: &ResultTarget,
        fault: &UnwindTarget,
    ) -> Result<(), CodegenError> {
        let text = self.value(text)?.into_pointer_value();
        let (data, length) = self.backend.text_parts(text, "stdout.write.text")?;
        let status = call_int(
            &self.backend.builder,
            self.backend.runtime_stdout_write(),
            &[data.into(), length.into()],
            "stdout.write",
        )?;

        let succeeded = self
            .backend
            .context
            .append_basic_block(self.function, "stdout.write.succeeded");
        let write_failed = self
            .backend
            .context
            .append_basic_block(self.function, "stdout.write.failed");
        let invalid = self
            .backend
            .context
            .append_basic_block(self.function, "stdout.write.invalid_status");
        let status_type = self.backend.context.i32_type();
        let ok = status_type.const_int(
            u64::try_from(STDOUT_WRITE_OK).expect("stdout success status is non-negative"),
            false,
        );
        let failed = status_type.const_int(
            u64::try_from(STDOUT_WRITE_FAILED).expect("stdout failure status is non-negative"),
            false,
        );
        self.backend
            .builder
            .build_switch(status, invalid, &[(ok, succeeded), (failed, write_failed)])
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(invalid);
        let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
            .and_then(|intrinsic| intrinsic.get_declaration(&self.backend.module, &[]))
            .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "missing llvm.trap"))?;
        self.backend
            .builder
            .build_call(trap, &[], "stdout.write.invalid_status.trap")
            .map_err(builder_error)?;
        self.backend
            .builder
            .build_unreachable()
            .map_err(builder_error)?;

        self.backend.builder.position_at_end(write_failed);
        self.emit_source_fault(FaultCode::StdoutWrite, origin)?;
        self.unwind_branch(fault)?;

        self.backend.builder.position_at_end(succeeded);
        self.result_branch(normal, self.backend.unit_type.const_zero().into())
    }

    fn emit_source_fault(&self, code: FaultCode, origin: Origin) -> Result<(), CodegenError> {
        self.emit_fault(FaultEmission::Runtime { code, origin })
    }

    fn emit_contract_fault(&self, metadata: &ContractFaultMetadata) -> Result<(), CodegenError> {
        self.emit_fault(FaultEmission::Contract(metadata))
    }

    fn coroutine_caller_span(&self) -> Result<CoroutineCallerSpan<'ctx>, CodegenError> {
        let coroutine = self.coroutine.as_ref().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "dynamic precondition blame is not inside a coroutine",
            )
        })?;
        let fields = coroutine.layout.caller_span_fields.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "dynamic precondition blame has no coroutine caller-span fields",
            )
        })?;
        let mut values = Vec::with_capacity(3);
        for (index, field) in fields.into_iter().enumerate() {
            let pointer = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    field,
                    &format!("coroutine.caller_span.{index}.pointer"),
                )
                .map_err(builder_error)?;
            values.push(
                self.backend
                    .builder
                    .build_load(
                        self.backend.context.i64_type(),
                        pointer,
                        &format!("coroutine.caller_span.{index}"),
                    )
                    .map_err(builder_error)?
                    .into_int_value(),
            );
        }
        let [file, start, end] = values.as_slice() else {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "coroutine caller span does not have three fields",
            ));
        };
        Ok(CoroutineCallerSpan {
            file: *file,
            start: *start,
            end: *end,
        })
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
            FaultEmission::Contract(metadata) => match metadata.blame() {
                ContractFaultBlame::Static(_) => {
                    self.backend.raise_contract_fault(runtime, metadata)?;
                }
                ContractFaultBlame::CoroutineCallSite => {
                    let span = self.coroutine_caller_span()?;
                    self.backend
                        .raise_contract_fault_with_span(runtime, metadata, span)?;
                }
            },
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
        if let Some(coroutine) = &self.coroutine {
            if !writebacks.is_empty() {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "typed coroutine return cannot carry inout writebacks",
                ));
            }
            let result = self
                .backend
                .builder
                .build_struct_gep(
                    coroutine.layout.frame,
                    coroutine.frame,
                    coroutine.layout.result_field,
                    "task.result.pointer",
                )
                .map_err(builder_error)?;
            self.backend
                .builder
                .build_store(result, value)
                .map_err(builder_error)?;
            let published = call_int(
                &self.backend.builder,
                self.backend.typed_task_publish_result(),
                &[coroutine.task.into()],
                "task.result.publish",
            )?;
            self.backend
                .require_zero_status(published, "task.result.publish")?;
            return self.emit_coroutine_step_return(TASK_COMPLETED);
        }
        let mut values = Vec::with_capacity(1 + writebacks.len());
        values.push(value);
        for writeback in writebacks {
            values.push(self.value(*writeback)?);
        }
        if self.source.effects().contains(Effects::MAY_FAULT) {
            self.emit_status_return(self.backend.context.i32_type().const_zero(), &values)
        } else if let [value] = values.as_slice() {
            self.pop_root_frame()?;
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
            self.pop_root_frame()?;
            self.backend
                .builder
                .build_return(Some(&aggregate))
                .map_err(builder_error)?;
            Ok(())
        }
    }

    fn emit_fault_return(&self, writebacks: &[ValueId]) -> Result<(), CodegenError> {
        if self.coroutine.is_some() {
            if !writebacks.is_empty() {
                return Err(CodegenError::new(
                    "LlvmAbiDefect",
                    "typed coroutine fault cannot carry inout writebacks",
                ));
            }
            return self.emit_coroutine_step_return(TASK_FAULTED);
        }
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
        self.pop_root_frame()?;
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
        effects: Effects,
    ) -> Result<Vec<BasicMetadataValueEnum<'ctx>>, CodegenError> {
        let mut values = arguments
            .iter()
            .copied()
            .map(|value| self.value(value).map(Into::into))
            .collect::<Result<Vec<_>, _>>()?;
        if effects.contains(Effects::MAY_FAULT) {
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
        if effects.contains(Effects::NEEDS_EXECUTOR) {
            values.push(self.executor_context()?.into());
        }
        Ok(values)
    }

    fn executor_context(&self) -> Result<PointerValue<'ctx>, CodegenError> {
        self.executor_context.ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                format!("{} has no active executor context", self.source.id()),
            )
        })
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
        let mut value = self
            .values
            .get(id.index())
            .copied()
            .flatten()
            .ok_or_else(|| {
                CodegenError::new(
                    "LlvmAbiDefect",
                    format!("LCIR value {id} has no LLVM definition"),
                )
            })?;
        let Some((start, end)) = self.root_slot_ranges.get(id.index()).copied().flatten() else {
            return Ok(value);
        };
        for index in start..end {
            let slot = self.root_plan.slots().get(index).ok_or_else(|| {
                CodegenError::new("LlvmAbiDefect", "managed-root slot disappeared")
            })?;
            let cell = self
                .root_cells
                .get(index)
                .copied()
                .flatten()
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", "managed-root cell is missing")
                })?;
            let pointer = self
                .backend
                .builder
                .build_load(
                    self.backend.ptr_type,
                    cell,
                    &format!(
                        "managed.root.reload.v{}.{}",
                        id.raw(),
                        Self::managed_projection_name(slot.projection())
                    ),
                )
                .map_err(builder_error)?
                .into_pointer_value();
            let ty = self
                .source
                .value(id)
                .ok_or_else(|| {
                    CodegenError::new("LlvmAbiDefect", format!("LCIR value {id} disappeared"))
                })?
                .ty();
            // Deliberately feed each replacement into the aggregate rebuilt by
            // the preceding slot. This prevents a later sibling or alias from
            // restoring a stale pre-collection pointer.
            value = self.rebuild_projected_value(value, ty, slot.projection(), pointer)?;
        }
        Ok(value)
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
    fn uses_process_arguments(&self) -> bool {
        self.artifact.functions().iter().any(|function| {
            function.instructions().iter().any(|instruction| {
                matches!(
                    instruction.kind(),
                    InstructionKind::ProcessArgumentCount
                        | InstructionKind::ProcessArgumentAt { .. }
                )
            })
        })
    }

    fn emit_main(&self) -> Result<(), CodegenError> {
        let main_type = self.context.i32_type().fn_type(
            &[self.context.i32_type().into(), self.ptr_type.into()],
            false,
        );
        let main = self.module.add_function("main", main_type, None);
        let entry = self.context.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);
        if self.uses_process_arguments() {
            let argument_count = main
                .get_nth_param(0)
                .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "main argc is missing"))?;
            let argument_vector = main
                .get_nth_param(1)
                .ok_or_else(|| CodegenError::new("LlvmAbiDefect", "main argv is missing"))?;
            let status = call_int(
                &self.builder,
                self.runtime_process_arguments_initialize_typed(),
                &[argument_count.into(), argument_vector.into()],
                "process.arguments.initialize.status",
            )?;
            self.require_zero_status(status, "process.arguments.initialize")?;
        }
        if let Some(root) = self.artifact.run_root() {
            let source = self.artifact.function(root).ok_or_else(|| {
                CodegenError::new("InvalidFunctionReference", "LCIR run root is missing")
            })?;
            if source.coroutine().is_some()
                || source.effects().contains(Effects::NEEDS_RUNTIME)
                || source.effects().contains(Effects::MAY_FAULT)
            {
                self.emit_runtime_run(main, root)
            } else {
                self.builder
                    .build_call(self.function(root)?, &[], "run")
                    .map_err(builder_error)?;
                let output_status = self.stdout_line("Unit")?;
                self.builder
                    .build_return(Some(&output_status))
                    .map_err(builder_error)?;
                Ok(())
            }
        } else {
            self.emit_tests(main)
        }
    }

    fn emit_runtime_run(
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
        self.stdout_line("RuntimeFault: runtime creation failed")?;
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
        self.stdout_line("RuntimeFault: runtime activation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        self.builder.position_at_end(activated);
        let source = self.artifact.function(root).ok_or_else(|| {
            CodegenError::new("InvalidFunctionReference", "LCIR run root is missing")
        })?;
        let status = if source.coroutine().is_some() {
            self.call_typed_async_root(runtime, root, "run")?.0
        } else if source.effects().contains(Effects::MAY_FAULT) {
            let fault_context = self.initialize_fault_context(runtime)?;
            self.call_fallible_root(root, fault_context, "run")?.0
        } else {
            self.builder
                .build_call(self.function(root)?, &[], "run")
                .map_err(builder_error)?;
            self.context.i32_type().const_zero()
        };
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
        let output_status = self.stdout_line("Unit")?;
        self.builder
            .build_return(Some(&output_status))
            .map_err(builder_error)?;
        self.builder.position_at_end(failure);
        self.builder
            .build_return(Some(&status))
            .map_err(builder_error)?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the async root harness keeps executor lifetime, exact result storage, terminal-state dispatch, fault reporting, and typed result merge in one auditable boundary"
    )]
    fn call_typed_async_root(
        &self,
        runtime: PointerValue<'ctx>,
        root: InstanceId,
        name: &str,
    ) -> Result<(IntValue<'ctx>, BasicValueEnum<'ctx>), CodegenError> {
        let source = self.artifact.function(root).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("LCIR async root {root} is missing"),
            )
        })?;
        if source.coroutine().is_none() || !source.signature().params().is_empty() {
            return Err(CodegenError::new(
                "LlvmAbiDefect",
                "typed async root must be a zero-argument checked coroutine",
            ));
        }
        let result_type = source.signature().result();
        let physical = self.llvm_type(result_type)?;
        let result_size = self.target_data.get_abi_size(&physical);
        let result_align = u64::from(self.target_data.get_abi_alignment(&physical));
        let executor = call_pointer(
            &self.builder,
            self.executor_create_for_runtime(),
            &[runtime.into()],
            &format!("{name}.executor.create"),
        )?;
        self.require_nonnull(executor, &format!("{name}.executor.create"))?;
        let mut constructor_arguments = Vec::<BasicMetadataValueEnum<'ctx>>::new();
        if source
            .coroutine()
            .is_some_and(CoroutinePlan::carries_caller_span)
        {
            let span = source.origin().span;
            for coordinate in [span.file.0, span.range.start, span.range.end] {
                constructor_arguments.push(
                    self.context
                        .i64_type()
                        .const_int(u64::from(coordinate), false)
                        .into(),
                );
            }
        }
        constructor_arguments.push(executor.into());
        let task = call_pointer(
            &self.builder,
            self.function(root)?,
            &constructor_arguments,
            &format!("{name}.task.create"),
        )?;
        self.require_nonnull(task, &format!("{name}.task.create"))?;
        let storage = (result_size != 0)
            .then(|| {
                self.builder
                    .build_alloca(physical, &format!("{name}.task.result"))
                    .map_err(builder_error)
            })
            .transpose()?;
        let status = call_int(
            &self.builder,
            self.executor_run(),
            &[executor.into(), task.into()],
            &format!("{name}.task.run"),
        )?;
        let function = self
            .builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "async root has no function"))?;
        let completed = self
            .context
            .append_basic_block(function, &format!("{name}.task.completed"));
        let incomplete = self
            .context
            .append_basic_block(function, &format!("{name}.task.incomplete"));
        let merge = self
            .context
            .append_basic_block(function, &format!("{name}.task.merge"));
        let succeeded = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context
                    .i32_type()
                    .const_int(TASK_COMPLETED as u64, false),
                &format!("{name}.task.succeeded"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(succeeded, completed, incomplete)
            .map_err(builder_error)?;

        self.builder.position_at_end(completed);
        let taken = call_int(
            &self.builder,
            self.typed_task_take_result(),
            &[
                task.into(),
                storage.unwrap_or_else(|| self.ptr_type.const_null()).into(),
                self.context.i64_type().const_int(result_size, false).into(),
                self.context
                    .i64_type()
                    .const_int(result_align, false)
                    .into(),
            ],
            &format!("{name}.task.take"),
        )?;
        self.require_zero_status(taken, &format!("{name}.task.take"))?;
        let returned = if let Some(storage) = storage {
            self.builder
                .build_load(physical, storage, &format!("{name}.task.returned"))
                .map_err(builder_error)?
        } else {
            self.zero(result_type)?
        };
        let completed_tail = self
            .builder
            .get_insert_block()
            .ok_or_else(|| CodegenError::new("LlvmBuilderFailed", "async take has no block"))?;
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(incomplete);
        let report = self
            .context
            .append_basic_block(function, &format!("{name}.task.report_fault"));
        let skip_report = self
            .context
            .append_basic_block(function, &format!("{name}.task.skip_fault"));
        let failed = self
            .context
            .append_basic_block(function, &format!("{name}.task.failed"));
        let faulted = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status,
                self.context
                    .i32_type()
                    .const_int(TASK_FAULTED as u64, false),
                &format!("{name}.task.faulted"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_conditional_branch(faulted, report, skip_report)
            .map_err(builder_error)?;
        self.builder.position_at_end(report);
        self.builder
            .build_call(
                self.task_report_fault(),
                &[task.into()],
                &format!("{name}.task.fault.report"),
            )
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(skip_report);
        self.builder
            .build_unconditional_branch(failed)
            .map_err(builder_error)?;
        self.builder.position_at_end(failed);
        self.builder
            .build_unconditional_branch(merge)
            .map_err(builder_error)?;

        self.builder.position_at_end(merge);
        let value = self
            .builder
            .build_phi(physical, &format!("{name}.task.value"))
            .map_err(builder_error)?;
        let zero = self.zero(result_type)?;
        value.add_incoming(&[(&returned, completed_tail), (&zero, failed)]);
        self.builder
            .build_call(
                self.executor_destroy(),
                &[executor.into()],
                &format!("{name}.executor.destroy"),
            )
            .map_err(builder_error)?;
        Ok((status, value.as_basic_value()))
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
            if source.coroutine().is_some()
                || source.effects().contains(Effects::NEEDS_RUNTIME)
                || source.effects().contains(Effects::MAY_FAULT)
            {
                self.emit_runtime_test(main, *root, source.name(), *outcome, failed)?;
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

    fn emit_runtime_test(
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
        self.stdout_line("RuntimeFault: runtime creation failed")?;
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
        self.stdout_line("RuntimeFault: runtime activation failed")?;
        self.builder
            .build_return(Some(&self.context.i32_type().const_int(6, false)))
            .map_err(builder_error)?;

        let next = self.context.append_basic_block(main, "test.next");
        self.builder.position_at_end(activated);
        let source = self.artifact.function(root).ok_or_else(|| {
            CodegenError::new(
                "InvalidFunctionReference",
                format!("LCIR test root {root} is missing"),
            )
        })?;
        let (status, returned) = if source.coroutine().is_some() {
            self.call_typed_async_root(runtime, root, "test")?
        } else if source.effects().contains(Effects::MAY_FAULT) {
            let fault_context = self.initialize_fault_context(runtime)?;
            self.call_fallible_root(root, fault_context, "test")?
        } else {
            (
                self.context.i32_type().const_zero(),
                call_basic(&self.builder, self.function(root)?, &[], "test")?,
            )
        };
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
        let output_status = self.stdout_line(&format!("passed {name}"))?;
        let output_failed = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                output_status,
                self.context.i32_type().const_zero(),
                "test.output.failed",
            )
            .map_err(builder_error)?;
        let previous = self
            .builder
            .build_load(self.context.i32_type(), failed, "tests.previous_status")
            .map_err(builder_error)?
            .into_int_value();
        let previously_failed = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                previous,
                self.context.i32_type().const_zero(),
                "tests.previously_failed",
            )
            .map_err(builder_error)?;
        let any_failed = self
            .builder
            .build_or(previously_failed, output_failed, "tests.any_failed")
            .map_err(builder_error)?;
        let normalized = self
            .builder
            .build_int_z_extend(
                any_failed,
                self.context.i32_type(),
                "tests.status.with_output",
            )
            .map_err(builder_error)?;
        self.builder
            .build_store(failed, normalized)
            .map_err(builder_error)?;
        self.builder
            .build_unconditional_branch(next)
            .map_err(builder_error)?;
        self.builder.position_at_end(fail);
        self.stdout_line(&format!("failed {name}"))?;
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

    fn stdout_line(&self, value: &str) -> Result<IntValue<'ctx>, CodegenError> {
        let value = format!("{value}\n");
        let length = u64::try_from(value.len())
            .map_err(|_| CodegenError::new("ProgramTooLarge", "stdout line is too large"))?;
        let string = self
            .builder
            .build_global_string_ptr(&value, &self.unique("stdout.line"))
            .map_err(builder_error)?;
        call_int(
            &self.builder,
            self.runtime_stdout_write(),
            &[
                string.as_pointer_value().into(),
                self.context.i64_type().const_int(length, false).into(),
            ],
            "stdout.write",
        )
    }

    fn raise_fault(
        &self,
        runtime: PointerValue<'ctx>,
        fault: FaultCode,
        origin: Origin,
    ) -> Result<(), CodegenError> {
        let (code, message) = fault_properties(fault);
        let display = format!("{code}: {message}");
        // Language-defined fault families expose the same stable RuntimeFault
        // payload as the interpreter. Other LCIR-private
        // families retain their existing backend detail until their source
        // diagnostic contracts are specified separately.
        let detail = if matches!(
            fault,
            FaultCode::IntegerOverflow
                | FaultCode::ArtifactProofRejected
                | FaultCode::InvalidDuration
                | FaultCode::InvalidSleepDuration
                | FaultCode::SleepDurationOverflow
                | FaultCode::TaskAnyFailed
                | FaultCode::EmptyTaskJoin
                | FaultCode::LogWrite
                | FaultCode::StdoutWrite
        ) {
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
        let blame_span = metadata.blame_span().ok_or_else(|| {
            CodegenError::new(
                "LlvmAbiDefect",
                "static contract-fault emission has dynamic blame",
            )
        })?;
        let detail = serde_json::to_string(&serde_json::json!({
            "channel": "contract",
            "fault": {
                "code": code,
                "category": metadata.kind().category(),
                "message": message,
                "contractSpan": metadata.contract_span(),
                "blameSpan": blame_span,
            },
        }))
        .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
        self.raise_fault_payload(runtime, code, message, &display, &detail)
    }

    fn raise_contract_fault_with_span(
        &self,
        runtime: PointerValue<'ctx>,
        metadata: &ContractFaultMetadata,
        span: CoroutineCallerSpan<'ctx>,
    ) -> Result<(), CodegenError> {
        let code = metadata.kind().fault_code();
        let message = metadata.message();
        let display = metadata.user_code().map_or_else(
            || code.to_owned(),
            |user_code| format!("{code}: {user_code}"),
        );
        let marker = serde_json::json!({"__loomCallerSpan": true});
        let marker_text = serde_json::to_string(&marker)
            .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
        let detail = serde_json::to_string(&serde_json::json!({
            "channel": "contract",
            "fault": {
                "code": code,
                "category": metadata.kind().category(),
                "message": message,
                "contractSpan": metadata.contract_span(),
                "blameSpan": marker,
            },
        }))
        .map_err(|error| CodegenError::new("FaultEncodingFailed", error.to_string()))?;
        let (prefix, suffix) = detail.split_once(&marker_text).ok_or_else(|| {
            CodegenError::new(
                "FaultEncodingFailed",
                "caller span marker is missing from contract fault detail",
            )
        })?;
        self.raise_fault_payload_with_span(runtime, code, message, &display, prefix, span, suffix)
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

    #[allow(clippy::too_many_arguments)]
    fn raise_fault_payload_with_span(
        &self,
        runtime: PointerValue<'ctx>,
        code: &str,
        message: &str,
        display: &str,
        detail_prefix: &str,
        span: CoroutineCallerSpan<'ctx>,
        detail_suffix: &str,
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
        let prefix_data = self
            .builder
            .build_global_string_ptr(detail_prefix, &self.unique("fault.detail.prefix"))
            .map_err(builder_error)?;
        let suffix_data = self
            .builder
            .build_global_string_ptr(detail_suffix, &self.unique("fault.detail.suffix"))
            .map_err(builder_error)?;
        self.builder
            .build_call(
                self.context_raise_fault_with_span()?,
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
                    prefix_data.as_pointer_value().into(),
                    self.context
                        .i64_type()
                        .const_int(detail_prefix.len() as u64, false)
                        .into(),
                    span.file.into(),
                    span.start.into(),
                    span.end.into(),
                    suffix_data.as_pointer_value().into(),
                    self.context
                        .i64_type()
                        .const_int(detail_suffix.len() as u64, false)
                        .into(),
                ],
                "fault.raise.with.span",
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

    fn context_raise_fault_with_span(&self) -> Result<FunctionValue<'ctx>, CodegenError> {
        if let Some(function) = self
            .module
            .get_function("loom_context_raise_fault_with_span_v1")
        {
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
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.context.i64_type().into(),
                self.ptr_type.into(),
                self.context.i64_type().into(),
            ],
            false,
        );
        let function =
            self.module
                .add_function("loom_context_raise_fault_with_span_v1", function_type, None);
        mark_cold_noinline(self.context, function)?;
        Ok(function)
    }

    fn runtime_stdout_write(&self) -> FunctionValue<'ctx> {
        self.module
            .get_function(STDOUT_WRITE_SYMBOL)
            .unwrap_or_else(|| {
                let function_type = self.context.i32_type().fn_type(
                    &[self.ptr_type.into(), self.context.i64_type().into()],
                    false,
                );
                self.module
                    .add_function(STDOUT_WRITE_SYMBOL, function_type, None)
            })
    }
}

const fn task_join_mode_name(mode: AwaitMode) -> &'static str {
    match mode {
        AwaitMode::All => "all",
        AwaitMode::Any => "any",
        AwaitMode::Settled => "settled",
        AwaitMode::Race => "race",
    }
}

fn fault_properties(code: FaultCode) -> (&'static str, &'static str) {
    match code {
        FaultCode::ArtifactProofRejected => (
            ARTIFACT_PROOF_REJECTED_FAULT_CODE,
            ARTIFACT_PROOF_REJECTED_FAULT_MESSAGE,
        ),
        FaultCode::IntegerOverflow => (INTEGER_OVERFLOW_FAULT_CODE, INTEGER_OVERFLOW_FAULT_MESSAGE),
        FaultCode::IntegerDivisionByZero => ("IntegerDivisionByZero", "integer division by zero"),
        FaultCode::IntegerDivisionOverflow => {
            ("IntegerDivisionOverflow", "integer division overflowed")
        }
        FaultCode::InvalidDuration => (INVALID_DURATION_FAULT_CODE, INVALID_DURATION_FAULT_MESSAGE),
        FaultCode::InvalidSleepDuration => (
            INVALID_SLEEP_DURATION_FAULT_CODE,
            INVALID_SLEEP_DURATION_FAULT_MESSAGE,
        ),
        FaultCode::SleepDurationOverflow => (
            SLEEP_DURATION_OVERFLOW_FAULT_CODE,
            SLEEP_DURATION_OVERFLOW_FAULT_MESSAGE,
        ),
        FaultCode::TaskAnyFailed => (TASK_ANY_FAILED_FAULT_CODE, TASK_ANY_FAILED_FAULT_MESSAGE),
        FaultCode::EmptyTaskJoin => (EMPTY_TASK_JOIN_FAULT_CODE, EMPTY_TASK_JOIN_FAULT_MESSAGE),
        FaultCode::LogWrite => (LOG_WRITE_FAULT_CODE, LOG_WRITE_FAULT_MESSAGE),
        FaultCode::StdoutWrite => (STDOUT_WRITE_FAULT_CODE, STDOUT_WRITE_FAULT_MESSAGE),
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

    use super::{
        SUM_CARRIER_MAX_BYTES, SUM_CARRIER_MAX_EMISSION_BYTE_WORK, SumPayloadShape,
        checked_sum_carrier_emission_work, plan_sum_carrier,
    };

    #[test]
    fn bare_artifact_paths_do_not_attempt_to_create_an_empty_parent() {
        super::create_parent_directory(Path::new("artifact.o"))
            .expect("a bare artifact path has no directory to create");
    }

    #[test]
    fn sum_carrier_planner_compacts_equal_byte_classes_without_cross_class_aliases() {
        let json = plan_sum_carrier(
            &[
                SumPayloadShape {
                    size: 0,
                    alignment: 1,
                    pointer_offsets: vec![],
                },
                SumPayloadShape {
                    size: 1,
                    alignment: 1,
                    pointer_offsets: vec![],
                },
                SumPayloadShape {
                    size: 8,
                    alignment: 8,
                    pointer_offsets: vec![],
                },
                SumPayloadShape {
                    size: 8,
                    alignment: 8,
                    pointer_offsets: vec![0],
                },
                SumPayloadShape {
                    size: 8,
                    alignment: 8,
                    pointer_offsets: vec![0],
                },
                SumPayloadShape {
                    size: 8,
                    alignment: 8,
                    pointer_offsets: vec![0],
                },
            ],
            8,
            8,
            super::SUM_CARRIER_MAX_PLACEMENT_WORK,
        )
        .expect("plan compact Json carrier");
        assert_eq!(json.payload_byte_offsets, [0, 0, 0, 8, 8, 8]);
        assert_eq!(json.byte_len, 16);
        assert_eq!(json.alignment, 8);

        let outer = plan_sum_carrier(
            &[
                SumPayloadShape {
                    size: 24,
                    alignment: 8,
                    pointer_offsets: vec![16],
                },
                SumPayloadShape {
                    size: 24,
                    alignment: 8,
                    pointer_offsets: vec![],
                },
            ],
            8,
            8,
            super::SUM_CARRIER_MAX_PLACEMENT_WORK,
        )
        .expect("plan compact nested carrier");
        assert_eq!(outer.payload_byte_offsets, [8, 0]);
        assert_eq!(outer.byte_len, 32);
        assert_eq!(outer.alignment, 8);

        let crossed = plan_sum_carrier(
            &[
                SumPayloadShape {
                    size: 16,
                    alignment: 8,
                    pointer_offsets: vec![0],
                },
                SumPayloadShape {
                    size: 16,
                    alignment: 8,
                    pointer_offsets: vec![8],
                },
            ],
            8,
            8,
            super::SUM_CARRIER_MAX_PLACEMENT_WORK,
        )
        .expect("separate oppositely interleaved pointer/scalar variants");
        assert_eq!(crossed.payload_byte_offsets, [0, 8]);
        assert_eq!(crossed.byte_len, 24);
        assert_eq!(crossed.alignment, 8);
    }

    #[test]
    fn sum_carrier_planner_fails_closed_on_invalid_or_oversized_shapes() {
        let invalid_pointer = plan_sum_carrier(
            &[SumPayloadShape {
                size: 8,
                alignment: 8,
                pointer_offsets: vec![1],
            }],
            8,
            8,
            super::SUM_CARRIER_MAX_PLACEMENT_WORK,
        )
        .expect_err("unaligned pointer bytes must fail");
        assert_eq!(invalid_pointer.code(), "LlvmAbiDefect");

        let oversized = plan_sum_carrier(
            &[SumPayloadShape {
                size: SUM_CARRIER_MAX_BYTES + 1,
                alignment: 8,
                pointer_offsets: vec![],
            }],
            8,
            8,
            super::SUM_CARRIER_MAX_PLACEMENT_WORK,
        )
        .expect_err("oversized carriers must fail before host allocation");
        assert_eq!(oversized.code(), "ProgramTooLarge");

        assert_eq!(
            checked_sum_carrier_emission_work(SUM_CARRIER_MAX_EMISSION_BYTE_WORK - 8, 8)
                .expect("the exact global byte-emission limit is admitted"),
            SUM_CARRIER_MAX_EMISSION_BYTE_WORK
        );
        let excessive_emission =
            checked_sum_carrier_emission_work(SUM_CARRIER_MAX_EMISSION_BYTE_WORK, 1)
                .expect_err("pack/unpack work beyond the shared limit must fail");
        assert_eq!(excessive_emission.code(), "ProgramTooLarge");

        let exhausted_placement = plan_sum_carrier(
            &[SumPayloadShape {
                size: 8,
                alignment: 8,
                pointer_offsets: vec![],
            }],
            8,
            8,
            0,
        )
        .expect_err("a depleted artifact-wide placement budget must fail immediately");
        assert_eq!(exhausted_placement.code(), "ProgramTooLarge");
    }
}
