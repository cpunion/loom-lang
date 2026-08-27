use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::ids::ProgramBrand;
use crate::{
    Block, BlockId, CheckedProgram, Effects, Function, InstanceId, InstanceKey, InstancePlan,
    Instruction, InstructionId, InstructionKind, Origin, PlannedInstance, Program,
    RepresentationPlan, Signature, TargetLayout, Terminator, Value, ValueDefinition, ValueId,
    ValueTypeId, check_program,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildErrorCode {
    ProgramTooLarge,
    DuplicateInstance,
    InstanceSourceMismatch,
    InstanceKeyStructureBudget,
    OpenInstanceKey,
    InvalidFunction,
    InvalidBlock,
    InvalidValueType,
    InvalidTextType,
    InvalidProductType,
    InvalidSumType,
    DuplicateEntry,
    BlockAlreadyTerminated,
    TrustedInstruction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildError {
    code: BuildErrorCode,
    message: String,
}

impl BuildError {
    fn new(code: BuildErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> BuildErrorCode {
        self.code
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BuildError {}

/// Constructs an unchecked LCIR program through dense, deterministic tables.
/// Independent validation remains authoritative.
pub struct ProgramBuilder {
    brand: ProgramBrand,
    representations: RepresentationPlan,
    instances: InstancePlan,
    functions: Vec<Function>,
}

impl ProgramBuilder {
    #[must_use]
    pub fn new(target: TargetLayout) -> Self {
        let brand = ProgramBrand::fresh();
        Self {
            brand,
            representations: RepresentationPlan::direct_with_brand(target, brand),
            instances: InstancePlan::with_brand(brand),
            functions: Vec::new(),
        }
    }

    #[must_use]
    pub const fn representations(&self) -> &RepresentationPlan {
        &self.representations
    }

    #[must_use]
    pub fn type_id(&self, semantic: &Type) -> Option<ValueTypeId> {
        self.representations.type_id(semantic)
    }

    #[must_use]
    pub const fn instances(&self) -> &InstancePlan {
        &self.instances
    }

    /// Registers the compiler-private representation used by the bounded
    /// literal-only Text slice.
    ///
    /// This is intentionally not a general managed Text representation. Its
    /// only constructor is [`InstructionKind::TextLiteral`], and the complete
    /// artifact boundary prevents foreign values from entering internal
    /// functions. The current native runtime Text layout is defined only for
    /// 64-bit targets.
    ///
    /// # Errors
    ///
    /// Returns an error after function declaration, for a non-64-bit target,
    /// for duplicate registration, or when an identity table is exhausted.
    pub fn add_immortal_text_type(&mut self) -> Result<ValueTypeId, BuildError> {
        if !self.functions.is_empty() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidTextType,
                "LCIR immortal Text must be registered before functions",
            ));
        }
        self.representations.add_immortal_text().ok_or_else(|| {
            BuildError::new(
                BuildErrorCode::InvalidTextType,
                "LCIR immortal Text requires one unique registration on a 64-bit target",
            )
        })
    }

    /// Adds one monomorphic record whose fields already have canonical direct
    /// representations. Nested products must therefore be registered before
    /// their containing products. Product types must be registered before
    /// declaring functions so every signature is fixed before CFG construction.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate/non-nominal semantic type, a
    /// unregistered field, or an exhausted representation identity domain.
    pub fn add_pod_record_type(
        &mut self,
        semantic: Type,
        fields: &[Type],
    ) -> Result<ValueTypeId, BuildError> {
        if !self.functions.is_empty() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidProductType,
                "LCIR product types must be registered before functions",
            ));
        }
        self.representations
            .add_pod_record(semantic, fields)
            .ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::InvalidProductType,
                    "LCIR POD record requires one unique monomorphic nominal type whose fields already have direct representations",
                )
            })
    }

    /// Adds a monomorphic record whose invariant has already been proved by
    /// semantic analysis. Its physical layout is an ordinary direct product,
    /// but independent validation prevents the ordinary product-construction
    /// instruction from creating values of this type.
    ///
    /// # Errors
    ///
    /// Returns an error under the same registration constraints as
    /// [`Self::add_pod_record_type`].
    pub fn add_invariant_record_type(
        &mut self,
        semantic: Type,
        fields: &[Type],
    ) -> Result<ValueTypeId, BuildError> {
        if !self.functions.is_empty() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidProductType,
                "LCIR invariant record types must be registered before functions",
            ));
        }
        self.representations
            .add_invariant_record(semantic, fields)
            .ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::InvalidProductType,
                    "LCIR invariant record requires one unique monomorphic nominal type whose fields already have direct representations",
                )
            })
    }

    /// Adds a semantically distinct nominal type which transparently reuses
    /// its already-registered base representation.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate/non-nominal semantic type, an
    /// unregistered or uninhabited base, registration after function
    /// declaration, or an exhausted value-type identity domain.
    pub fn add_transparent_type(
        &mut self,
        semantic: Type,
        base: &Type,
    ) -> Result<ValueTypeId, BuildError> {
        if !self.functions.is_empty() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidValueType,
                "LCIR transparent types must be registered before functions",
            ));
        }
        self.representations
            .add_transparent(semantic, base)
            .ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::InvalidValueType,
                    "LCIR transparent type requires one unique monomorphic nominal type and an already-registered inhabited base",
                )
            })
    }

    /// Adds one structural tuple whose elements already have canonical direct
    /// representations. Nested products must be registered before their
    /// containing tuple, and all product types must be registered before any
    /// function declaration.
    ///
    /// # Errors
    ///
    /// Returns an error for a duplicate tuple, an unregistered element type,
    /// registration after function declaration, or an exhausted identity
    /// domain.
    pub fn add_tuple_type(&mut self, elements: &[Type]) -> Result<ValueTypeId, BuildError> {
        if !self.functions.is_empty() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidProductType,
                "LCIR product types must be registered before functions",
            ));
        }
        self.representations.add_tuple(elements).ok_or_else(|| {
            BuildError::new(
                BuildErrorCode::InvalidProductType,
                "LCIR tuple requires one unique structural tuple whose elements already have direct representations",
            )
        })
    }

    /// Adds one closed concrete sum whose ordered variant payload types have
    /// already been registered. Sum types must be fixed before functions so
    /// signatures and case-edge payload parameters have stable identities.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty/duplicate/non-nominal sum, an
    /// unregistered payload type, registration after function declaration, or
    /// an exhausted identity domain.
    pub fn add_sum_type(
        &mut self,
        semantic: Type,
        variants: &[Box<[Type]>],
    ) -> Result<ValueTypeId, BuildError> {
        if !self.functions.is_empty() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidSumType,
                "LCIR sum types must be registered before functions",
            ));
        }
        self.representations
            .add_sum(semantic, variants)
            .ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::InvalidSumType,
                    "LCIR sum requires one unique nonempty concrete nominal type whose variant payloads already have direct representations",
                )
            })
    }

    /// Declares a monomorphic function before its CFG is built. Declaring all
    /// functions first permits direct recursive and mutually recursive call
    /// references. Producers with explicit arguments use
    /// [`Self::declare_instance`].
    ///
    /// # Errors
    ///
    /// Returns an error when an LCIR table would exceed `u32` or the signature
    /// names a type outside this program's representation plan.
    pub fn declare_function(
        &mut self,
        origin: Origin,
        name: impl Into<String>,
        signature: Signature,
        effects: Effects,
    ) -> Result<InstanceId, BuildError> {
        self.declare_instance(
            InstanceKey::monomorphic(origin.source_function),
            origin,
            name,
            signature,
            effects,
        )
    }

    /// Declares one explicitly keyed callable instance before its CFG is built.
    ///
    /// # Errors
    ///
    /// Returns an error for duplicate or structurally oversized keys, a source
    /// mismatch between semantic identity and diagnostic origin, an oversized
    /// LCIR table, or a signature type outside this representation plan.
    pub fn declare_instance(
        &mut self,
        key: InstanceKey,
        origin: Origin,
        name: impl Into<String>,
        signature: Signature,
        effects: Effects,
    ) -> Result<InstanceId, BuildError> {
        for ty in signature.params().iter().chain([&signature.result()]) {
            self.require_type(*ty)?;
        }
        if key.source() != origin.source_function {
            return Err(BuildError::new(
                BuildErrorCode::InstanceSourceMismatch,
                format!(
                    "LCIR instance source #{} does not match origin source #{}",
                    key.source().0,
                    origin.source_function.0
                ),
            ));
        }
        if let Err(error) = key.validate_structure() {
            return Err(match error {
                crate::instance::InstanceKeyStructureError::BudgetExceeded => BuildError::new(
                    BuildErrorCode::InstanceKeyStructureBudget,
                    format!(
                        "LCIR instance key exceeds the {0}-node structural budget",
                        crate::INSTANCE_KEY_STRUCTURE_BUDGET
                    ),
                ),
                crate::instance::InstanceKeyStructureError::OpenArgument => BuildError::new(
                    BuildErrorCode::OpenInstanceKey,
                    "LCIR instance keys require fully substituted type and witness arguments",
                ),
            });
        }
        if let Some(existing) = self.instances.find(&key) {
            return Err(BuildError::new(
                BuildErrorCode::DuplicateInstance,
                format!("LCIR instance key is already assigned to {existing}"),
            ));
        }
        let id =
            InstanceId::from_index(self.brand, self.instances.entries.len()).ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::ProgramTooLarge,
                    "LCIR has too many function instances",
                )
            })?;
        self.instances.entries.push(PlannedInstance { id, key });
        self.functions.push(Function {
            id,
            origin,
            name: name.into(),
            signature,
            effects,
            entry: None,
            blocks: Vec::new(),
            instructions: Vec::new(),
            values: Vec::new(),
        });
        Ok(id)
    }

    /// Borrows one declared function for CFG construction.
    ///
    /// # Errors
    ///
    /// Returns an error when `function` is not declared in this program.
    pub fn function(&mut self, function: InstanceId) -> Result<FunctionBuilder<'_>, BuildError> {
        if self.instances.key(function).is_none() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidFunction,
                format!("LCIR function {function} does not exist"),
            ));
        }
        let representations = &self.representations;
        let Some(function) = self
            .functions
            .get_mut(function.index())
            .filter(|candidate| candidate.id == function)
        else {
            return Err(BuildError::new(
                BuildErrorCode::InvalidFunction,
                format!("LCIR function {function} does not exist"),
            ));
        };
        Ok(FunctionBuilder {
            representations,
            function,
        })
    }

    #[must_use]
    pub fn finish(self) -> Program {
        Program {
            brand: self.brand,
            representations: self.representations,
            instances: self.instances,
            functions: self.functions,
        }
    }

    /// Finishes and crosses the independent LCIR validation boundary.
    ///
    /// # Errors
    ///
    /// Returns every independently discoverable structural failure.
    pub fn finish_checked(self) -> Result<CheckedProgram, crate::ValidationErrors> {
        check_program(self.finish())
    }

    fn require_type(&self, ty: ValueTypeId) -> Result<(), BuildError> {
        if self.representations.value_type(ty).is_none() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidValueType,
                format!("LCIR value type {ty} does not exist"),
            ));
        }
        Ok(())
    }
}

/// Builds one function's explicit SSA control-flow graph.
pub struct FunctionBuilder<'a> {
    representations: &'a RepresentationPlan,
    function: &'a mut Function,
}

impl FunctionBuilder<'_> {
    pub(crate) const fn representations(&self) -> &RepresentationPlan {
        self.representations
    }

    /// Appends a block. Blocks and values use independent dense identity
    /// domains within this function.
    ///
    /// # Errors
    ///
    /// Returns an error when the block table would exceed `u32`.
    pub fn create_block(&mut self) -> Result<BlockId, BuildError> {
        let id =
            BlockId::from_index(self.function.id, self.function.blocks.len()).ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::ProgramTooLarge,
                    format!("LCIR function {} has too many blocks", self.function.id),
                )
            })?;
        self.function.blocks.push(Block {
            id,
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: None,
        });
        Ok(id)
    }

    /// Selects the unique entry block.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown block or a second entry assignment.
    pub fn set_entry(&mut self, block: BlockId) -> Result<(), BuildError> {
        self.require_block(block)?;
        if self.function.entry.is_some() {
            return Err(BuildError::new(
                BuildErrorCode::DuplicateEntry,
                format!(
                    "LCIR function {} already has an entry block",
                    self.function.id
                ),
            ));
        }
        self.function.entry = Some(block);
        Ok(())
    }

    /// Appends an SSA block parameter.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown block/type or an exhausted identity
    /// domain.
    pub fn append_block_parameter(
        &mut self,
        block: BlockId,
        ty: ValueTypeId,
    ) -> Result<ValueId, BuildError> {
        self.require_block(block)?;
        self.require_type(ty)?;
        let value = self.next_value_id()?;
        let index =
            u32::try_from(self.function.blocks[block.index()].params.len()).map_err(|_| {
                BuildError::new(
                    BuildErrorCode::ProgramTooLarge,
                    format!("LCIR block {block} has too many parameters"),
                )
            })?;
        self.function.values.push(Value {
            id: value,
            ty,
            definition: ValueDefinition::BlockParameter { block, index },
        });
        self.function.blocks[block.index()].params.push(value);
        Ok(value)
    }

    /// Appends an instruction and allocates its result values atomically.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/terminated block, an unknown result
    /// type, or an exhausted identity domain.
    pub fn append_instruction(
        &mut self,
        block: BlockId,
        kind: InstructionKind,
        result_types: &[ValueTypeId],
        origin: Origin,
    ) -> Result<Box<[ValueId]>, BuildError> {
        if matches!(
            kind,
            InstructionKind::RefineProven { .. } | InstructionKind::InvariantRecordProven { .. }
        ) {
            return Err(BuildError::new(
                BuildErrorCode::TrustedInstruction,
                "proof-establishing LCIR instructions may only be emitted from checked MIR",
            ));
        }
        self.append_instruction_inner(block, kind, result_types, origin)
    }

    /// Appends an instruction carrying a frontend proof certificate.
    ///
    /// This entry point is deliberately crate-private: only lowering from
    /// checked MIR may establish a refined or invariant-protected value.
    pub(crate) fn append_trusted_instruction(
        &mut self,
        block: BlockId,
        kind: InstructionKind,
        result_types: &[ValueTypeId],
        origin: Origin,
    ) -> Result<Box<[ValueId]>, BuildError> {
        if !matches!(
            kind,
            InstructionKind::RefineProven { .. } | InstructionKind::InvariantRecordProven { .. }
        ) {
            return Err(BuildError::new(
                BuildErrorCode::TrustedInstruction,
                "the checked-MIR instruction path accepts only proof-establishing instructions",
            ));
        }
        self.append_instruction_inner(block, kind, result_types, origin)
    }

    fn append_instruction_inner(
        &mut self,
        block: BlockId,
        kind: InstructionKind,
        result_types: &[ValueTypeId],
        origin: Origin,
    ) -> Result<Box<[ValueId]>, BuildError> {
        self.require_open_block(block)?;
        for ty in result_types {
            self.require_type(*ty)?;
        }
        let instruction =
            InstructionId::from_index(self.function.id, self.function.instructions.len())
                .ok_or_else(|| {
                    BuildError::new(
                        BuildErrorCode::ProgramTooLarge,
                        format!(
                            "LCIR function {} has too many instructions",
                            self.function.id
                        ),
                    )
                })?;
        let first_result = self.function.values.len();
        let final_result = first_result
            .checked_add(result_types.len())
            .ok_or_else(|| {
                BuildError::new(
                    BuildErrorCode::ProgramTooLarge,
                    format!("LCIR function {} has too many values", self.function.id),
                )
            })?;
        if result_types.len() > u32::MAX as usize || final_result > u32::MAX as usize {
            return Err(BuildError::new(
                BuildErrorCode::ProgramTooLarge,
                format!("LCIR function {} has too many values", self.function.id),
            ));
        }
        let new_values = result_types
            .iter()
            .enumerate()
            .map(|(index, ty)| {
                let id = ValueId::from_index(self.function.id, first_result + index).ok_or_else(
                    || {
                        BuildError::new(
                            BuildErrorCode::ProgramTooLarge,
                            format!("LCIR function {} has too many values", self.function.id),
                        )
                    },
                )?;
                let index = u32::try_from(index).map_err(|_| {
                    BuildError::new(
                        BuildErrorCode::ProgramTooLarge,
                        "LCIR instruction has too many results",
                    )
                })?;
                Ok(Value {
                    id,
                    ty: *ty,
                    definition: ValueDefinition::InstructionResult { instruction, index },
                })
            })
            .collect::<Result<Vec<_>, BuildError>>()?;
        let results = new_values.iter().map(Value::id).collect::<Vec<_>>();
        self.function.values.extend(new_values);
        self.function.instructions.push(Instruction {
            id: instruction,
            results: results.clone(),
            kind,
            origin,
        });
        self.function.blocks[block.index()]
            .instructions
            .push(instruction);
        Ok(results.into_boxed_slice())
    }

    /// Installs the block's one terminator.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown block or an already installed
    /// terminator.
    pub fn terminate(&mut self, block: BlockId, terminator: Terminator) -> Result<(), BuildError> {
        self.require_open_block(block)?;
        self.function.blocks[block.index()].terminator = Some(terminator);
        Ok(())
    }

    fn require_type(&self, ty: ValueTypeId) -> Result<(), BuildError> {
        if self.representations.value_type(ty).is_none() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidValueType,
                format!("LCIR value type {ty} does not exist"),
            ));
        }
        Ok(())
    }

    fn require_block(&self, block: BlockId) -> Result<(), BuildError> {
        if block.owner() != self.function.id || self.function.blocks.get(block.index()).is_none() {
            return Err(BuildError::new(
                BuildErrorCode::InvalidBlock,
                format!(
                    "LCIR block {block} does not exist in function {}",
                    self.function.id
                ),
            ));
        }
        Ok(())
    }

    fn require_open_block(&self, block: BlockId) -> Result<(), BuildError> {
        self.require_block(block)?;
        if self.function.blocks[block.index()].terminator.is_some() {
            return Err(BuildError::new(
                BuildErrorCode::BlockAlreadyTerminated,
                format!("LCIR block {block} already has a terminator"),
            ));
        }
        Ok(())
    }

    fn next_value_id(&self) -> Result<ValueId, BuildError> {
        ValueId::from_index(self.function.id, self.function.values.len()).ok_or_else(|| {
            BuildError::new(
                BuildErrorCode::ProgramTooLarge,
                format!("LCIR function {} has too many values", self.function.id),
            )
        })
    }
}
