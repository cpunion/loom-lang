use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::ids::ProgramBrand;
use crate::{
    Block, BlockId, CheckedProgram, Effects, Function, InstanceId, Instruction, InstructionId,
    InstructionKind, Origin, Program, RepresentationPlan, Signature, TargetLayout, Terminator,
    Value, ValueDefinition, ValueId, ValueTypeId, check_program,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildErrorCode {
    ProgramTooLarge,
    InvalidFunction,
    InvalidBlock,
    InvalidValueType,
    DuplicateEntry,
    BlockAlreadyTerminated,
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
    functions: Vec<Function>,
}

impl ProgramBuilder {
    #[must_use]
    pub fn new(target: TargetLayout) -> Self {
        let brand = ProgramBrand::fresh();
        Self {
            brand,
            representations: RepresentationPlan::scalar_with_brand(target, brand),
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

    /// Declares a function before its CFG is built. Declaring all functions
    /// first permits direct recursive and mutually recursive call references.
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
        for ty in signature.params().iter().chain([&signature.result()]) {
            self.require_type(*ty)?;
        }
        let id = InstanceId::from_index(self.brand, self.functions.len()).ok_or_else(|| {
            BuildError::new(
                BuildErrorCode::ProgramTooLarge,
                "LCIR has too many function instances",
            )
        })?;
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
        let representations = &self.representations;
        let Some(function) = (function.brand() == self.brand)
            .then(|| self.functions.get_mut(function.index()))
            .flatten()
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
