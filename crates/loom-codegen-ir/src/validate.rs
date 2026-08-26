use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use loom_mir::Type;

use crate::{
    BlockId, Constant, Effects, Function, InstanceId, Instruction, InstructionId, InstructionKind,
    Program, Repr, RepresentationPlan, Terminator, TerminatorKind, ValueDefinition, ValueId,
    ValueTypeId,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ValidationCode {
    RepresentationPlan,
    IndexMismatch,
    InvalidFunctionReference,
    InvalidBlockReference,
    InvalidInstructionReference,
    InvalidValueReference,
    InvalidTypeReference,
    MissingEntry,
    EntrySignature,
    EntryPredecessor,
    MissingTerminator,
    InstructionSchedule,
    ValueDefinition,
    InstructionShape,
    TypeMismatch,
    BlockArgument,
    ReturnType,
    CallShape,
    EffectMismatch,
    OriginMismatch,
    DuplicateSuccessor,
    UninhabitedValue,
    UnreachableBlock,
    Dominance,
}

impl ValidationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RepresentationPlan => "LcirRepresentationPlan",
            Self::IndexMismatch => "LcirIndexMismatch",
            Self::InvalidFunctionReference => "LcirInvalidFunctionReference",
            Self::InvalidBlockReference => "LcirInvalidBlockReference",
            Self::InvalidInstructionReference => "LcirInvalidInstructionReference",
            Self::InvalidValueReference => "LcirInvalidValueReference",
            Self::InvalidTypeReference => "LcirInvalidTypeReference",
            Self::MissingEntry => "LcirMissingEntry",
            Self::EntrySignature => "LcirEntrySignature",
            Self::EntryPredecessor => "LcirEntryPredecessor",
            Self::MissingTerminator => "LcirMissingTerminator",
            Self::InstructionSchedule => "LcirInstructionSchedule",
            Self::ValueDefinition => "LcirValueDefinition",
            Self::InstructionShape => "LcirInstructionShape",
            Self::TypeMismatch => "LcirTypeMismatch",
            Self::BlockArgument => "LcirBlockArgument",
            Self::ReturnType => "LcirReturnType",
            Self::CallShape => "LcirCallShape",
            Self::EffectMismatch => "LcirEffectMismatch",
            Self::OriginMismatch => "LcirOriginMismatch",
            Self::DuplicateSuccessor => "LcirDuplicateSuccessor",
            Self::UninhabitedValue => "LcirUninhabitedValue",
            Self::UnreachableBlock => "LcirUnreachableBlock",
            Self::Dominance => "LcirDominance",
        }
    }
}

impl fmt::Display for ValidationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationError {
    code: ValidationCode,
    path: String,
    message: String,
}

impl ValidationError {
    #[must_use]
    pub const fn code(&self) -> ValidationCode {
        self.code
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationErrors {
    errors: Vec<ValidationError>,
}

impl ValidationErrors {
    #[must_use]
    pub fn as_slice(&self) -> &[ValidationError] {
        &self.errors
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.errors.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LCIR validation failed with {} error(s)",
            self.errors.len()
        )
    }
}

impl Error for ValidationErrors {}

/// An owned LCIR program which crossed the complete structural validator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedProgram {
    program: Program,
}

impl CheckedProgram {
    #[must_use]
    pub const fn as_program(&self) -> &Program {
        &self.program
    }

    /// Removes the checked wrapper. The returned value no longer carries the
    /// type-level guarantee required by code-generation consumers.
    #[must_use]
    pub fn into_unchecked(self) -> Program {
        self.program
    }
}

impl Program {
    /// Validates this LCIR program without consuming it.
    ///
    /// # Errors
    ///
    /// Returns all independently discoverable structural failures.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        validate_program(self)
    }

    /// Consumes and validates this LCIR program.
    ///
    /// # Errors
    ///
    /// Returns all independently discoverable structural failures.
    pub fn into_checked(self) -> Result<CheckedProgram, ValidationErrors> {
        check_program(self)
    }
}

/// Validates a borrowed LCIR program.
///
/// # Errors
///
/// Returns all independently discoverable structural failures.
pub fn validate_program(program: &Program) -> Result<(), ValidationErrors> {
    let errors = Validator::new(program).run();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationErrors { errors })
    }
}

/// Validates and wraps an owned LCIR program.
///
/// # Errors
///
/// Returns all independently discoverable structural failures.
pub fn check_program(program: Program) -> Result<CheckedProgram, ValidationErrors> {
    validate_program(&program)?;
    Ok(CheckedProgram { program })
}

struct Validator<'a> {
    program: &'a Program,
    errors: Vec<ValidationError>,
}

impl<'a> Validator<'a> {
    fn new(program: &'a Program) -> Self {
        Self {
            program,
            errors: Vec::new(),
        }
    }

    fn run(mut self) -> Vec<ValidationError> {
        self.validate_representations();
        for (index, function) in self.program.functions.iter().enumerate() {
            let expected = InstanceId::from_index(self.program.brand, index);
            if expected != Some(function.id) {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("function[{index}]"),
                    format!(
                        "function table index {index} carries identity {}",
                        function.id
                    ),
                );
            }
            self.validate_function(function, index);
        }
        self.errors
    }

    fn validate_representations(&mut self) {
        let expected = RepresentationPlan::scalar_with_brand(
            self.program.representations.target(),
            self.program.brand,
        );
        if self.program.representations != expected {
            self.error(
                ValidationCode::RepresentationPlan,
                "representations",
                "scalar LCIR representation table is not canonical for its target",
            );
        }
        for (index, value_type) in self
            .program
            .representations
            .value_types()
            .iter()
            .enumerate()
        {
            if self
                .program
                .representations
                .repr(value_type.repr())
                .is_none()
            {
                self.error(
                    ValidationCode::InvalidTypeReference,
                    format!("representations.type[{index}]"),
                    format!(
                        "value type references missing representation {}",
                        value_type.repr()
                    ),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_function(&mut self, function: &Function, function_index: usize) {
        let base = format!("function[{function_index}]");
        self.validate_signature(function, &base);

        for (index, block) in function.blocks.iter().enumerate() {
            if block.id.owner() != function.id || block.id.index() != index {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("{base}.block[{index}]"),
                    format!("block table index {index} carries identity {}", block.id),
                );
            }
        }
        for (index, instruction) in function.instructions.iter().enumerate() {
            if instruction.id.owner() != function.id || instruction.id.index() != index {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("{base}.instruction[{index}]"),
                    format!(
                        "instruction table index {index} carries identity {}",
                        instruction.id
                    ),
                );
            }
        }
        for (index, value) in function.values.iter().enumerate() {
            if value.id.owner() != function.id || value.id.index() != index {
                self.error(
                    ValidationCode::IndexMismatch,
                    format!("{base}.value[{index}]"),
                    format!("value table index {index} carries identity {}", value.id),
                );
            }
            self.require_type(value.ty, format!("{base}.value[{index}].type"));
            self.require_inhabited_type(value.ty, format!("{base}.value[{index}].type"));
        }
        let schedule = self.validate_schedule(function, &base);
        self.validate_value_definitions(function, &base);
        self.validate_entry(function, &base);

        let mut successors = vec![Vec::new(); function.blocks.len()];
        let mut predecessors = vec![Vec::new(); function.blocks.len()];
        for block_index in 0..function.blocks.len() {
            self.validate_block(
                function,
                block_index,
                &base,
                &mut successors,
                &mut predecessors,
            );
        }

        let Some(entry) = function
            .entry
            .filter(|entry| function.block(*entry).is_some())
        else {
            return;
        };
        if predecessors
            .get(entry.index())
            .is_some_and(|incoming| !incoming.is_empty())
        {
            self.error(
                ValidationCode::EntryPredecessor,
                format!("{base}.entry"),
                "the entry block cannot have a CFG predecessor; use a separate loop header",
            );
        }
        let reachable = reachable_blocks(entry.index(), &successors);
        for (index, is_reachable) in reachable.iter().copied().enumerate() {
            if !is_reachable {
                self.error(
                    ValidationCode::UnreachableBlock,
                    format!("{base}.block[{index}]"),
                    "block is not reachable from the function entry",
                );
            }
        }
        let dominators = compute_dominators(entry.index(), &reachable, &successors, &predecessors);
        self.validate_dominance(function, &base, &schedule, &reachable, &dominators);
    }

    fn validate_signature(&mut self, function: &Function, base: &str) {
        for (index, ty) in function.signature.params().iter().copied().enumerate() {
            self.require_type(ty, format!("{base}.signature.param[{index}]"));
            self.require_inhabited_type(ty, format!("{base}.signature.param[{index}]"));
        }
        self.require_type(
            function.signature.result(),
            format!("{base}.signature.result"),
        );
        self.require_inhabited_type(
            function.signature.result(),
            format!("{base}.signature.result"),
        );
    }

    fn validate_schedule(
        &mut self,
        function: &Function,
        base: &str,
    ) -> Vec<Option<(BlockId, usize)>> {
        let mut schedule = vec![None; function.instructions.len()];
        for (block_index, block) in function.blocks.iter().enumerate() {
            let Some(canonical_block) = BlockId::from_index(function.id, block_index) else {
                continue;
            };
            for (position, instruction) in block.instructions.iter().copied().enumerate() {
                let path = format!("{base}.block[{block_index}].instruction[{position}]");
                if instruction.owner() != function.id {
                    self.error(
                        ValidationCode::InvalidInstructionReference,
                        path,
                        format!("scheduled instruction {instruction} belongs to another function"),
                    );
                    continue;
                }
                let Some(slot) = schedule.get_mut(instruction.index()) else {
                    self.error(
                        ValidationCode::InvalidInstructionReference,
                        path,
                        format!("scheduled instruction {instruction} does not exist"),
                    );
                    continue;
                };
                if let Some((previous, _)) = slot {
                    self.error(
                        ValidationCode::InstructionSchedule,
                        path,
                        format!(
                            "instruction {instruction} is scheduled in both {previous} and {}",
                            block.id
                        ),
                    );
                } else {
                    *slot = Some((canonical_block, position));
                }
            }
        }
        for (index, location) in schedule.iter().enumerate() {
            if location.is_none() {
                self.error(
                    ValidationCode::InstructionSchedule,
                    format!("{base}.instruction[{index}]"),
                    "instruction is not scheduled in any block",
                );
            }
        }
        schedule
    }

    fn validate_value_definitions(&mut self, function: &Function, base: &str) {
        for (block_index, block) in function.blocks.iter().enumerate() {
            let Some(canonical_block) = BlockId::from_index(function.id, block_index) else {
                continue;
            };
            for (index, value) in block.params.iter().copied().enumerate() {
                let path = format!("{base}.block[{block_index}].param[{index}]");
                let Some(definition) = function.value(value).map(|value| value.definition) else {
                    self.error(
                        ValidationCode::InvalidValueReference,
                        path,
                        format!("block parameter {value} does not exist"),
                    );
                    continue;
                };
                let expected = ValueDefinition::BlockParameter {
                    block: canonical_block,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                };
                if definition != expected {
                    self.error(
                        ValidationCode::ValueDefinition,
                        path,
                        format!("{value} has definition {definition:?}, expected {expected:?}"),
                    );
                }
            }
        }
        for (instruction_index, instruction) in function.instructions.iter().enumerate() {
            let Some(canonical_instruction) =
                InstructionId::from_index(function.id, instruction_index)
            else {
                continue;
            };
            for (index, value) in instruction.results.iter().copied().enumerate() {
                let path = format!("{base}.instruction[{instruction_index}].result[{index}]");
                let Some(definition) = function.value(value).map(|value| value.definition) else {
                    self.error(
                        ValidationCode::InvalidValueReference,
                        path,
                        format!("instruction result {value} does not exist"),
                    );
                    continue;
                };
                let expected = ValueDefinition::InstructionResult {
                    instruction: canonical_instruction,
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                };
                if definition != expected {
                    self.error(
                        ValidationCode::ValueDefinition,
                        path,
                        format!("{value} has definition {definition:?}, expected {expected:?}"),
                    );
                }
            }
        }
        for (value_index, value) in function.values.iter().enumerate() {
            let valid = match value.definition {
                ValueDefinition::BlockParameter { block, index } => {
                    function
                        .block(block)
                        .and_then(|block| block.params.get(index as usize))
                        == Some(&value.id)
                }
                ValueDefinition::InstructionResult { instruction, index } => {
                    function
                        .instruction(instruction)
                        .and_then(|instruction| instruction.results.get(index as usize))
                        == Some(&value.id)
                }
            };
            if !valid {
                self.error(
                    ValidationCode::ValueDefinition,
                    format!("{base}.value[{value_index}].definition"),
                    format!("{} is not owned by its declared definition", value.id),
                );
            }
        }
    }

    fn validate_entry(&mut self, function: &Function, base: &str) {
        let Some(entry) = function.entry else {
            self.error(
                ValidationCode::MissingEntry,
                format!("{base}.entry"),
                "function has no entry block",
            );
            return;
        };
        let Some(block) = function.block(entry) else {
            self.error(
                ValidationCode::InvalidBlockReference,
                format!("{base}.entry"),
                format!("entry block {entry} does not exist"),
            );
            return;
        };
        if block.params.len() != function.signature.params().len() {
            self.error(
                ValidationCode::EntrySignature,
                format!("{base}.entry"),
                format!(
                    "entry has {} parameters, signature requires {}",
                    block.params.len(),
                    function.signature.params().len()
                ),
            );
        }
        for (index, (value, expected)) in block
            .params
            .iter()
            .copied()
            .zip(function.signature.params().iter().copied())
            .enumerate()
        {
            self.require_value_type(
                function,
                value,
                expected,
                ValidationCode::EntrySignature,
                format!("{base}.entry.param[{index}]"),
            );
        }
    }

    fn validate_block(
        &mut self,
        function: &Function,
        block_index: usize,
        base: &str,
        successors: &mut [Vec<usize>],
        predecessors: &mut [Vec<usize>],
    ) {
        let block = &function.blocks[block_index];
        for instruction in block.instructions.iter().copied() {
            if let Some(instruction) = function.instruction(instruction) {
                self.validate_instruction(function, instruction, base);
            }
        }
        let Some(terminator) = &block.terminator else {
            self.error(
                ValidationCode::MissingTerminator,
                format!("{base}.block[{block_index}].terminator"),
                "block has no terminator",
            );
            return;
        };
        self.validate_terminator(function, block_index, terminator, base);
        for target in terminator.targets() {
            if function.block(target.block).is_none() {
                continue;
            }
            successors[block_index].push(target.block.index());
            if let Some(incoming) = predecessors.get_mut(target.block.index()) {
                incoming.push(block_index);
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_instruction(&mut self, function: &Function, instruction: &Instruction, base: &str) {
        let path = format!("{base}.instruction[{}]", instruction.id.index());
        if instruction.origin.source_function != function.source() {
            self.error(
                ValidationCode::OriginMismatch,
                format!("{path}.origin"),
                format!(
                    "origin names source function f{}, expected f{}",
                    instruction.origin.source_function.0,
                    function.source().0
                ),
            );
        }
        let unit = self.scalar_type(&Type::Unit);
        let boolean = self.scalar_type(&Type::Bool);
        let integer = self.scalar_type(&Type::Int);
        let float = self.scalar_type(&Type::Float);
        match &instruction.kind {
            InstructionKind::Constant(constant) => {
                let expected = match constant {
                    Constant::Unit => unit,
                    Constant::Bool(_) => boolean,
                    Constant::Int(_) => integer,
                    Constant::FloatBits(_) => float,
                };
                self.require_results(function, instruction, &[expected], &path);
            }
            InstructionKind::BoolNot { value } => {
                self.require_known_value_type(
                    function,
                    *value,
                    boolean,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::FloatBinary { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[float], &path);
            }
            InstructionKind::IntCompare { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    integer,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::FloatCompare { left, right, .. } => {
                self.require_known_value_type(
                    function,
                    *left,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[0]"),
                );
                self.require_known_value_type(
                    function,
                    *right,
                    float,
                    ValidationCode::TypeMismatch,
                    format!("{path}.operand[1]"),
                );
                self.require_results(function, instruction, &[boolean], &path);
            }
            InstructionKind::DirectCall { callee, arguments } => {
                let Some(callee) = self.program.function(*callee) else {
                    self.error(
                        ValidationCode::InvalidFunctionReference,
                        format!("{path}.callee"),
                        format!("callee {callee} does not exist"),
                    );
                    return;
                };
                if !callee.effects.is_empty() {
                    self.error(
                        ValidationCode::CallShape,
                        format!("{path}.callee"),
                        "foundation direct calls require an infallible callee",
                    );
                }
                if arguments.len() != callee.signature.params().len() {
                    self.error(
                        ValidationCode::CallShape,
                        format!("{path}.arguments"),
                        format!(
                            "call has {} arguments, callee requires {}",
                            arguments.len(),
                            callee.signature.params().len()
                        ),
                    );
                }
                for (index, (argument, expected)) in arguments
                    .iter()
                    .copied()
                    .zip(callee.signature.params().iter().copied())
                    .enumerate()
                {
                    self.require_value_type(
                        function,
                        argument,
                        expected,
                        ValidationCode::CallShape,
                        format!("{path}.argument[{index}]"),
                    );
                }
                self.require_results(
                    function,
                    instruction,
                    &[Some(callee.signature.result())],
                    &path,
                );
            }
        }
    }

    fn validate_terminator(
        &mut self,
        function: &Function,
        block_index: usize,
        terminator: &Terminator,
        base: &str,
    ) {
        let path = format!("{base}.block[{block_index}].terminator");
        if terminator.origin.source_function != function.source() {
            self.error(
                ValidationCode::OriginMismatch,
                format!("{path}.origin"),
                format!(
                    "origin names source function f{}, expected f{}",
                    terminator.origin.source_function.0,
                    function.source().0
                ),
            );
        }
        let mut targets = BTreeSet::new();
        for target in terminator.targets() {
            if !targets.insert(target.block) {
                self.error(
                    ValidationCode::DuplicateSuccessor,
                    path.clone(),
                    format!(
                        "terminator has multiple edges from one block to {}; split the edges",
                        target.block
                    ),
                );
            }
        }
        match terminator.kind() {
            TerminatorKind::Jump(target) => {
                self.validate_target(function, target, format!("{path}.target"));
            }
            TerminatorKind::Branch {
                condition,
                then_target,
                else_target,
            } => {
                self.require_known_value_type(
                    function,
                    *condition,
                    self.scalar_type(&Type::Bool),
                    ValidationCode::TypeMismatch,
                    format!("{path}.condition"),
                );
                self.validate_target(function, then_target, format!("{path}.then"));
                self.validate_target(function, else_target, format!("{path}.else"));
            }
            TerminatorKind::Return(value) => {
                self.require_value_type(
                    function,
                    *value,
                    function.signature.result(),
                    ValidationCode::ReturnType,
                    format!("{path}.value"),
                );
            }
            TerminatorKind::Fault { .. } => {
                if !function.effects.contains(Effects::MAY_FAULT) {
                    self.error(
                        ValidationCode::EffectMismatch,
                        path,
                        "fault terminator requires the function's MAY_FAULT effect",
                    );
                }
            }
        }
    }

    fn validate_target(&mut self, function: &Function, target: &crate::BlockTarget, path: String) {
        let Some(block) = function.block(target.block) else {
            self.error(
                ValidationCode::InvalidBlockReference,
                path,
                format!("target block {} does not exist", target.block),
            );
            return;
        };
        if target.arguments.len() != block.params.len() {
            self.error(
                ValidationCode::BlockArgument,
                path.clone(),
                format!(
                    "edge has {} arguments, target {} requires {}",
                    target.arguments.len(),
                    target.block,
                    block.params.len()
                ),
            );
        }
        for (index, (argument, parameter)) in target
            .arguments
            .iter()
            .copied()
            .zip(block.params.iter().copied())
            .enumerate()
        {
            let Some(expected) = function.value(parameter).map(|value| value.ty) else {
                continue;
            };
            self.require_value_type(
                function,
                argument,
                expected,
                ValidationCode::BlockArgument,
                format!("{path}.argument[{index}]"),
            );
        }
    }

    fn validate_dominance(
        &mut self,
        function: &Function,
        base: &str,
        schedule: &[Option<(BlockId, usize)>],
        reachable: &[bool],
        dominators: &DominatorTree,
    ) {
        for (block_index, block) in function.blocks.iter().enumerate() {
            if !reachable.get(block_index).copied().unwrap_or(false) {
                continue;
            }
            let Some(use_block) = BlockId::from_index(function.id, block_index) else {
                continue;
            };
            for (position, instruction_id) in block.instructions.iter().copied().enumerate() {
                let Some(instruction) = function.instruction(instruction_id) else {
                    continue;
                };
                for (operand_index, operand) in instruction.kind.operands().into_iter().enumerate()
                {
                    self.require_dominance(
                        function,
                        operand,
                        use_block,
                        position,
                        schedule,
                        dominators,
                        format!(
                            "{base}.block[{block_index}].instruction[{position}].operand[{operand_index}]"
                        ),
                    );
                }
            }
            if let Some(terminator) = &block.terminator {
                for (operand_index, operand) in terminator.operands().into_iter().enumerate() {
                    self.require_dominance(
                        function,
                        operand,
                        use_block,
                        block.instructions.len(),
                        schedule,
                        dominators,
                        format!("{base}.block[{block_index}].terminator.operand[{operand_index}]"),
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn require_dominance(
        &mut self,
        function: &Function,
        value: ValueId,
        use_block: BlockId,
        use_position: usize,
        schedule: &[Option<(BlockId, usize)>],
        dominators: &DominatorTree,
        path: String,
    ) {
        let Some(value) = function.value(value) else {
            self.error(
                ValidationCode::InvalidValueReference,
                path,
                format!("operand {value} does not exist"),
            );
            return;
        };
        let (definition_block, definition_position) = match value.definition {
            ValueDefinition::BlockParameter { block, .. } => (block, None),
            ValueDefinition::InstructionResult { instruction, .. } => {
                if instruction.owner() != function.id {
                    return;
                }
                let Some(Some((block, position))) = schedule.get(instruction.index()) else {
                    return;
                };
                (*block, Some(*position))
            }
        };
        let dominates = if definition_block == use_block {
            definition_position.is_none_or(|position| position < use_position)
        } else {
            dominators.dominates(definition_block.index(), use_block.index())
        };
        if !dominates {
            self.error(
                ValidationCode::Dominance,
                path,
                format!("{} does not dominate its use in {}", value.id, use_block),
            );
        }
    }

    fn require_results(
        &mut self,
        function: &Function,
        instruction: &Instruction,
        expected: &[Option<ValueTypeId>],
        path: &str,
    ) {
        if instruction.results.len() != expected.len() {
            self.error(
                ValidationCode::InstructionShape,
                format!("{path}.results"),
                format!(
                    "instruction has {} results, operation requires {}",
                    instruction.results.len(),
                    expected.len()
                ),
            );
        }
        for (index, (value, expected)) in instruction
            .results
            .iter()
            .copied()
            .zip(expected.iter().copied())
            .enumerate()
        {
            if let Some(expected) = expected {
                self.require_value_type(
                    function,
                    value,
                    expected,
                    ValidationCode::TypeMismatch,
                    format!("{path}.result[{index}]"),
                );
            }
        }
    }

    fn require_known_value_type(
        &mut self,
        function: &Function,
        value: ValueId,
        expected: Option<ValueTypeId>,
        code: ValidationCode,
        path: String,
    ) {
        if let Some(expected) = expected {
            self.require_value_type(function, value, expected, code, path);
        }
    }

    fn require_value_type(
        &mut self,
        function: &Function,
        value: ValueId,
        expected: ValueTypeId,
        code: ValidationCode,
        path: String,
    ) {
        let Some(value) = function.value(value) else {
            self.error(
                ValidationCode::InvalidValueReference,
                path,
                format!("value {value} does not exist"),
            );
            return;
        };
        if value.ty != expected {
            self.error(
                code,
                path,
                format!("{} has type {}, expected {expected}", value.id, value.ty),
            );
        }
    }

    fn scalar_type(&self, semantic: &Type) -> Option<ValueTypeId> {
        self.program.representations.type_id(semantic)
    }

    fn require_type(&mut self, ty: ValueTypeId, path: String) {
        if self.program.representations.value_type(ty).is_none() {
            self.error(
                ValidationCode::InvalidTypeReference,
                path,
                format!("value type {ty} does not exist"),
            );
        }
    }

    fn require_inhabited_type(&mut self, ty: ValueTypeId, path: String) {
        let is_uninhabited = self
            .program
            .representations
            .value_type(ty)
            .and_then(|value_type| {
                self.program
                    .representations
                    .repr(value_type.repr())
                    .copied()
            })
            == Some(Repr::Uninhabited);
        if is_uninhabited {
            self.error(
                ValidationCode::UninhabitedValue,
                path,
                "the scalar foundation cannot materialize an uninhabited SSA value; lower Never-producing operations as terminators",
            );
        }
    }

    fn error(&mut self, code: ValidationCode, path: impl Into<String>, message: impl Into<String>) {
        self.errors.push(ValidationError {
            code,
            path: path.into(),
            message: message.into(),
        });
    }
}

fn reachable_blocks(entry: usize, successors: &[Vec<usize>]) -> Vec<bool> {
    let mut reachable = vec![false; successors.len()];
    let mut pending = vec![entry];
    while let Some(block) = pending.pop() {
        let Some(is_reachable) = reachable.get_mut(block) else {
            continue;
        };
        if *is_reachable {
            continue;
        }
        *is_reachable = true;
        if let Some(next) = successors.get(block) {
            pending.extend(
                next.iter()
                    .copied()
                    .filter(|target| *target < successors.len()),
            );
        }
    }
    reachable
}

#[derive(Clone, Copy)]
struct DominatorInterval {
    start: usize,
    end: usize,
}

struct DominatorTree {
    intervals: Vec<Option<DominatorInterval>>,
}

impl DominatorTree {
    fn dominates(&self, dominator: usize, block: usize) -> bool {
        let Some(Some(dominator)) = self.intervals.get(dominator) else {
            return false;
        };
        let Some(Some(block)) = self.intervals.get(block) else {
            return false;
        };
        dominator.start <= block.start && block.start < dominator.end
    }
}

/// Computes immediate dominators in reverse postorder, then assigns intervals
/// in the dominator tree. This avoids materializing one set per block and
/// makes every later dominance query constant time.
fn compute_dominators(
    entry: usize,
    reachable: &[bool],
    successors: &[Vec<usize>],
    predecessors: &[Vec<usize>],
) -> DominatorTree {
    let block_count = reachable.len();
    let order = reverse_postorder(entry, reachable, successors);
    let mut order_index = vec![usize::MAX; block_count];
    for (index, block) in order.iter().copied().enumerate() {
        if let Some(slot) = order_index.get_mut(block) {
            *slot = index;
        }
    }

    let mut immediate = vec![None; block_count];
    if entry < block_count {
        immediate[entry] = Some(entry);
    }
    loop {
        let mut changed = false;
        for block in order.iter().copied().skip(1) {
            let Some(incoming) = predecessors.get(block) else {
                continue;
            };
            let mut defined = incoming.iter().copied().filter(|predecessor| {
                reachable.get(*predecessor).copied().unwrap_or(false)
                    && immediate.get(*predecessor).is_some_and(Option::is_some)
            });
            let Some(first) = defined.next() else {
                continue;
            };
            let mut next = first;
            for predecessor in defined {
                let Some(common) =
                    intersect_dominators(next, predecessor, &immediate, &order_index)
                else {
                    continue;
                };
                next = common;
            }
            if immediate.get(block).copied().flatten() != Some(next)
                && let Some(slot) = immediate.get_mut(block)
            {
                *slot = Some(next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut children = vec![Vec::new(); block_count];
    for (block, parent) in immediate.iter().copied().enumerate() {
        if block == entry || !reachable.get(block).copied().unwrap_or(false) {
            continue;
        }
        if let Some(parent) = parent
            && let Some(parent_children) = children.get_mut(parent)
        {
            parent_children.push(block);
        }
    }
    DominatorTree {
        intervals: dominator_intervals(entry, &children),
    }
}

fn reverse_postorder(entry: usize, reachable: &[bool], successors: &[Vec<usize>]) -> Vec<usize> {
    let mut visited = vec![false; successors.len()];
    let mut postorder = Vec::new();
    if !reachable.get(entry).copied().unwrap_or(false) {
        return postorder;
    }
    visited[entry] = true;
    let mut stack = vec![(entry, 0_usize)];
    while let Some((block, next_index)) = stack.last_mut() {
        let Some(next) = successors
            .get(*block)
            .and_then(|targets| targets.get(*next_index))
            .copied()
        else {
            postorder.push(*block);
            stack.pop();
            continue;
        };
        *next_index += 1;
        if visited.get(next).is_some_and(|seen| !seen) {
            visited[next] = true;
            stack.push((next, 0));
        }
    }
    postorder.reverse();
    postorder
}

fn intersect_dominators(
    mut left: usize,
    mut right: usize,
    immediate: &[Option<usize>],
    order_index: &[usize],
) -> Option<usize> {
    while left != right {
        while *order_index.get(left)? > *order_index.get(right)? {
            left = immediate.get(left).copied().flatten()?;
        }
        while *order_index.get(right)? > *order_index.get(left)? {
            right = immediate.get(right).copied().flatten()?;
        }
    }
    Some(left)
}

fn dominator_intervals(entry: usize, children: &[Vec<usize>]) -> Vec<Option<DominatorInterval>> {
    let mut intervals = vec![None; children.len()];
    if entry >= children.len() {
        return intervals;
    }
    let mut next = 0_usize;
    let mut stack = vec![(entry, false)];
    while let Some((block, exiting)) = stack.pop() {
        if exiting {
            if let Some(Some(interval)) = intervals.get_mut(block) {
                interval.end = next;
            }
            continue;
        }
        if let Some(slot) = intervals.get_mut(block) {
            *slot = Some(DominatorInterval {
                start: next,
                end: next,
            });
        }
        next = next.saturating_add(1);
        stack.push((block, true));
        if let Some(block_children) = children.get(block) {
            stack.extend(
                block_children
                    .iter()
                    .rev()
                    .copied()
                    .map(|child| (child, false)),
            );
        }
    }
    intervals
}

#[cfg(test)]
mod tests {
    use loom_mir::FunctionId as MirFunctionId;

    use super::*;
    use crate::{Constant, Origin, ProgramBuilder, Signature, TargetLayout, Terminator};

    #[test]
    fn malformed_block_identity_does_not_enter_cfg_indices() {
        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(90)),
                "malformed.block_identity",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function_builder = builder.function(function).expect("builder");
            let entry = function_builder.create_block().expect("entry");
            let exit = function_builder.create_block().expect("exit");
            function_builder.set_entry(entry).expect("set entry");
            let unit = function_builder
                .append_instruction(
                    entry,
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    Origin::synthetic(MirFunctionId(90)),
                )
                .expect("unit")[0];
            function_builder
                .terminate(
                    entry,
                    Terminator::new(
                        TerminatorKind::Jump(crate::BlockTarget::new(exit, Vec::new())),
                        Origin::synthetic(MirFunctionId(90)),
                    ),
                )
                .expect("jump");
            function_builder
                .terminate(
                    exit,
                    Terminator::new(
                        TerminatorKind::Return(unit),
                        Origin::synthetic(MirFunctionId(90)),
                    ),
                )
                .expect("return");
        }
        let mut program = builder.finish();
        program.functions[0].blocks[0].id =
            BlockId::from_index(function, 99).expect("malformed identity");

        let errors = validate_program(&program).expect_err("corruption must be rejected");
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|error| error.code() == ValidationCode::IndexMismatch)
        );
    }

    #[test]
    fn reverse_table_order_linear_cfg_uses_bounded_dominator_state() {
        const BLOCK_COUNT: usize = 2_048;

        let mut builder = ProgramBuilder::new(TargetLayout::new(64).expect("target"));
        let unit_ty = builder.type_id(&Type::Unit).expect("Unit type");
        let function = builder
            .declare_function(
                Origin::synthetic(MirFunctionId(92)),
                "dominator.reverse_linear",
                Signature::new(Vec::new(), unit_ty),
                Effects::NONE,
            )
            .expect("declare");
        {
            let mut function_builder = builder.function(function).expect("builder");
            let blocks = (0..BLOCK_COUNT)
                .map(|_| function_builder.create_block().expect("block"))
                .collect::<Vec<_>>();
            function_builder.set_entry(blocks[0]).expect("set entry");
            let unit = function_builder
                .append_instruction(
                    blocks[0],
                    InstructionKind::Constant(Constant::Unit),
                    &[unit_ty],
                    Origin::synthetic(MirFunctionId(92)),
                )
                .expect("unit")[0];
            function_builder
                .terminate(
                    blocks[0],
                    Terminator::new(
                        TerminatorKind::Jump(crate::BlockTarget::new(
                            blocks[BLOCK_COUNT - 1],
                            Vec::new(),
                        )),
                        Origin::synthetic(MirFunctionId(92)),
                    ),
                )
                .expect("entry jump");
            for source in (2..BLOCK_COUNT).rev() {
                function_builder
                    .terminate(
                        blocks[source],
                        Terminator::new(
                            TerminatorKind::Jump(crate::BlockTarget::new(
                                blocks[source - 1],
                                Vec::new(),
                            )),
                            Origin::synthetic(MirFunctionId(92)),
                        ),
                    )
                    .expect("linear jump");
            }
            function_builder
                .terminate(
                    blocks[1],
                    Terminator::new(
                        TerminatorKind::Return(unit),
                        Origin::synthetic(MirFunctionId(92)),
                    ),
                )
                .expect("return");
        }

        builder
            .finish_checked()
            .expect("reverse table-order linear CFG is valid");
    }
}
