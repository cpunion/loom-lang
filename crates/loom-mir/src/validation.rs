use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::ops::Deref;

use loom_core::Span;

use crate::{
    BinaryOp, Block, Builtin, CallArgument, CallTarget, ConceptDef, ConceptId, Constant,
    ConstructionMode, Contract, ContractArm, ContractExpr, ContractExprKind, ContractValue, Expr,
    ExprKind, Function, FunctionId, LocalDecl, LocalId, MatchArm, Pattern, Place, Program,
    Receiver, RequirementDef, RequirementId, RequirementType, RequirementWitnessParam, Statement,
    StatementKind, TaskJoinMode, Type, TypeDef, TypeDefKind, UnaryOp, VariantId, Witness,
    WitnessParam, WitnessRef,
};

const MAX_VALIDATION_DEPTH: u16 = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MirValidationCode {
    IndexMismatch,
    InvalidTypeReference,
    InvalidFunctionReference,
    InvalidConceptReference,
    InvalidRequirementReference,
    InvalidWitnessReference,
    InvalidVariantReference,
    InvalidLocalReference,
    DuplicateLocal,
    InvalidPlace,
    ImmutablePlace,
    ProjectedMove,
    TypeMismatch,
    ExpressionIdentity,
    ExpressionShape,
    ObligationShape,
    CallArity,
    WitnessArity,
    RecordShape,
    VariantShape,
    PatternShape,
    WitnessShape,
    ConceptShape,
    RequirementShape,
    ContractShape,
    ReceiverShape,
    LocalState,
    BorrowShape,
    BuiltinShape,
    ErrorType,
    NestingLimit,
}

impl MirValidationCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IndexMismatch => "MirIndexMismatch",
            Self::InvalidTypeReference => "MirInvalidTypeReference",
            Self::InvalidFunctionReference => "MirInvalidFunctionReference",
            Self::InvalidConceptReference => "MirInvalidConceptReference",
            Self::InvalidRequirementReference => "MirInvalidRequirementReference",
            Self::InvalidWitnessReference => "MirInvalidWitnessReference",
            Self::InvalidVariantReference => "MirInvalidVariantReference",
            Self::InvalidLocalReference => "MirInvalidLocalReference",
            Self::DuplicateLocal => "MirDuplicateLocal",
            Self::InvalidPlace => "MirInvalidPlace",
            Self::ImmutablePlace => "MirImmutablePlace",
            Self::ProjectedMove => "MirProjectedMove",
            Self::TypeMismatch => "MirTypeMismatch",
            Self::ExpressionIdentity => "MirExpressionIdentity",
            Self::ExpressionShape => "MirExpressionShape",
            Self::ObligationShape => "MirObligationShape",
            Self::CallArity => "MirCallArity",
            Self::WitnessArity => "MirWitnessArity",
            Self::RecordShape => "MirRecordShape",
            Self::VariantShape => "MirVariantShape",
            Self::PatternShape => "MirPatternShape",
            Self::WitnessShape => "MirWitnessShape",
            Self::ConceptShape => "MirConceptShape",
            Self::RequirementShape => "MirRequirementShape",
            Self::ContractShape => "MirContractShape",
            Self::ReceiverShape => "MirReceiverShape",
            Self::LocalState => "MirLocalState",
            Self::BorrowShape => "MirBorrowShape",
            Self::BuiltinShape => "MirBuiltinShape",
            Self::ErrorType => "MirErrorType",
            Self::NestingLimit => "MirNestingLimit",
        }
    }
}

impl fmt::Display for MirValidationCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirValidationError {
    pub code: MirValidationCode,
    pub message: String,
    pub span: Span,
    /// Stable structural location, independent of source filenames.
    pub path: String,
}

impl fmt::Display for MirValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirValidationErrors {
    errors: Vec<MirValidationError>,
}

impl MirValidationErrors {
    #[must_use]
    pub fn as_slice(&self) -> &[MirValidationError] {
        &self.errors
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    #[must_use]
    pub fn contains(&self, code: MirValidationCode) -> bool {
        self.errors.iter().any(|error| error.code == code)
    }
}

impl Deref for MirValidationErrors {
    type Target = [MirValidationError];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

impl IntoIterator for MirValidationErrors {
    type IntoIter = std::vec::IntoIter<MirValidationError>;
    type Item = MirValidationError;

    fn into_iter(self) -> Self::IntoIter {
        self.errors.into_iter()
    }
}

impl fmt::Display for MirValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "MIR validation failed with {} error(s)",
            self.len()
        )
    }
}

impl Error for MirValidationErrors {}

/// An owned program which has crossed the complete MIR validation boundary.
#[derive(Clone, Debug)]
pub struct CheckedProgram {
    program: Program,
}

impl CheckedProgram {
    /// Validates and wraps an unchecked program.
    ///
    /// # Errors
    ///
    /// Returns every independently discoverable structural error.
    pub fn new(program: Program) -> Result<Self, MirValidationErrors> {
        check_program(program)
    }

    #[must_use]
    pub const fn as_program(&self) -> &Program {
        &self.program
    }

    #[must_use]
    pub fn into_program(self) -> Program {
        self.program
    }
}

impl AsRef<Program> for CheckedProgram {
    fn as_ref(&self) -> &Program {
        self.as_program()
    }
}

impl Deref for CheckedProgram {
    type Target = Program;

    fn deref(&self) -> &Self::Target {
        self.as_program()
    }
}

/// Validates a borrowed MIR program without mutating it.
///
/// # Errors
///
/// Returns every independently discoverable structural error.
pub fn validate_program(program: &Program) -> Result<(), MirValidationErrors> {
    let errors = Validator::new(program).run();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(MirValidationErrors { errors })
    }
}

/// Validates and converts an owned program into [`CheckedProgram`].
///
/// # Errors
///
/// Returns every independently discoverable structural error.
pub fn check_program(program: Program) -> Result<CheckedProgram, MirValidationErrors> {
    validate_program(&program)?;
    Ok(CheckedProgram { program })
}

#[derive(Clone)]
struct ContractEnv {
    receiver: Option<Type>,
    result: Option<Type>,
    arguments: Vec<Type>,
    bindings: Vec<Type>,
    allow_old: bool,
}

#[derive(Clone)]
struct ResolvedWitness {
    definition: Option<crate::WitnessId>,
    proof: WitnessParam,
    /// When this proof is a function witness parameter, an unbound
    /// associated requirement remains an executable projection through this
    /// frame slot instead of becoming an invalid missing binding.
    projection_witness: Option<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SlotState {
    Uninitialized,
    Available,
    Moved,
    MaybeUnavailable,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PlaceLoan {
    owner: Place,
    mutable: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum BorrowedViewPosition {
    DirectCallArgument,
    Other,
}

#[derive(Clone)]
struct DataflowState {
    slots: Vec<SlotState>,
    /// Possible loans carried by each local. A branch join may retain more
    /// than one conservative possibility for the same carrier.
    view_loans: Vec<Vec<PlaceLoan>>,
    /// Accesses established by arguments of every active call expression.
    /// Each call restores its entry checkpoint only after every argument has
    /// finished evaluating.
    temporary_loans: Vec<PlaceLoan>,
}

struct ExprFlow {
    diverges: bool,
    loans: Vec<PlaceLoan>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ValueObligations {
    task: bool,
    resource: bool,
    unresolved: bool,
}

impl ValueObligations {
    fn merge(&mut self, other: Self) {
        self.task |= other.task;
        self.resource |= other.resource;
        self.unresolved |= other.unresolved;
    }

    const fn is_empty(self) -> bool {
        !self.task && !self.resource && !self.unresolved
    }
}

#[derive(Clone, Copy)]
enum MethodTypes<'types> {
    /// Method parameters stay generic in a witness implementation and begin
    /// after the conformance's own generic parameters.
    ParametersFrom(u32),
    /// A call site supplies a concrete/generic instantiation explicitly.
    Arguments(&'types [Type]),
}

#[derive(Clone, Copy)]
enum RequirementProofs<'proofs> {
    /// Requirement proof projections stay symbolic in a witness method body.
    /// Method-specific proofs follow the conformance prerequisites.
    FunctionParametersFrom(u32),
    /// A static call resolves projections through its method proof arguments.
    Resolved(&'proofs [Option<ResolvedWitness>]),
    /// Dynamic requirements cannot declare method-specific proofs.
    Unavailable,
}

struct Validator<'program> {
    program: &'program Program,
    errors: Vec<MirValidationError>,
    nesting_failed: bool,
}

impl<'program> Validator<'program> {
    fn new(program: &'program Program) -> Self {
        Self {
            program,
            errors: Vec::new(),
            nesting_failed: false,
        }
    }

    fn run(mut self) -> Vec<MirValidationError> {
        self.validate_definition_indices();
        for (index, definition) in self.program.types.iter().enumerate() {
            self.validate_type_definition(definition, &format!("types[{index}]"));
            if self.nesting_failed {
                return self.errors;
            }
        }
        for (index, concept) in self.program.concepts.iter().enumerate() {
            self.validate_concept(concept, &format!("concepts[{index}]"));
            if self.nesting_failed {
                return self.errors;
            }
        }
        for (index, requirement) in self.program.requirements.iter().enumerate() {
            self.validate_requirement(requirement, &format!("requirements[{index}]"));
            if self.nesting_failed {
                return self.errors;
            }
        }
        for (index, witness) in self.program.witnesses.iter().enumerate() {
            self.validate_witness(witness, &format!("witnesses[{index}]"));
            if self.nesting_failed {
                return self.errors;
            }
        }
        self.validate_witness_coherence();
        for (index, function) in self.program.functions.iter().enumerate() {
            self.validate_function(function, &format!("functions[{index}]"));
            if self.nesting_failed {
                return self.errors;
            }
        }
        self.validate_roots();
        self.errors
    }

    fn validate_definition_indices(&mut self) {
        for (index, definition) in self.program.types.iter().enumerate() {
            if definition.id.0 as usize != index {
                self.push(
                    MirValidationCode::IndexMismatch,
                    format!(
                        "type vector position {index} contains id #{}; ids are direct indices",
                        definition.id.0
                    ),
                    definition.span,
                    format!("types[{index}].id"),
                );
            }
        }
        for (index, function) in self.program.functions.iter().enumerate() {
            if function.id.0 as usize != index {
                self.push(
                    MirValidationCode::IndexMismatch,
                    format!(
                        "function vector position {index} contains id #{}; ids are direct indices",
                        function.id.0
                    ),
                    function.span,
                    format!("functions[{index}].id"),
                );
            }
        }
        for (index, concept) in self.program.concepts.iter().enumerate() {
            if concept.id.0 as usize != index {
                self.push(
                    MirValidationCode::IndexMismatch,
                    format!(
                        "concept vector position {index} contains id #{}; ids are direct indices",
                        concept.id.0
                    ),
                    concept.span,
                    format!("concepts[{index}].id"),
                );
            }
        }
        for (index, requirement) in self.program.requirements.iter().enumerate() {
            if requirement.id.0 as usize != index {
                self.push(
                    MirValidationCode::IndexMismatch,
                    format!(
                        "requirement vector position {index} contains id #{}; ids are direct indices",
                        requirement.id.0
                    ),
                    requirement.span,
                    format!("requirements[{index}].id"),
                );
            }
        }
        for (index, witness) in self.program.witnesses.iter().enumerate() {
            if witness.id.0 as usize != index {
                self.push(
                    MirValidationCode::IndexMismatch,
                    format!(
                        "witness vector position {index} contains id #{}; ids are direct indices",
                        witness.id.0
                    ),
                    Span::default(),
                    format!("witnesses[{index}].id"),
                );
            }
        }
    }

    fn validate_roots(&mut self) {
        self.validate_prelude_ids();
        for (index, function_id) in self.program.tests.iter().copied().enumerate() {
            let path = format!("tests[{index}]");
            let Some(function) = self.program.function(function_id) else {
                self.push(
                    MirValidationCode::InvalidFunctionReference,
                    format!("test references unknown function #{}", function_id.0),
                    Span::default(),
                    path,
                );
                continue;
            };
            if !function.params.is_empty() {
                self.push(
                    MirValidationCode::CallArity,
                    "test entry points must accept zero value arguments",
                    function.span,
                    path.clone(),
                );
            }
            if !function.witness_params.is_empty() {
                self.push(
                    MirValidationCode::WitnessArity,
                    "test entry points must accept zero witness arguments",
                    function.span,
                    path,
                );
            }
            if function.type_parameters != 0 {
                self.push(
                    MirValidationCode::CallArity,
                    "test entry points must be monomorphic",
                    function.span,
                    format!("tests[{index}]"),
                );
            }
        }
        for (name, function_id) in &self.program.exports {
            if self.program.function(*function_id).is_none() {
                self.push(
                    MirValidationCode::InvalidFunctionReference,
                    format!(
                        "export `{name}` references unknown function #{}",
                        function_id.0
                    ),
                    Span::default(),
                    format!("exports[{name:?}]"),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_prelude_ids(&mut self) {
        let entries = [
            ("result", self.program.prelude.result, "enum"),
            ("option", self.program.prelude.option, "enum"),
            (
                "constraint_error",
                self.program.prelude.constraint_error,
                "record",
            ),
            (
                "parse_float_error",
                self.program.prelude.parse_float_error,
                "enum",
            ),
            (
                "parse_int_error",
                self.program.prelude.parse_int_error,
                "enum",
            ),
            ("task_fault", self.program.prelude.task_fault, "record"),
            ("task_outcome", self.program.prelude.task_outcome, "enum"),
            ("duration", self.program.prelude.duration, "record"),
            ("file", self.program.prelude.file, "record"),
            ("socket", self.program.prelude.socket, "record"),
            ("bytes", self.program.prelude.bytes, "record"),
            ("path", self.program.prelude.path, "record"),
            (
                "decode_text_error",
                self.program.prelude.decode_text_error,
                "enum",
            ),
            ("path_error", self.program.prelude.path_error, "enum"),
            ("text_map", self.program.prelude.text_map, "record"),
            ("json", self.program.prelude.json, "enum"),
            ("json_error", self.program.prelude.json_error, "enum"),
            ("io_error", self.program.prelude.io_error, "record"),
            ("io_error_kind", self.program.prelude.io_error_kind, "enum"),
            ("log_level", self.program.prelude.log_level, "enum"),
        ];
        for (name, id, expected_kind) in entries {
            let Some(id) = id else {
                continue;
            };
            let Some(definition) = self.program.type_def(id) else {
                self.invalid_type(id, Span::default(), format!("prelude.{name}"));
                continue;
            };
            let kind_matches = matches!(
                (&definition.kind, expected_kind),
                (TypeDefKind::Enum { .. }, "enum") | (TypeDefKind::Record { .. }, "record")
            );
            if !kind_matches {
                self.push(
                    MirValidationCode::RecordShape,
                    format!("prelude `{name}` must reference a {expected_kind} type"),
                    definition.span,
                    format!("prelude.{name}"),
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .result
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 2
                        && variants.len() == 2
                        && variants[0].id == VariantId(0)
                        && variants[0].payload == [Type::Parameter(0)]
                        && variants[1].id == VariantId(1)
                        && variants[1].payload == [Type::Parameter(1)]
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude Result must use variants #0(T) and #1(E)",
                    definition.span,
                    "prelude.result",
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .task_fault
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Record { fields, invariant: None }
                    if definition.type_parameters == 0
                        && fields.len() == 2
                        && fields[0].name == "code"
                        && fields[0].ty == Type::Text
                        && fields[1].name == "message"
                        && fields[1].ty == Type::Text
            );
            if !valid {
                self.push(
                    MirValidationCode::RecordShape,
                    "prelude TaskFault must be a { code Text, message Text } record",
                    definition.span,
                    "prelude.task_fault",
                );
            }
        }
        if let (Some(task_fault), Some(definition)) = (
            self.program.prelude.task_fault,
            self.program
                .prelude
                .task_outcome
                .and_then(|id| self.program.type_def(id)),
        ) {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 1
                        && variants.len() == 3
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "Completed"
                        && variants[0].payload == [Type::Parameter(0)]
                        && variants[1].id == VariantId(1)
                        && variants[1].name == "Faulted"
                        && variants[1].payload == [Type::Nominal(task_fault, Vec::new())]
                        && variants[2].id == VariantId(2)
                        && variants[2].name == "Cancelled"
                        && variants[2].payload.is_empty()
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude TaskOutcome must use Completed#0(T), Faulted#1(TaskFault), and Cancelled#2",
                    definition.span,
                    "prelude.task_outcome",
                );
            }
        }
        for (name, id) in [
            ("Duration", self.program.prelude.duration),
            ("File", self.program.prelude.file),
            ("Socket", self.program.prelude.socket),
        ] {
            let Some(definition) = id.and_then(|id| self.program.type_def(id)) else {
                continue;
            };
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Record { fields, invariant: None }
                    if definition.type_parameters == 0
                        && fields.len() == 1
                        && fields[0].name == "raw"
                        && fields[0].ty == Type::Int
            );
            if !valid {
                self.push(
                    MirValidationCode::RecordShape,
                    format!("prelude {name} must be a non-generic {{ raw Int }} record"),
                    definition.span,
                    format!("prelude.{}", name.to_ascii_lowercase()),
                );
            }
        }
        for (name, id) in [
            ("Bytes", self.program.prelude.bytes),
            ("Path", self.program.prelude.path),
        ] {
            let Some(definition) = id.and_then(|id| self.program.type_def(id)) else {
                continue;
            };
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Record { fields, invariant: None }
                    if definition.type_parameters == 0
                        && fields.len() == 1
                        && fields[0].name == "raw"
                        && fields[0].ty == Type::Text
            );
            if !valid {
                self.push(
                    MirValidationCode::RecordShape,
                    format!("prelude {name} must be a non-generic {{ raw Text }} record"),
                    definition.span,
                    format!("prelude.{}", name.to_ascii_lowercase()),
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .parse_int_error
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 0
                        && variants.len() == 2
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "InvalidSyntax"
                        && variants[0].payload.is_empty()
                        && variants[1].id == VariantId(1)
                        && variants[1].name == "OutOfRange"
                        && variants[1].payload.is_empty()
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude ParseIntError must use empty InvalidSyntax#0 and OutOfRange#1 variants",
                    definition.span,
                    "prelude.parse_int_error",
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .decode_text_error
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 0
                        && variants.len() == 1
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "InvalidUtf8"
                        && variants[0].payload.is_empty()
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude DecodeTextError must use empty InvalidUtf8#0",
                    definition.span,
                    "prelude.decode_text_error",
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .path_error
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 0
                        && variants.len() == 2
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "ContainsNul"
                        && variants[0].payload.is_empty()
                        && variants[1].id == VariantId(1)
                        && variants[1].name == "AbsoluteJoin"
                        && variants[1].payload.is_empty()
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude PathError must use empty ContainsNul#0 and AbsoluteJoin#1 variants",
                    definition.span,
                    "prelude.path_error",
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .text_map
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Record { fields, invariant: None }
                    if definition.type_parameters == 1
                        && fields.len() == 1
                        && fields[0].name == "raw"
                        && fields[0].ty == Type::Int
            );
            if !valid {
                self.push(
                    MirValidationCode::RecordShape,
                    "prelude TextMap must be a unary generic { raw Int } record",
                    definition.span,
                    "prelude.text_map",
                );
            }
        }
        if let (Some(text_map), Some(definition)) = (
            self.program.prelude.text_map,
            self.program
                .prelude
                .json
                .and_then(|id| self.program.type_def(id)),
        ) {
            let self_ty = Type::Nominal(definition.id, Vec::new());
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 0
                        && variants.len() == 6
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "Null"
                        && variants[0].payload.is_empty()
                        && variants[1].id == VariantId(1)
                        && variants[1].name == "Bool"
                        && variants[1].payload == [Type::Bool]
                        && variants[2].id == VariantId(2)
                        && variants[2].name == "Number"
                        && variants[2].payload == [Type::Float]
                        && variants[3].id == VariantId(3)
                        && variants[3].name == "Text"
                        && variants[3].payload == [Type::Text]
                        && variants[4].id == VariantId(4)
                        && variants[4].name == "Array"
                        && variants[4].payload == [Type::List(Box::new(self_ty.clone()))]
                        && variants[5].id == VariantId(5)
                        && variants[5].name == "Object"
                        && variants[5].payload
                            == [Type::Nominal(text_map, vec![self_ty])]
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude Json must use canonical Null/Bool/Number/Text/Array/Object variants",
                    definition.span,
                    "prelude.json",
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .json_error
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 0
                        && variants.len() == 4
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "InvalidSyntax"
                        && variants[0].payload == [Type::Int]
                        && variants[1].id == VariantId(1)
                        && variants[1].name == "NumberOutOfRange"
                        && variants[1].payload == [Type::Int]
                        && variants[2].id == VariantId(2)
                        && variants[2].name == "DepthLimit"
                        && variants[2].payload.is_empty()
                        && variants[3].id == VariantId(3)
                        && variants[3].name == "NonFiniteNumber"
                        && variants[3].payload.is_empty()
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude JsonError must use canonical offset/depth/non-finite variants",
                    definition.span,
                    "prelude.json_error",
                );
            }
        }
        if let (Some(kind), Some(definition)) = (
            self.program.prelude.io_error_kind,
            self.program
                .prelude
                .io_error
                .and_then(|id| self.program.type_def(id)),
        ) {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Record { fields, invariant: None }
                    if definition.type_parameters == 0
                        && fields.len() == 2
                        && fields[0].name == "kind"
                        && fields[0].ty == Type::Nominal(kind, Vec::new())
                        && fields[1].name == "message"
                        && fields[1].ty == Type::Text
            );
            if !valid {
                self.push(
                    MirValidationCode::RecordShape,
                    "prelude IoError must be { kind IoErrorKind, message Text }",
                    definition.span,
                    "prelude.io_error",
                );
            }
        }
        for (name, id, variants) in [
            (
                "io_error_kind",
                self.program.prelude.io_error_kind,
                &[
                    "NotFound",
                    "PermissionDenied",
                    "AlreadyExists",
                    "InvalidInput",
                    "ConnectionRefused",
                    "ConnectionReset",
                    "TimedOut",
                    "UnexpectedEof",
                    "Closed",
                    "Other",
                ][..],
            ),
            (
                "log_level",
                self.program.prelude.log_level,
                &["Debug", "Info", "Warn", "Error"][..],
            ),
        ] {
            let Some(definition) = id.and_then(|id| self.program.type_def(id)) else {
                continue;
            };
            let valid = matches!(&definition.kind, TypeDefKind::Enum { variants: actual }
            if definition.type_parameters == 0
                && actual.len() == variants.len()
                && actual.iter().zip(variants).enumerate().all(|(index, (variant, name))| {
                    variant.id == VariantId(u32::try_from(index).unwrap_or(u32::MAX))
                        && variant.name == *name
                        && variant.payload.is_empty()
                }));
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    format!("prelude {name} does not use its canonical closed variants"),
                    definition.span,
                    format!("prelude.{name}"),
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .constraint_error
            .and_then(|id| self.program.type_def(id))
            && (!matches!(definition.kind, TypeDefKind::Record { .. })
                || definition.type_parameters != 0)
        {
            self.push(
                MirValidationCode::RecordShape,
                "prelude ConstraintError must be a non-generic record",
                definition.span,
                "prelude.constraint_error",
            );
        }
        if let Some(definition) = self
            .program
            .prelude
            .option
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 1
                        && variants.len() == 2
                        && variants[0].id == VariantId(0)
                        && variants[0].payload.is_empty()
                        && variants[1].id == VariantId(1)
                        && variants[1].payload == [Type::Parameter(0)]
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude Option must use variants #0() and #1(T)",
                    definition.span,
                    "prelude.option",
                );
            }
        }
        if let Some(definition) = self
            .program
            .prelude
            .parse_float_error
            .and_then(|id| self.program.type_def(id))
        {
            let valid = matches!(
                &definition.kind,
                TypeDefKind::Enum { variants }
                    if definition.type_parameters == 0
                        && variants.len() == 2
                        && variants[0].id == VariantId(0)
                        && variants[0].name == "InvalidSyntax"
                        && variants[0].payload.is_empty()
                        && variants[1].id == VariantId(1)
                        && variants[1].name == "OutOfRange"
                        && variants[1].payload.is_empty()
            );
            if !valid {
                self.push(
                    MirValidationCode::VariantShape,
                    "prelude ParseFloatError must use empty InvalidSyntax#0 and OutOfRange#1 variants",
                    definition.span,
                    "prelude.parse_float_error",
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_type_definition(&mut self, definition: &TypeDef, path: &str) {
        match &definition.kind {
            TypeDefKind::Record { fields, invariant } => {
                for (index, field) in fields.iter().enumerate() {
                    self.validate_type(
                        &field.ty,
                        field.span,
                        &format!("{path}.fields[{index}].ty"),
                        0,
                    );
                    self.reject_frame_projection(
                        &field.ty,
                        field.span,
                        &format!("{path}.fields[{index}].ty"),
                        0,
                    );
                    self.validate_view_placement(
                        &field.ty,
                        false,
                        field.span,
                        &format!("{path}.fields[{index}].ty"),
                        0,
                    );
                    self.validate_type_parameter_arity(
                        &field.ty,
                        definition.type_parameters,
                        field.span,
                        &format!("{path}.fields[{index}].ty"),
                        0,
                    );
                }
                if let Some(contract) = invariant {
                    let receiver = Self::nominal_self_type(definition);
                    self.validate_contract(
                        contract,
                        &ContractEnv {
                            receiver: Some(receiver),
                            result: None,
                            arguments: Vec::new(),
                            bindings: Vec::new(),
                            allow_old: false,
                        },
                        &format!("{path}.invariant"),
                    );
                }
            }
            TypeDefKind::Enum { variants } => {
                for (index, variant) in variants.iter().enumerate() {
                    let variant_path = format!("{path}.variants[{index}]");
                    if variant.id.0 as usize != index {
                        self.push(
                            MirValidationCode::IndexMismatch,
                            format!(
                                "variant vector position {index} contains id #{}",
                                variant.id.0
                            ),
                            variant.span,
                            format!("{variant_path}.id"),
                        );
                    }
                    for (payload_index, ty) in variant.payload.iter().enumerate() {
                        self.validate_type(
                            ty,
                            variant.span,
                            &format!("{variant_path}.payload[{payload_index}]"),
                            0,
                        );
                        self.reject_frame_projection(
                            ty,
                            variant.span,
                            &format!("{variant_path}.payload[{payload_index}]"),
                            0,
                        );
                        self.validate_view_placement(
                            ty,
                            false,
                            variant.span,
                            &format!("{variant_path}.payload[{payload_index}]"),
                            0,
                        );
                        self.validate_type_parameter_arity(
                            ty,
                            definition.type_parameters,
                            variant.span,
                            &format!("{variant_path}.payload[{payload_index}]"),
                            0,
                        );
                    }
                }
            }
            TypeDefKind::Refined { base, predicate } => {
                if definition.type_parameters != 0 {
                    self.push(
                        MirValidationCode::TypeMismatch,
                        "Core refined types cannot declare generic parameters",
                        definition.span,
                        format!("{path}.type_parameters"),
                    );
                }
                self.validate_type(base, definition.span, &format!("{path}.base"), 0);
                self.reject_frame_projection(base, definition.span, &format!("{path}.base"), 0);
                self.validate_view_placement(
                    base,
                    false,
                    definition.span,
                    &format!("{path}.base"),
                    0,
                );
                self.validate_type_parameter_arity(
                    base,
                    definition.type_parameters,
                    definition.span,
                    &format!("{path}.base"),
                    0,
                );
                self.validate_contract(
                    predicate,
                    &ContractEnv {
                        receiver: Some(base.clone()),
                        result: None,
                        arguments: Vec::new(),
                        bindings: Vec::new(),
                        allow_old: false,
                    },
                    &format!("{path}.predicate"),
                );
            }
        }
    }

    fn validate_concept(&mut self, concept: &ConceptDef, path: &str) {
        let mut associated_names = BTreeSet::new();
        for (index, associated) in concept.associated_types.iter().enumerate() {
            if !associated_names.insert(associated.name.as_str()) {
                self.push(
                    MirValidationCode::ConceptShape,
                    format!(
                        "associated type `{}` is declared more than once",
                        associated.name
                    ),
                    associated.span,
                    format!("{path}.associated_types[{index}]"),
                );
            }
        }

        let mut listed = BTreeSet::new();
        for (index, requirement_id) in concept.requirements.iter().copied().enumerate() {
            if !listed.insert(requirement_id) {
                self.push(
                    MirValidationCode::ConceptShape,
                    format!("requirement #{} is listed more than once", requirement_id.0),
                    concept.span,
                    format!("{path}.requirements[{index}]"),
                );
            }
            let Some(requirement) = self.program.requirement(requirement_id) else {
                self.invalid_requirement(
                    requirement_id,
                    concept.span,
                    format!("{path}.requirements[{index}]"),
                );
                continue;
            };
            if requirement.concept != concept.id {
                self.push(
                    MirValidationCode::RequirementShape,
                    format!(
                        "requirement #{} belongs to concept #{}, not #{}",
                        requirement_id.0, requirement.concept.0, concept.id.0
                    ),
                    requirement.span,
                    format!("{path}.requirements[{index}]"),
                );
            }
        }

        for requirement in &self.program.requirements {
            if requirement.concept == concept.id && !listed.contains(&requirement.id) {
                self.push(
                    MirValidationCode::ConceptShape,
                    format!(
                        "owned requirement #{} is absent from the concept method table",
                        requirement.id.0
                    ),
                    requirement.span,
                    format!("{path}.requirements"),
                );
            }
        }
    }

    fn validate_requirement(&mut self, requirement: &RequirementDef, path: &str) {
        let Some(concept) = self.program.concept(requirement.concept).cloned() else {
            self.invalid_concept(
                requirement.concept,
                requirement.span,
                format!("{path}.concept"),
            );
            return;
        };

        if !concept.requirements.contains(&requirement.id) {
            self.push(
                MirValidationCode::RequirementShape,
                "requirement owner does not list this requirement id",
                requirement.span,
                format!("{path}.concept"),
            );
        }
        if requirement.receiver.is_some()
            && !matches!(requirement.params.first(), Some(RequirementType::SelfType))
        {
            self.push(
                MirValidationCode::ReceiverShape,
                "receiver requirement parameter zero must be SelfType",
                requirement.span,
                format!("{path}.params"),
            );
        }

        for (index, parameter) in requirement.params.iter().enumerate() {
            self.validate_requirement_type(
                parameter,
                &concept,
                requirement.method_type_parameters,
                &requirement.witness_params,
                requirement.span,
                &format!("{path}.params[{index}]"),
                0,
            );
        }
        self.validate_requirement_type(
            &requirement.return_ty,
            &concept,
            requirement.method_type_parameters,
            &requirement.witness_params,
            requirement.span,
            &format!("{path}.return_ty"),
            0,
        );
        if self.nesting_failed {
            return;
        }
        for (index, parameter) in requirement.witness_params.iter().enumerate() {
            self.validate_requirement_witness_param(
                parameter,
                &concept,
                requirement.method_type_parameters,
                &requirement.witness_params[..index],
                &format!("{path}.witness_params[{index}]"),
            );
        }

        if concept.dynamic {
            if requirement.receiver.is_none() {
                self.push(
                    MirValidationCode::RequirementShape,
                    "a dyn concept cannot contain a static requirement",
                    requirement.span,
                    path,
                );
            }
            if requirement.method_type_parameters != 0 || !requirement.witness_params.is_empty() {
                self.push(
                    MirValidationCode::RequirementShape,
                    "a dyn requirement cannot have method-specific type or witness parameters",
                    requirement.span,
                    path,
                );
            }
            for (index, parameter) in requirement.params.iter().enumerate().skip(1) {
                if requirement_type_contains_self(parameter) {
                    self.push(
                        MirValidationCode::RequirementShape,
                        "SelfType may only appear in the receiver of a dyn requirement",
                        requirement.span,
                        format!("{path}.params[{index}]"),
                    );
                }
            }
            if requirement_type_contains_self(&requirement.return_ty) {
                self.push(
                    MirValidationCode::RequirementShape,
                    "SelfType may not appear in a dyn requirement return type",
                    requirement.span,
                    format!("{path}.return_ty"),
                );
            }
        }
    }

    fn validate_requirement_witness_param(
        &mut self,
        parameter: &RequirementWitnessParam,
        owner: &ConceptDef,
        method_type_parameters: u32,
        available_witness_params: &[RequirementWitnessParam],
        path: &str,
    ) {
        self.validate_requirement_type(
            &parameter.target,
            owner,
            method_type_parameters,
            available_witness_params,
            parameter.span,
            &format!("{path}.target"),
            0,
        );
        self.validate_concept_binding_schema(
            parameter.concept,
            parameter.bindings.iter(),
            false,
            parameter.span,
            path,
            |validator, ty, binding_path| {
                validator.validate_requirement_type(
                    ty,
                    owner,
                    method_type_parameters,
                    available_witness_params,
                    parameter.span,
                    binding_path,
                    0,
                );
            },
        );
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn validate_requirement_type(
        &mut self,
        ty: &RequirementType,
        owner: &ConceptDef,
        method_type_parameters: u32,
        available_witness_params: &[RequirementWitnessParam],
        span: Span,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            RequirementType::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_requirement_type(
                        element,
                        owner,
                        method_type_parameters,
                        available_witness_params,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            RequirementType::Nominal(id, arguments) => {
                if let Some(definition) = self.program.type_def(*id) {
                    if arguments.len() != definition.type_parameters as usize {
                        self.push(
                            MirValidationCode::RequirementShape,
                            format!(
                                "type `{}` expects {} argument(s), found {}",
                                definition.name,
                                definition.type_parameters,
                                arguments.len()
                            ),
                            span,
                            path,
                        );
                    }
                } else {
                    self.invalid_type(*id, span, path);
                }
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_requirement_type(
                        argument,
                        owner,
                        method_type_parameters,
                        available_witness_params,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            RequirementType::Associated(name) => {
                if !owner
                    .associated_types
                    .iter()
                    .any(|associated| associated.name == *name)
                {
                    self.push(
                        MirValidationCode::RequirementShape,
                        format!("unknown associated type `{name}` in concept signature"),
                        span,
                        path,
                    );
                }
            }
            RequirementType::MethodParameter(index) => {
                if *index >= method_type_parameters {
                    self.push(
                        MirValidationCode::RequirementShape,
                        format!(
                            "method type parameter #{index} is outside declared arity {method_type_parameters}"
                        ),
                        span,
                        path,
                    );
                }
            }
            RequirementType::AssociatedProjection {
                witness,
                associated,
            } => {
                let Some(parameter) = available_witness_params.get(*witness as usize) else {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        format!(
                            "requirement associated projection references unavailable method witness #{witness}"
                        ),
                        span,
                        path,
                    );
                    return;
                };
                let Some(concept) = self.program.concept(parameter.concept) else {
                    self.invalid_concept(parameter.concept, span, path);
                    return;
                };
                if !concept
                    .associated_types
                    .iter()
                    .any(|candidate| candidate.name == *associated)
                {
                    self.push(
                        MirValidationCode::RequirementShape,
                        format!(
                            "concept `{}` has no associated type `{associated}`",
                            concept.name
                        ),
                        span,
                        path,
                    );
                }
            }
            RequirementType::View {
                concept, bindings, ..
            } => {
                if self
                    .program
                    .concept(*concept)
                    .is_some_and(|concept| !concept.dynamic)
                {
                    self.push(
                        MirValidationCode::ConceptShape,
                        "requirement interface type must reference a dyn concept",
                        span,
                        format!("{path}.concept"),
                    );
                }
                self.validate_concept_binding_schema(
                    *concept,
                    bindings.iter(),
                    true,
                    span,
                    path,
                    |validator, ty, binding_path| {
                        validator.validate_requirement_type(
                            ty,
                            owner,
                            method_type_parameters,
                            available_witness_params,
                            span,
                            binding_path,
                            depth + 1,
                        );
                    },
                );
            }
            RequirementType::Unit
            | RequirementType::Bool
            | RequirementType::Int
            | RequirementType::Float
            | RequirementType::Text
            | RequirementType::SelfType => {}
        }
    }

    fn validate_concept_binding_schema<'binding, T: 'binding>(
        &mut self,
        concept_id: ConceptId,
        bindings: impl Iterator<Item = (&'binding String, &'binding T)>,
        require_complete: bool,
        span: Span,
        path: &str,
        mut validate_binding: impl FnMut(&mut Self, &'binding T, &str),
    ) {
        let Some(concept) = self.program.concept(concept_id).cloned() else {
            self.invalid_concept(concept_id, span, format!("{path}.concept"));
            return;
        };
        let bindings: Vec<_> = bindings.collect();
        let expected: BTreeSet<_> = concept
            .associated_types
            .iter()
            .map(|associated| associated.name.as_str())
            .collect();
        let actual: BTreeSet<_> = bindings.iter().map(|(name, _)| name.as_str()).collect();
        for (name, ty) in bindings {
            if !expected.contains(name.as_str()) {
                self.push(
                    MirValidationCode::WitnessShape,
                    format!("unknown associated binding `{name}` for `{}`", concept.name),
                    span,
                    format!("{path}.bindings[{name:?}]"),
                );
            }
            validate_binding(self, ty, &format!("{path}.bindings[{name:?}]"));
        }
        if require_complete && actual != expected {
            self.push(
                MirValidationCode::WitnessShape,
                format!(
                    "`{}` requires exactly the associated bindings {:?}, found {:?}",
                    concept.name, expected, actual
                ),
                span,
                format!("{path}.bindings"),
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_witness(&mut self, witness: &Witness, path: &str) {
        let Some(concept) = self.program.concept(witness.concept).cloned() else {
            self.invalid_concept(witness.concept, Span::default(), format!("{path}.concept"));
            return;
        };
        if matches!(witness.concrete, Type::Parameter(_)) {
            self.push(
                MirValidationCode::WitnessShape,
                "a conformance head cannot be a bare blanket type parameter",
                Span::default(),
                format!("{path}.concrete"),
            );
        }
        self.validate_type(
            &witness.concrete,
            Span::default(),
            &format!("{path}.concrete"),
            0,
        );
        if self.nesting_failed {
            return;
        }
        self.reject_frame_projection(
            &witness.concrete,
            Span::default(),
            &format!("{path}.concrete"),
            0,
        );
        self.validate_view_placement(
            &witness.concrete,
            false,
            Span::default(),
            &format!("{path}.concrete"),
            0,
        );
        self.validate_type_parameter_arity(
            &witness.concrete,
            witness.type_parameters,
            Span::default(),
            &format!("{path}.concrete"),
            0,
        );
        let mut head_parameters = BTreeSet::new();
        collect_type_parameters(&witness.concrete, &mut head_parameters);
        let declared_parameters: BTreeSet<_> = (0..witness.type_parameters).collect();
        if head_parameters != declared_parameters {
            self.push(
                MirValidationCode::WitnessShape,
                format!(
                    "witness generic parameters must be constrained exactly by its concrete head; expected {declared_parameters:?}, found {head_parameters:?}"
                ),
                Span::default(),
                format!("{path}.concrete"),
            );
        }
        for (name, ty) in &witness.associated {
            self.validate_type(
                ty,
                Span::default(),
                &format!("{path}.associated[{name:?}]"),
                0,
            );
            self.reject_frame_projection(
                ty,
                Span::default(),
                &format!("{path}.associated[{name:?}]"),
                0,
            );
            self.validate_view_placement(
                ty,
                false,
                Span::default(),
                &format!("{path}.associated[{name:?}]"),
                0,
            );
            self.validate_type_parameter_arity(
                ty,
                witness.type_parameters,
                Span::default(),
                &format!("{path}.associated[{name:?}]"),
                0,
            );
            if self.nesting_failed {
                return;
            }
        }
        self.validate_concept_binding_schema(
            witness.concept,
            witness.associated.iter(),
            true,
            Span::default(),
            path,
            |validator, ty, binding_path| {
                validator.validate_type(ty, Span::default(), binding_path, 0);
            },
        );
        for (index, prerequisite) in witness.prerequisites.iter().enumerate() {
            self.validate_witness_param(
                prerequisite,
                witness.type_parameters,
                &format!("{path}.prerequisites[{index}]"),
                false,
            );
        }

        let expected_methods: BTreeSet<_> = concept.requirements.iter().copied().collect();
        let actual_methods: BTreeSet<_> = witness.methods.keys().copied().collect();
        if actual_methods != expected_methods {
            self.push(
                MirValidationCode::WitnessShape,
                format!(
                    "witness method table must contain exactly {expected_methods:?}, found {actual_methods:?}"
                ),
                Span::default(),
                format!("{path}.methods"),
            );
        }
        for (requirement, function_id) in &witness.methods {
            let method_path = format!("{path}.methods[{}]", requirement.0);
            let Some(requirement_def) = self.program.requirement(*requirement).cloned() else {
                self.invalid_requirement(*requirement, Span::default(), &method_path);
                continue;
            };
            if requirement_def.concept != witness.concept {
                self.push(
                    MirValidationCode::WitnessShape,
                    format!(
                        "requirement #{} belongs to concept #{}, not witness concept #{}",
                        requirement.0, requirement_def.concept.0, witness.concept.0
                    ),
                    requirement_def.span,
                    &method_path,
                );
                continue;
            }
            let Some(function) = self.program.function(*function_id) else {
                self.push(
                    MirValidationCode::InvalidFunctionReference,
                    format!(
                        "witness method references unknown function #{}",
                        function_id.0
                    ),
                    Span::default(),
                    method_path,
                );
                continue;
            };

            if function.is_async {
                self.push(
                    MirValidationCode::WitnessShape,
                    "async concept requirements are not part of the current MIR contract",
                    function.span,
                    format!("{method_path}.is_async"),
                );
            }

            let expected_type_parameters = witness
                .type_parameters
                .saturating_add(requirement_def.method_type_parameters);
            if function.type_parameters != expected_type_parameters {
                self.push(
                    MirValidationCode::WitnessShape,
                    format!(
                        "witness method declares {} type parameter(s), but conformance + method schemas require {expected_type_parameters}",
                        function.type_parameters
                    ),
                    function.span,
                    format!("{method_path}.type_parameters"),
                );
            }

            let expected_params = requirement_def
                .params
                .iter()
                .enumerate()
                .map(|(index, ty)| {
                    self.instantiate_requirement_type(
                        ty,
                        witness,
                        requirement_def.span,
                        &format!("{method_path}.signature.params[{index}]"),
                        0,
                    )
                })
                .collect::<Option<Vec<_>>>();
            let expected_return = self.instantiate_requirement_type(
                &requirement_def.return_ty,
                witness,
                requirement_def.span,
                &format!("{method_path}.signature.return_ty"),
                0,
            );
            if function.receiver != requirement_def.receiver {
                self.push(
                    MirValidationCode::ReceiverShape,
                    "witness method receiver mode differs from its requirement",
                    function.span,
                    method_path.clone(),
                );
            }
            if let Some(expected_params) = expected_params
                && (function.params.len() != expected_params.len()
                    || function
                        .params
                        .iter()
                        .zip(&expected_params)
                        .any(|(actual, expected)| actual.ty != *expected))
            {
                self.push(
                    MirValidationCode::WitnessShape,
                    "witness method value signature does not equal the substituted requirement",
                    function.span,
                    format!("{method_path}.params"),
                );
            }
            if expected_return.is_some_and(|expected| function.return_ty != expected) {
                self.push(
                    MirValidationCode::WitnessShape,
                    "witness method return type does not equal the substituted requirement",
                    function.span,
                    format!("{method_path}.return_ty"),
                );
            }

            let mut expected_witness_params = witness.prerequisites.clone();
            for (index, parameter) in requirement_def.witness_params.iter().enumerate() {
                if let Some(parameter) = self.instantiate_requirement_witness_param(
                    parameter,
                    witness,
                    &format!("{method_path}.signature.witness_params[{index}]"),
                ) {
                    expected_witness_params.push(parameter);
                }
            }
            match u32::try_from(witness.prerequisites.len()) {
                Ok(expected_prefix_count)
                    if function.witness_prefix_count != expected_prefix_count =>
                {
                    self.push(
                        MirValidationCode::WitnessArity,
                        format!(
                            "witness method declares {} conformance proof parameter(s), but its witness requires {expected_prefix_count}",
                            function.witness_prefix_count
                        ),
                        function.span,
                        format!("{method_path}.witness_prefix_count"),
                    );
                }
                Err(_) => self.push(
                    MirValidationCode::WitnessArity,
                    "witness prerequisite count exceeds the representable function proof prefix",
                    function.span,
                    format!("{method_path}.witness_prefix_count"),
                ),
                Ok(_) => {}
            }
            let actual_suffix_count = usize::try_from(function.witness_prefix_count)
                .ok()
                .and_then(|prefix| function.witness_params.len().checked_sub(prefix));
            if actual_suffix_count != Some(requirement_def.witness_params.len()) {
                let actual_suffix = actual_suffix_count.map_or_else(
                    || "no valid segment because its prefix exceeds the total".to_owned(),
                    |count| format!("{count} parameter(s)"),
                );
                self.push(
                    MirValidationCode::WitnessArity,
                    format!(
                        "witness method proof suffix has {actual_suffix}, but its requirement requires {} parameter(s)",
                        requirement_def.witness_params.len()
                    ),
                    function.span,
                    format!("{method_path}.witness_params"),
                );
            }
            if function.witness_params.len() != expected_witness_params.len() {
                self.push(
                    MirValidationCode::WitnessArity,
                    format!(
                        "witness method expects {} proof parameter(s), but conditional and method schemas require {}",
                        function.witness_params.len(),
                        expected_witness_params.len()
                    ),
                    function.span,
                    format!("{method_path}.witness_params"),
                );
            }
            for (index, (actual, expected)) in function
                .witness_params
                .iter()
                .zip(&expected_witness_params)
                .enumerate()
            {
                if !witness_params_equal(actual, expected) {
                    self.push(
                        MirValidationCode::WitnessShape,
                        "witness method proof schema differs from the substituted requirement",
                        actual.span,
                        format!("{method_path}.witness_params[{index}]"),
                    );
                }
            }
        }
    }

    fn validate_witness_coherence(&mut self) {
        for (left_index, left) in self.program.witnesses.iter().enumerate() {
            for (right_index, right) in self
                .program
                .witnesses
                .iter()
                .enumerate()
                .skip(left_index + 1)
            {
                if left.concept == right.concept
                    && type_schemas_overlap(&left.concrete, &right.concrete)
                {
                    self.push(
                        MirValidationCode::WitnessShape,
                        format!("conformance heads overlap with witness #{}", right.id.0),
                        Span::default(),
                        format!("witnesses[{left_index}].concrete"),
                    );
                    self.push(
                        MirValidationCode::WitnessShape,
                        format!("conformance heads overlap with witness #{}", left.id.0),
                        Span::default(),
                        format!("witnesses[{right_index}].concrete"),
                    );
                }
            }
            for (index, prerequisite) in left.prerequisites.iter().enumerate() {
                if !is_strict_type_subterm(&prerequisite.target, &left.concrete) {
                    self.push(
                        MirValidationCode::WitnessShape,
                        "conditional conformance prerequisites must structurally decrease from the conformance head",
                        prerequisite.span,
                        format!("witnesses[{left_index}].prerequisites[{index}].target"),
                    );
                }
            }
        }
    }

    fn validate_witness_param(
        &mut self,
        parameter: &WitnessParam,
        arity: u32,
        path: &str,
        allow_frame_projection: bool,
    ) {
        self.validate_type(
            &parameter.target,
            parameter.span,
            &format!("{path}.target"),
            0,
        );
        if !allow_frame_projection {
            self.reject_frame_projection(
                &parameter.target,
                parameter.span,
                &format!("{path}.target"),
                0,
            );
        }
        self.validate_view_placement(
            &parameter.target,
            false,
            parameter.span,
            &format!("{path}.target"),
            0,
        );
        self.validate_type_parameter_arity(
            &parameter.target,
            arity,
            parameter.span,
            &format!("{path}.target"),
            0,
        );
        self.validate_concept_binding_schema(
            parameter.concept,
            parameter.bindings.iter(),
            false,
            parameter.span,
            path,
            |validator, ty, binding_path| {
                validator.validate_type(ty, parameter.span, binding_path, 0);
                if !allow_frame_projection {
                    validator.reject_frame_projection(ty, parameter.span, binding_path, 0);
                }
                validator.validate_view_placement(ty, false, parameter.span, binding_path, 0);
                validator.validate_type_parameter_arity(ty, arity, parameter.span, binding_path, 0);
            },
        );
    }

    fn instantiate_requirement_witness_param(
        &mut self,
        parameter: &RequirementWitnessParam,
        witness: &Witness,
        path: &str,
    ) -> Option<WitnessParam> {
        self.instantiate_requirement_witness_param_with(
            parameter,
            &witness.concrete,
            &witness.associated,
            None,
            MethodTypes::ParametersFrom(witness.type_parameters),
            RequirementProofs::FunctionParametersFrom(
                u32::try_from(witness.prerequisites.len()).unwrap_or(u32::MAX),
            ),
            path,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate_requirement_witness_param_with(
        &mut self,
        parameter: &RequirementWitnessParam,
        self_ty: &Type,
        associated: &BTreeMap<String, Type>,
        projection_witness: Option<u32>,
        method_types: MethodTypes<'_>,
        requirement_proofs: RequirementProofs<'_>,
        path: &str,
    ) -> Option<WitnessParam> {
        let target = self.instantiate_requirement_type_with(
            &parameter.target,
            self_ty,
            associated,
            projection_witness,
            method_types,
            requirement_proofs,
            parameter.span,
            &format!("{path}.target"),
            0,
        )?;
        let mut bindings = BTreeMap::new();
        for (name, ty) in &parameter.bindings {
            let ty = self.instantiate_requirement_type_with(
                ty,
                self_ty,
                associated,
                projection_witness,
                method_types,
                requirement_proofs,
                parameter.span,
                &format!("{path}.bindings[{name:?}]"),
                0,
            )?;
            bindings.insert(name.clone(), ty);
        }
        Some(WitnessParam {
            target,
            concept: parameter.concept,
            bindings,
            span: parameter.span,
        })
    }

    fn instantiate_requirement_type(
        &mut self,
        ty: &RequirementType,
        witness: &Witness,
        span: Span,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        self.instantiate_requirement_type_with(
            ty,
            &witness.concrete,
            &witness.associated,
            None,
            MethodTypes::ParametersFrom(witness.type_parameters),
            RequirementProofs::FunctionParametersFrom(
                u32::try_from(witness.prerequisites.len()).unwrap_or(u32::MAX),
            ),
            span,
            path,
            depth,
        )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn instantiate_requirement_type_with(
        &mut self,
        ty: &RequirementType,
        self_ty: &Type,
        associated: &BTreeMap<String, Type>,
        projection_witness: Option<u32>,
        method_types: MethodTypes<'_>,
        requirement_proofs: RequirementProofs<'_>,
        span: Span,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        if !self.enter(depth, span, path) {
            return None;
        }
        Some(match ty {
            RequirementType::Unit => Type::Unit,
            RequirementType::Bool => Type::Bool,
            RequirementType::Int => Type::Int,
            RequirementType::Float => Type::Float,
            RequirementType::Text => Type::Text,
            RequirementType::Tuple(elements) => Type::Tuple(
                elements
                    .iter()
                    .enumerate()
                    .map(|(index, element)| {
                        self.instantiate_requirement_type_with(
                            element,
                            self_ty,
                            associated,
                            projection_witness,
                            method_types,
                            requirement_proofs,
                            span,
                            &format!("{path}.elements[{index}]"),
                            depth + 1,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?,
            ),
            RequirementType::SelfType => self_ty.clone(),
            RequirementType::Associated(name) => {
                if let Some(binding) = associated.get(name) {
                    binding.clone()
                } else if let Some(witness) = projection_witness {
                    Type::AssociatedProjection {
                        witness,
                        associated: name.clone(),
                    }
                } else {
                    self.push(
                        MirValidationCode::WitnessShape,
                        format!("witness does not bind associated type `{name}`"),
                        span,
                        path,
                    );
                    return None;
                }
            }
            RequirementType::MethodParameter(index) => match method_types {
                MethodTypes::ParametersFrom(offset) => Type::Parameter(offset + *index),
                MethodTypes::Arguments(arguments) => {
                    let Some(argument) = arguments.get(*index as usize) else {
                        self.push(
                            MirValidationCode::RequirementShape,
                            format!("missing method type argument #{index}"),
                            span,
                            path,
                        );
                        return None;
                    };
                    argument.clone()
                }
            },
            RequirementType::AssociatedProjection {
                witness,
                associated,
            } => match requirement_proofs {
                RequirementProofs::FunctionParametersFrom(offset) => Type::AssociatedProjection {
                    witness: offset + *witness,
                    associated: associated.clone(),
                },
                RequirementProofs::Resolved(proofs) => {
                    let Some(Some(resolved)) = proofs.get(*witness as usize) else {
                        self.push(
                            MirValidationCode::InvalidWitnessReference,
                            format!(
                                "requirement cannot resolve associated projection through method witness #{witness}"
                            ),
                            span,
                            path,
                        );
                        return None;
                    };
                    if let Some(binding) = resolved.proof.bindings.get(associated) {
                        binding.clone()
                    } else if let Some(witness) = resolved.projection_witness {
                        Type::AssociatedProjection {
                            witness,
                            associated: associated.clone(),
                        }
                    } else {
                        self.push(
                            MirValidationCode::WitnessShape,
                            format!("method witness does not bind associated type `{associated}`"),
                            span,
                            path,
                        );
                        return None;
                    }
                }
                RequirementProofs::Unavailable => {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        "associated projection requires a method-specific witness",
                        span,
                        path,
                    );
                    return None;
                }
            },
            RequirementType::Nominal(id, arguments) => Type::Nominal(
                *id,
                arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        self.instantiate_requirement_type_with(
                            argument,
                            self_ty,
                            associated,
                            projection_witness,
                            method_types,
                            requirement_proofs,
                            span,
                            &format!("{path}.arguments[{index}]"),
                            depth + 1,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?,
            ),
            RequirementType::View {
                mutable,
                concept,
                bindings,
            } => {
                let mut instantiated = BTreeMap::new();
                for (name, binding) in bindings {
                    instantiated.insert(
                        name.clone(),
                        self.instantiate_requirement_type_with(
                            binding,
                            self_ty,
                            associated,
                            projection_witness,
                            method_types,
                            requirement_proofs,
                            span,
                            &format!("{path}.bindings[{name:?}]"),
                            depth + 1,
                        )?,
                    );
                }
                Type::View {
                    mutable: *mutable,
                    concept: *concept,
                    bindings: instantiated,
                }
            }
        })
    }

    fn validate_type_parameter_arity(
        &mut self,
        ty: &Type,
        arity: u32,
        span: Span,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            Type::Parameter(index) if *index >= arity => self.push(
                MirValidationCode::WitnessShape,
                format!("type parameter #{index} is outside declared arity {arity}"),
                span,
                path,
            ),
            Type::Nominal(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_type_parameter_arity(
                        argument,
                        arity,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_type_parameter_arity(
                        element,
                        arity,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => self
                .validate_type_parameter_arity(
                    output,
                    arity,
                    span,
                    &format!("{path}.output"),
                    depth + 1,
                ),
            Type::View {
                concept, bindings, ..
            } => {
                let concept_def = self.program.concept(*concept).cloned();
                if concept_def.is_none() {
                    self.invalid_concept(*concept, span, format!("{path}.concept"));
                } else if concept_def.as_ref().is_some_and(|concept| !concept.dynamic) {
                    self.push(
                        MirValidationCode::ConceptShape,
                        "interface type must reference a dyn-declared concept",
                        span,
                        format!("{path}.concept"),
                    );
                }
                for (name, binding) in bindings {
                    self.validate_type_parameter_arity(
                        binding,
                        arity,
                        span,
                        &format!("{path}.bindings[{name:?}]"),
                        depth + 1,
                    );
                }
                if let Some(concept) = concept_def {
                    let expected: BTreeSet<_> = concept
                        .associated_types
                        .iter()
                        .map(|associated| associated.name.as_str())
                        .collect();
                    let actual: BTreeSet<_> = bindings.keys().map(String::as_str).collect();
                    if expected != actual {
                        self.push(
                            MirValidationCode::WitnessShape,
                            format!(
                                "interface bindings must be exactly {expected:?}, found {actual:?}"
                            ),
                            span,
                            format!("{path}.bindings"),
                        );
                    }
                }
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_function(&mut self, function: &Function, path: &str) {
        self.validate_expression_ids(function, path);
        let witness_prefix_count =
            usize::try_from(function.witness_prefix_count).unwrap_or(usize::MAX);
        if witness_prefix_count > function.witness_params.len() {
            self.push(
                MirValidationCode::WitnessArity,
                format!(
                    "witness proof prefix {} exceeds the function's {} proof parameter(s)",
                    function.witness_prefix_count,
                    function.witness_params.len()
                ),
                function.span,
                format!("{path}.witness_prefix_count"),
            );
        }
        let is_witness_method = self.program.witnesses.iter().any(|witness| {
            witness
                .methods
                .values()
                .any(|method| *method == function.id)
        });
        if !is_witness_method && function.witness_prefix_count != 0 {
            self.push(
                MirValidationCode::WitnessShape,
                "only witness methods may declare a conformance proof prefix",
                function.span,
                format!("{path}.witness_prefix_count"),
            );
        }
        for (index, parameter) in function.params.iter().enumerate() {
            self.validate_type(
                &parameter.ty,
                parameter.span,
                &format!("{path}.params[{index}].ty"),
                0,
            );
            if self.nesting_failed {
                return;
            }
            self.validate_function_type(
                function,
                &parameter.ty,
                parameter.span,
                &format!("{path}.params[{index}].ty"),
                0,
            );
            self.validate_view_placement(
                &parameter.ty,
                true,
                parameter.span,
                &format!("{path}.params[{index}].ty"),
                0,
            );
        }
        for (index, local) in function.locals.iter().enumerate() {
            self.validate_type(
                &local.ty,
                local.span,
                &format!("{path}.locals[{index}].ty"),
                0,
            );
            if self.nesting_failed {
                return;
            }
            self.validate_function_type(
                function,
                &local.ty,
                local.span,
                &format!("{path}.locals[{index}].ty"),
                0,
            );
            self.validate_view_placement(
                &local.ty,
                true,
                local.span,
                &format!("{path}.locals[{index}].ty"),
                0,
            );
        }
        self.validate_type(
            &function.return_ty,
            function.span,
            &format!("{path}.return_ty"),
            0,
        );
        if self.nesting_failed {
            return;
        }
        self.validate_function_type(
            function,
            &function.return_ty,
            function.span,
            &format!("{path}.return_ty"),
            0,
        );
        self.validate_view_placement(
            &function.return_ty,
            false,
            function.span,
            &format!("{path}.return_ty"),
            0,
        );
        for (index, witness_parameter) in function.witness_params.iter().enumerate() {
            self.validate_witness_param(
                witness_parameter,
                function.type_parameters,
                &format!("{path}.witness_params[{index}]"),
                true,
            );
            self.validate_function_type(
                function,
                &witness_parameter.target,
                witness_parameter.span,
                &format!("{path}.witness_params[{index}].target"),
                0,
            );
            self.validate_projection_precedes(
                &witness_parameter.target,
                index,
                witness_parameter.span,
                &format!("{path}.witness_params[{index}].target"),
                0,
            );
            for (name, binding) in &witness_parameter.bindings {
                self.validate_function_type(
                    function,
                    binding,
                    witness_parameter.span,
                    &format!("{path}.witness_params[{index}].bindings[{name:?}]"),
                    0,
                );
                self.validate_projection_precedes(
                    binding,
                    index,
                    witness_parameter.span,
                    &format!("{path}.witness_params[{index}].bindings[{name:?}]"),
                    0,
                );
            }
        }
        self.validate_locals(function, path);
        self.validate_receiver(function, path);

        let explicit_parameters = if function.receiver.is_some() {
            function.params.get(1..).unwrap_or_default()
        } else {
            &function.params
        };
        let arguments: Vec<_> = explicit_parameters
            .iter()
            .map(|parameter| parameter.ty.clone())
            .collect();
        let receiver = function.receiver.and_then(|_| {
            function
                .params
                .first()
                .map(|parameter| parameter.ty.clone())
        });
        if let Some(contract) = &function.call_plan.receiver_invariant {
            self.validate_contract(
                contract,
                &ContractEnv {
                    receiver: receiver.clone(),
                    result: None,
                    arguments: arguments.clone(),
                    bindings: Vec::new(),
                    allow_old: false,
                },
                &format!("{path}.call_plan.receiver_invariant"),
            );
        }
        for (index, contract) in function.call_plan.requires.iter().enumerate() {
            self.validate_contract(
                contract,
                &ContractEnv {
                    receiver: receiver.clone(),
                    result: None,
                    arguments: arguments.clone(),
                    bindings: Vec::new(),
                    allow_old: false,
                },
                &format!("{path}.call_plan.requires[{index}]"),
            );
        }
        for (index, contract) in function.call_plan.ensures.iter().enumerate() {
            self.validate_contract(
                contract,
                &ContractEnv {
                    receiver: receiver.clone(),
                    result: Some(function.return_ty.clone()),
                    arguments: arguments.clone(),
                    bindings: Vec::new(),
                    allow_old: true,
                },
                &format!("{path}.call_plan.ensures[{index}]"),
            );
        }
        self.validate_block(
            function,
            &function.body,
            Some(&function.return_ty),
            &format!("{path}.body"),
            0,
        );
        if !self.nesting_failed {
            self.validate_function_dataflow(function, path);
        }
    }

    fn validate_expression_ids(&mut self, function: &Function, path: &str) {
        for (expected, expression) in function.exprs_preorder().enumerate() {
            let Ok(expected) = u32::try_from(expected) else {
                self.push(
                    MirValidationCode::ExpressionIdentity,
                    "function exhausts the usable expression-id domain",
                    expression.span,
                    format!("{path}.body"),
                );
                return;
            };
            if expected == crate::ExprId::UNASSIGNED.0 {
                self.push(
                    MirValidationCode::ExpressionIdentity,
                    "function exhausts the usable expression-id domain",
                    expression.span,
                    format!("{path}.body"),
                );
                return;
            }
            if expression.id.0 != expected {
                self.push(
                    MirValidationCode::ExpressionIdentity,
                    format!(
                        "expression id must be canonical function-local preorder id {expected}, found {}",
                        expression.id.0
                    ),
                    expression.span,
                    format!("{path}.body.expr_ids[{expected}]"),
                );
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_function_type(
        &mut self,
        function: &Function,
        ty: &Type,
        span: Span,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            Type::Parameter(index) if *index >= function.type_parameters => self.push(
                MirValidationCode::TypeMismatch,
                format!(
                    "function type parameter #{index} is outside declared arity {}",
                    function.type_parameters
                ),
                span,
                path,
            ),
            Type::AssociatedProjection {
                witness,
                associated,
            } => {
                let Some(parameter) = function.witness_params.get(*witness as usize) else {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        format!("associated projection uses unknown witness parameter #{witness}"),
                        span,
                        path,
                    );
                    return;
                };
                let Some(concept) = self.program.concept(parameter.concept) else {
                    self.invalid_concept(parameter.concept, span, path);
                    return;
                };
                if !concept
                    .associated_types
                    .iter()
                    .any(|candidate| candidate.name == *associated)
                {
                    self.push(
                        MirValidationCode::TypeMismatch,
                        format!(
                            "concept `{}` has no associated type `{associated}`",
                            concept.name
                        ),
                        span,
                        path,
                    );
                }
            }
            Type::Nominal(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_function_type(
                        function,
                        argument,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_function_type(
                        function,
                        element,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => self
                .validate_function_type(
                    function,
                    output,
                    span,
                    &format!("{path}.output"),
                    depth + 1,
                ),
            Type::View {
                concept, bindings, ..
            } => {
                let concept_def = self.program.concept(*concept).cloned();
                match &concept_def {
                    None => self.invalid_concept(*concept, span, format!("{path}.concept")),
                    Some(concept) if !concept.dynamic => self.push(
                        MirValidationCode::ConceptShape,
                        "interface type must reference a dyn-declared concept",
                        span,
                        format!("{path}.concept"),
                    ),
                    Some(_) => {}
                }
                for (name, binding) in bindings {
                    self.validate_function_type(
                        function,
                        binding,
                        span,
                        &format!("{path}.bindings[{name:?}]"),
                        depth + 1,
                    );
                }
                if let Some(concept) = concept_def {
                    let expected: BTreeSet<_> = concept
                        .associated_types
                        .iter()
                        .map(|associated| associated.name.as_str())
                        .collect();
                    let actual: BTreeSet<_> = bindings.keys().map(String::as_str).collect();
                    if expected != actual {
                        self.push(
                            MirValidationCode::WitnessShape,
                            format!(
                                "interface bindings must be exactly {expected:?}, found {actual:?}"
                            ),
                            span,
                            format!("{path}.bindings"),
                        );
                    }
                }
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::Error => {}
        }
    }

    fn validate_projection_precedes(
        &mut self,
        ty: &Type,
        maximum_exclusive: usize,
        span: Span,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            Type::AssociatedProjection { witness, .. }
                if *witness as usize >= maximum_exclusive =>
            {
                self.push(
                    MirValidationCode::InvalidWitnessReference,
                    "witness-parameter projections must reference an earlier proof slot",
                    span,
                    path,
                );
            }
            Type::Nominal(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_projection_precedes(
                        argument,
                        maximum_exclusive,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_projection_precedes(
                        element,
                        maximum_exclusive,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => self
                .validate_projection_precedes(
                    output,
                    maximum_exclusive,
                    span,
                    &format!("{path}.output"),
                    depth + 1,
                ),
            Type::View { bindings, .. } => {
                for (name, binding) in bindings {
                    self.validate_projection_precedes(
                        binding,
                        maximum_exclusive,
                        span,
                        &format!("{path}.bindings[{name:?}]"),
                        depth + 1,
                    );
                }
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }

    fn validate_locals(&mut self, function: &Function, path: &str) {
        let mut seen = BTreeSet::new();
        for (expected, local) in function.params.iter().chain(&function.locals).enumerate() {
            if !seen.insert(local.id) {
                self.push(
                    MirValidationCode::DuplicateLocal,
                    format!("local #{} is declared more than once", local.id.0),
                    local.span,
                    format!("{path}.locals"),
                );
            }
            if local.id.0 as usize != expected {
                self.push(
                    MirValidationCode::IndexMismatch,
                    format!(
                        "local declaration position {expected} contains id #{}; locals are direct frame indices",
                        local.id.0
                    ),
                    local.span,
                    format!("{path}.locals"),
                );
            }
        }
    }

    fn validate_receiver(&mut self, function: &Function, path: &str) {
        let Some(receiver) = function.receiver else {
            return;
        };
        let Some(parameter) = function.params.first() else {
            self.push(
                MirValidationCode::ReceiverShape,
                "a receiver function must have its receiver as parameter zero",
                function.span,
                format!("{path}.receiver"),
            );
            return;
        };
        if parameter.mutable != (receiver == Receiver::Mutable) {
            self.push(
                MirValidationCode::ReceiverShape,
                "receiver parameter mutability must exactly match its receiver mode",
                parameter.span,
                format!("{path}.receiver"),
            );
        }
    }

    fn validate_block(
        &mut self,
        function: &Function,
        block: &Block,
        expected_tail: Option<&Type>,
        path: &str,
        depth: u16,
    ) -> Type {
        if !self.enter(depth, block.span, path) {
            return Type::Error;
        }
        for (index, statement) in block.statements.iter().enumerate() {
            self.validate_statement(
                function,
                statement,
                &format!("{path}.statements[{index}]"),
                depth + 1,
            );
        }
        let diverges = block_definitely_diverges(block, 0);
        let evaluated_tail = block
            .tail
            .as_deref()
            .map(|tail| self.validate_expr(function, tail, &format!("{path}.tail"), depth + 1));
        let tail_ty = if diverges {
            Type::Never
        } else {
            evaluated_tail.unwrap_or(Type::Unit)
        };
        if expected_tail.is_some_and(|expected| !types_compatible(expected, &tail_ty)) {
            self.type_mismatch(
                expected_tail.expect("checked above"),
                &tail_ty,
                block.span,
                path,
            );
        }
        tail_ty
    }

    #[allow(clippy::too_many_lines)]
    fn validate_statement(
        &mut self,
        function: &Function,
        statement: &Statement,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, statement.span, path) {
            return;
        }
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let value_ty = self.validate_expr(function, value, &format!("{path}.value"), depth);
                let declared = Self::local_decl(function, *local);
                let Some(declared) = declared else {
                    self.invalid_local(*local, statement.span, format!("{path}.local"));
                    return;
                };
                if !function
                    .locals
                    .iter()
                    .any(|candidate| candidate.id == *local)
                {
                    self.push(
                        MirValidationCode::InvalidLocalReference,
                        "a `Let` statement must initialize a declared non-parameter local",
                        statement.span,
                        format!("{path}.local"),
                    );
                } else if !types_compatible(&declared.ty, &value_ty) {
                    self.type_mismatch(&declared.ty, &value_ty, statement.span, path);
                }
            }
            StatementKind::LetTuple { locals, value } => {
                let value_ty = self.validate_expr(function, value, &format!("{path}.value"), depth);
                let Type::Tuple(elements) = value_ty else {
                    self.push(
                        MirValidationCode::TypeMismatch,
                        "LetTuple value must have tuple type",
                        statement.span,
                        format!("{path}.value"),
                    );
                    return;
                };
                if elements.len() != locals.len() {
                    self.push(
                        MirValidationCode::TypeMismatch,
                        format!(
                            "LetTuple initializes {} local(s) from {} tuple element(s)",
                            locals.len(),
                            elements.len()
                        ),
                        statement.span,
                        path,
                    );
                }
                for (index, (local, element)) in locals.iter().zip(&elements).enumerate() {
                    let Some(declared) = Self::local_decl(function, *local) else {
                        self.invalid_local(
                            *local,
                            statement.span,
                            format!("{path}.locals[{index}]"),
                        );
                        continue;
                    };
                    if !function
                        .locals
                        .iter()
                        .any(|candidate| candidate.id == *local)
                    {
                        self.push(
                            MirValidationCode::InvalidLocalReference,
                            "LetTuple must initialize declared non-parameter locals",
                            statement.span,
                            format!("{path}.locals[{index}]"),
                        );
                    } else if !types_compatible(&declared.ty, element) {
                        self.type_mismatch(
                            &declared.ty,
                            element,
                            statement.span,
                            &format!("{path}.locals[{index}]"),
                        );
                    }
                }
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let Some(declared) = Self::local_decl(function, *local) else {
                    self.invalid_local(*local, statement.span, format!("{path}.local"));
                    return;
                };
                if !function
                    .locals
                    .iter()
                    .any(|candidate| candidate.id == *local)
                {
                    self.push(
                        MirValidationCode::InvalidLocalReference,
                        "ForRange binding must be a declared non-parameter local",
                        statement.span,
                        format!("{path}.local"),
                    );
                }
                if !types_compatible(&Type::Int, &declared.ty) {
                    self.type_mismatch(&Type::Int, &declared.ty, statement.span, path);
                }
                if declared.mutable {
                    self.push(
                        MirValidationCode::ImmutablePlace,
                        "ForRange induction binding must be immutable",
                        statement.span,
                        format!("{path}.local"),
                    );
                }
                let start_ty = self.validate_expr(function, start, &format!("{path}.start"), depth);
                let end_ty = self.validate_expr(function, end, &format!("{path}.end"), depth);
                if !types_compatible(&Type::Int, &start_ty) {
                    self.type_mismatch(&Type::Int, &start_ty, start.span, path);
                }
                if !types_compatible(&Type::Int, &end_ty) {
                    self.type_mismatch(&Type::Int, &end_ty, end.span, path);
                }
                self.validate_block(
                    function,
                    body,
                    Some(&Type::Unit),
                    &format!("{path}.body"),
                    depth + 1,
                );
            }
            StatementKind::Assign { place, value } => {
                let place_ty = self.validate_place(
                    function,
                    place,
                    true,
                    statement.span,
                    &format!("{path}.place"),
                );
                let value_ty = self.validate_expr(function, value, &format!("{path}.value"), depth);
                if place_ty
                    .as_ref()
                    .is_some_and(|ty| !types_compatible(ty, &value_ty))
                {
                    self.type_mismatch(
                        &place_ty.expect("checked above"),
                        &value_ty,
                        statement.span,
                        path,
                    );
                }
            }
            StatementKind::Assert { condition } => {
                let ty =
                    self.validate_expr(function, condition, &format!("{path}.condition"), depth);
                if !types_compatible(&Type::Bool, &ty) {
                    self.type_mismatch(&Type::Bool, &ty, condition.span, path);
                }
            }
            StatementKind::Evaluate(expression) => {
                let expression_path = format!("{path}.expression");
                let ty = self.validate_expr(function, expression, &expression_path, depth);
                if !expr_definitely_diverges(expression, 0) {
                    self.reject_obligation_loss(&ty, expression.span, &expression_path, "discard");
                }
            }
            StatementKind::Defer(cleanup) => {
                self.validate_block(
                    function,
                    cleanup,
                    Some(&Type::Unit),
                    &format!("{path}.cleanup"),
                    depth + 1,
                );
                if cleanup_contains_forbidden_control(cleanup, 0) {
                    self.push(
                        MirValidationCode::ExpressionShape,
                        "defer cleanup cannot return, await, or register another cleanup",
                        statement.span,
                        format!("{path}.cleanup"),
                    );
                }
            }
            StatementKind::Return(value) => {
                let actual = value.as_ref().map_or(Type::Unit, |expression| {
                    self.validate_expr(function, expression, &format!("{path}.value"), depth)
                });
                if !types_compatible(&function.return_ty, &actual) {
                    self.type_mismatch(&function.return_ty, &actual, statement.span, path);
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_expr(
        &mut self,
        function: &Function,
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Type {
        self.validate_type(
            &expression.ty,
            expression.span,
            &format!("{path}.ty"),
            depth,
        );
        if self.nesting_failed {
            return Type::Error;
        }
        self.validate_function_type(
            function,
            &expression.ty,
            expression.span,
            &format!("{path}.ty"),
            depth,
        );
        self.validate_view_placement(
            &expression.ty,
            true,
            expression.span,
            &format!("{path}.ty"),
            depth,
        );
        if !self.enter(depth, expression.span, path) {
            return Type::Error;
        }
        let inferred = match &expression.kind {
            ExprKind::Constant(constant) => Some(constant_type(constant)),
            ExprKind::Tuple(elements) => Some(Type::Tuple(
                elements
                    .iter()
                    .enumerate()
                    .map(|(index, element)| {
                        self.validate_expr(
                            function,
                            element,
                            &format!("{path}.elements[{index}]"),
                            depth + 1,
                        )
                    })
                    .collect(),
            )),
            ExprKind::List(elements) => {
                let declared = match &expression.ty {
                    Type::List(element) => Some(element.as_ref().clone()),
                    _ => None,
                };
                let mut inferred_element = declared;
                for (index, element) in elements.iter().enumerate() {
                    let actual = self.validate_expr(
                        function,
                        element,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                    if let Some(expected) = &inferred_element {
                        if !types_compatible(expected, &actual) {
                            self.type_mismatch(
                                expected,
                                &actual,
                                element.span,
                                &format!("{path}.elements[{index}]"),
                            );
                        }
                    } else {
                        inferred_element = Some(actual);
                    }
                }
                inferred_element.map(|element| Type::List(Box::new(element)))
            }
            ExprKind::Copy(place) => self.validate_place(
                function,
                place,
                false,
                expression.span,
                &format!("{path}.place"),
            ),
            ExprKind::Move(place) => {
                if !place.projection.is_empty() {
                    self.push(
                        MirValidationCode::ProjectedMove,
                        "the interpreter only supports moving an entire local place",
                        expression.span,
                        format!("{path}.place"),
                    );
                }
                self.validate_place(
                    function,
                    place,
                    false,
                    expression.span,
                    &format!("{path}.place"),
                )
            }
            ExprKind::Unary(operator, operand) => {
                let operand_ty =
                    self.validate_expr(function, operand, &format!("{path}.operand"), depth + 1);
                self.validate_unary(*operator, &operand_ty, expression.span, path)
            }
            ExprKind::Binary(operator, left, right) => {
                let left_ty =
                    self.validate_expr(function, left, &format!("{path}.left"), depth + 1);
                let right_ty =
                    self.validate_expr(function, right, &format!("{path}.right"), depth + 1);
                self.validate_binary(*operator, &left_ty, &right_ty, expression.span, path)
            }
            ExprKind::Block(block) => Some(self.validate_block(
                function,
                block,
                Some(&expression.ty),
                &format!("{path}.block"),
                depth + 1,
            )),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_ty = self.validate_expr(
                    function,
                    condition,
                    &format!("{path}.condition"),
                    depth + 1,
                );
                if !types_compatible(&Type::Bool, &condition_ty) {
                    self.type_mismatch(
                        &Type::Bool,
                        &condition_ty,
                        condition.span,
                        &format!("{path}.condition"),
                    );
                }
                let then_ty = self.validate_block(
                    function,
                    then_branch,
                    Some(&expression.ty),
                    &format!("{path}.then"),
                    depth + 1,
                );
                let else_ty = self.validate_block(
                    function,
                    else_branch,
                    Some(&expression.ty),
                    &format!("{path}.else"),
                    depth + 1,
                );
                if !flow_types_compatible(&then_ty, &else_ty) {
                    self.type_mismatch(&then_ty, &else_ty, expression.span, path);
                }
                Some(if then_ty == Type::Never {
                    else_ty
                } else {
                    then_ty
                })
            }
            ExprKind::Match { scrutinee, arms } => {
                let scrutinee_ty = self.validate_expr(
                    function,
                    scrutinee,
                    &format!("{path}.scrutinee"),
                    depth + 1,
                );
                Some(self.validate_match(
                    function,
                    &scrutinee_ty,
                    arms,
                    &expression.ty,
                    path,
                    depth + 1,
                ))
            }
            ExprKind::Record {
                ty,
                type_arguments,
                fields,
                construction,
            } => self.validate_record_expr(
                function,
                *ty,
                type_arguments,
                fields,
                *construction,
                expression,
                path,
                depth + 1,
            ),
            ExprKind::Variant {
                ty,
                type_arguments,
                variant,
                payload,
            } => self.validate_variant_expr(
                function,
                *ty,
                type_arguments,
                *variant,
                payload,
                expression,
                path,
                depth + 1,
            ),
            ExprKind::Refine {
                ty,
                value,
                construction,
            } => self.validate_refine_expr(
                function,
                *ty,
                value,
                *construction,
                expression,
                path,
                depth + 1,
            ),
            ExprKind::Unrefine(value) => {
                let value_ty =
                    self.validate_expr(function, value, &format!("{path}.value"), depth + 1);
                let Type::Nominal(type_id, arguments) = value_ty else {
                    self.push(
                        MirValidationCode::ExpressionShape,
                        "Unrefine operand must have a refined nominal type",
                        value.span,
                        path,
                    );
                    return expression.ty.clone();
                };
                let Some(TypeDef {
                    kind: TypeDefKind::Refined { base, .. },
                    ..
                }) = self.program.type_def(type_id)
                else {
                    self.push(
                        MirValidationCode::ExpressionShape,
                        "Unrefine operand must reference a refined type definition",
                        value.span,
                        path,
                    );
                    return expression.ty.clone();
                };
                Some(substitute_type(base, &arguments))
            }
            ExprKind::Call {
                target,
                type_arguments,
                arguments,
                witnesses,
            } => self.validate_call(
                function,
                target,
                type_arguments,
                arguments,
                witnesses,
                expression,
                path,
                depth + 1,
            ),
            ExprKind::MakeView {
                value,
                writeback,
                witness,
                mutable,
                ..
            } => self.validate_make_view(
                function,
                value,
                writeback.as_ref(),
                witness,
                *mutable,
                expression,
                path,
                depth + 1,
            ),
            ExprKind::ReborrowView { owner, mutable, .. } => {
                self.validate_reborrow_view(function, owner, *mutable, expression, path)
            }
            ExprKind::Await { state, task } => {
                let task_ty =
                    self.validate_expr(function, task, &format!("{path}.task"), depth + 1);
                if !function.is_async {
                    self.push(
                        MirValidationCode::ExpressionShape,
                        "Await is only valid in an async MIR function",
                        expression.span,
                        path,
                    );
                }
                if !function
                    .suspension_points
                    .iter()
                    .any(|point| point.state == *state && point.span == expression.span)
                {
                    self.push(
                        MirValidationCode::ExpressionShape,
                        format!("Await state #{state} has no matching suspension metadata"),
                        expression.span,
                        format!("{path}.state"),
                    );
                }
                match task_ty {
                    Type::Task(output) => Some(*output),
                    Type::Tuple(tasks) => {
                        let mut outputs = Vec::with_capacity(tasks.len());
                        let mut valid = true;
                        for (index, task) in tasks.into_iter().enumerate() {
                            if let Type::Task(output) = task {
                                outputs.push(*output);
                            } else {
                                valid = false;
                                self.push(
                                    MirValidationCode::ExpressionShape,
                                    "every awaited tuple element must be Task",
                                    expression.span,
                                    format!("{path}.task.elements[{index}]"),
                                );
                            }
                        }
                        valid.then_some(Type::Tuple(outputs))
                    }
                    actual => {
                        self.push(
                            MirValidationCode::ExpressionShape,
                            format!("Await operand must be Task, found {actual:?}"),
                            task.span,
                            format!("{path}.task"),
                        );
                        None
                    }
                }
            }
            ExprKind::Sleep { milliseconds } => {
                let actual = self.validate_expr(
                    function,
                    milliseconds,
                    &format!("{path}.milliseconds"),
                    depth + 1,
                );
                let duration = self
                    .program
                    .prelude
                    .duration
                    .is_some_and(|duration| nominal_is(&actual, duration));
                if !types_compatible(&Type::Int, &actual) && !duration {
                    self.type_mismatch(
                        &self
                            .program
                            .prelude
                            .duration
                            .map_or(Type::Int, |id| Type::Nominal(id, Vec::new())),
                        &actual,
                        milliseconds.span,
                        &format!("{path}.milliseconds"),
                    );
                }
                Some(Type::Task(Box::new(Type::Unit)))
            }
            ExprKind::WaitFd { descriptor, .. } => {
                let actual = self.validate_expr(
                    function,
                    descriptor,
                    &format!("{path}.descriptor"),
                    depth + 1,
                );
                if !types_compatible(&Type::Int, &actual) {
                    self.type_mismatch(
                        &Type::Int,
                        &actual,
                        descriptor.span,
                        &format!("{path}.descriptor"),
                    );
                }
                Some(Type::Task(Box::new(Type::Unit)))
            }
            ExprKind::TaskJoin { mode, arguments } => {
                let argument_types = arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        self.validate_expr(
                            function,
                            argument,
                            &format!("{path}.arguments[{index}]"),
                            depth + 1,
                        )
                    })
                    .collect::<Vec<_>>();
                if let [Type::List(element)] = argument_types.as_slice()
                    && let Type::Task(output) = element.as_ref()
                {
                    let output = output.as_ref().clone();
                    Some(Type::Task(Box::new(match mode {
                        TaskJoinMode::All => Type::List(Box::new(output)),
                        TaskJoinMode::Settled => {
                            Type::List(Box::new(self.task_outcome_type(output)))
                        }
                        TaskJoinMode::Any => output,
                        TaskJoinMode::Race => self.task_outcome_type(output),
                    })))
                } else {
                    let mut outputs = Vec::with_capacity(argument_types.len());
                    for (index, argument) in argument_types.into_iter().enumerate() {
                        if let Type::Task(output) = argument {
                            outputs.push(*output);
                        } else {
                            self.push(
                                MirValidationCode::ExpressionShape,
                                "Task join argument must be Task or one List[Task[T]]",
                                expression.span,
                                format!("{path}.arguments[{index}]"),
                            );
                            outputs.push(Type::Error);
                        }
                    }
                    let output = match mode {
                        TaskJoinMode::All => Type::Tuple(outputs),
                        TaskJoinMode::Settled => Type::Tuple(
                            outputs
                                .into_iter()
                                .map(|output| self.task_outcome_type(output))
                                .collect(),
                        ),
                        TaskJoinMode::Any | TaskJoinMode::Race => {
                            let output = outputs.first().cloned().unwrap_or(Type::Error);
                            if outputs
                                .iter()
                                .any(|candidate| !types_compatible(&output, candidate))
                            {
                                self.push(
                                    MirValidationCode::TypeMismatch,
                                    "Task.any/race arguments must have one result type",
                                    expression.span,
                                    path,
                                );
                            }
                            if *mode == TaskJoinMode::Race {
                                self.task_outcome_type(output)
                            } else {
                                output
                            }
                        }
                    };
                    Some(Type::Task(Box::new(output)))
                }
            }
        };
        if inferred
            .as_ref()
            .is_some_and(|actual| !types_compatible(&expression.ty, actual))
        {
            self.type_mismatch(
                &expression.ty,
                inferred.as_ref().expect("checked above"),
                expression.span,
                path,
            );
        }
        expression.ty.clone()
    }

    fn validate_unary(
        &mut self,
        operator: UnaryOp,
        operand: &Type,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        match operator {
            UnaryOp::Negate if is_numeric(operand) => Some(operand.clone()),
            UnaryOp::Not if types_compatible(&Type::Bool, operand) => Some(Type::Bool),
            _ => {
                self.push(
                    MirValidationCode::ExpressionShape,
                    format!("operator {operator:?} cannot be applied to {operand:?}"),
                    span,
                    path,
                );
                None
            }
        }
    }

    fn validate_binary(
        &mut self,
        operator: BinaryOp,
        left: &Type,
        right: &Type,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let same = types_compatible(left, right);
        let valid = match operator {
            BinaryOp::Equal | BinaryOp::NotEqual => {
                same && self.supports_value_equality(left) && self.supports_value_equality(right)
            }
            BinaryOp::Add
            | BinaryOp::Subtract
            | BinaryOp::Multiply
            | BinaryOp::Divide
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => same && is_numeric(left),
            BinaryOp::And | BinaryOp::Or => same && types_compatible(&Type::Bool, left),
        };
        if !valid {
            self.push(
                MirValidationCode::ExpressionShape,
                format!("operator {operator:?} has incompatible operands {left:?} and {right:?}"),
                span,
                path,
            );
            return None;
        }
        Some(match operator {
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide => {
                left.clone()
            }
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
            | BinaryOp::And
            | BinaryOp::Or => Type::Bool,
        })
    }

    fn supports_value_equality(&self, ty: &Type) -> bool {
        self.supports_value_equality_inner(ty, &mut Vec::new(), 0)
    }

    fn reject_obligation_loss(&mut self, ty: &Type, span: Span, path: &str, action: &str) {
        let obligations = self.value_obligations_inner(ty, &mut Vec::new(), 0);
        if obligations.is_empty() {
            return;
        }

        let mut kinds = Vec::with_capacity(3);
        if obligations.resource {
            kinds.push("File or Socket");
        }
        if obligations.task {
            kinds.push("an unconsumed Task");
        }
        if obligations.unresolved {
            kinds.push("unresolved generic obligations");
        }
        self.push(
            MirValidationCode::ObligationShape,
            format!(
                "checked MIR cannot {action} a value containing {}",
                kinds.join(", ")
            ),
            span,
            path,
        );
    }

    fn value_obligations_inner(
        &self,
        ty: &Type,
        active: &mut Vec<Type>,
        depth: u16,
    ) -> ValueObligations {
        if depth >= MAX_VALIDATION_DEPTH {
            return ValueObligations {
                unresolved: true,
                ..ValueObligations::default()
            };
        }
        match ty {
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Error
            | Type::View { .. } => ValueObligations::default(),
            Type::Parameter(_) | Type::AssociatedProjection { .. } => ValueObligations {
                unresolved: true,
                ..ValueObligations::default()
            },
            Type::Task(_) => ValueObligations {
                task: true,
                ..ValueObligations::default()
            },
            Type::Tuple(elements) => {
                let mut obligations = ValueObligations::default();
                for element in elements {
                    obligations.merge(self.value_obligations_inner(element, active, depth + 1));
                }
                obligations
            }
            Type::List(element) | Type::TaskOutcome(element) => {
                self.value_obligations_inner(element, active, depth + 1)
            }
            Type::Nominal(type_id, arguments) => {
                if self.program.prelude.file == Some(*type_id)
                    || self.program.prelude.socket == Some(*type_id)
                {
                    return ValueObligations {
                        resource: true,
                        ..ValueObligations::default()
                    };
                }
                if active.contains(ty) {
                    return ValueObligations::default();
                }
                let Some(definition) = self.program.type_def(*type_id) else {
                    // The ordinary type-reference validator owns malformed nominal
                    // references; obligation hardening only reasons about valid types.
                    return ValueObligations::default();
                };
                if arguments.len() != definition.type_parameters as usize {
                    return ValueObligations::default();
                }

                active.push(ty.clone());
                let mut obligations = ValueObligations::default();
                match &definition.kind {
                    TypeDefKind::Record { fields, .. } => {
                        for field in fields {
                            let field_ty = substitute_type(&field.ty, arguments);
                            obligations.merge(self.value_obligations_inner(
                                &field_ty,
                                active,
                                depth + 1,
                            ));
                        }
                    }
                    TypeDefKind::Enum { variants } => {
                        for payload in variants.iter().flat_map(|variant| &variant.payload) {
                            let payload = substitute_type(payload, arguments);
                            obligations.merge(self.value_obligations_inner(
                                &payload,
                                active,
                                depth + 1,
                            ));
                        }
                    }
                    TypeDefKind::Refined { base, .. } => {
                        let base = substitute_type(base, arguments);
                        obligations.merge(self.value_obligations_inner(&base, active, depth + 1));
                    }
                }
                active.pop();
                obligations
            }
        }
    }

    fn supports_value_equality_inner(&self, ty: &Type, active: &mut Vec<Type>, depth: u16) -> bool {
        if depth >= MAX_VALIDATION_DEPTH {
            return false;
        }
        match ty {
            Type::Never
            | Type::Error
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text => true,
            Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Task(_)
            | Type::View { .. } => false,
            Type::Tuple(elements) => elements
                .iter()
                .all(|element| self.supports_value_equality_inner(element, active, depth + 1)),
            Type::List(element) | Type::TaskOutcome(element) => {
                self.supports_value_equality_inner(element, active, depth + 1)
            }
            Type::Nominal(type_id, arguments) => {
                if self.program.prelude.file == Some(*type_id)
                    || self.program.prelude.socket == Some(*type_id)
                    || self.program.prelude.io_error == Some(*type_id)
                {
                    return false;
                }
                if self.program.prelude.text_map == Some(*type_id) {
                    return arguments.len() == 1
                        && self.supports_value_equality_inner(&arguments[0], active, depth + 1);
                }
                if active.contains(ty) {
                    return true;
                }
                let Some(definition) = self.program.type_def(*type_id) else {
                    return false;
                };
                active.push(ty.clone());
                let result = match &definition.kind {
                    TypeDefKind::Record { fields, .. } => fields.iter().all(|field| {
                        let field_ty = substitute_type(&field.ty, arguments);
                        self.supports_value_equality_inner(&field_ty, active, depth + 1)
                    }),
                    TypeDefKind::Enum { variants } => variants.iter().all(|variant| {
                        variant.payload.iter().all(|payload| {
                            let payload = substitute_type(payload, arguments);
                            self.supports_value_equality_inner(&payload, active, depth + 1)
                        })
                    }),
                    TypeDefKind::Refined { base, .. } => {
                        let base = substitute_type(base, arguments);
                        self.supports_value_equality_inner(&base, active, depth + 1)
                    }
                };
                active.pop();
                result
            }
        }
    }

    fn validate_contract_binary(
        &mut self,
        operator: BinaryOp,
        left: &Type,
        right: &Type,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        // Contract evaluation reads a refined value through its declared base;
        // it cannot construct a refined value, so this normalization remains
        // local to the contract language rather than making executable MIR
        // nominal types globally transparent.
        let left = self.contract_base_type(left, span, &format!("{path}.left"))?;
        let right = self.contract_base_type(right, span, &format!("{path}.right"))?;
        let result = match operator {
            BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
                if left == Type::Float && right == Type::Float =>
            {
                Some(Type::Float)
            }
            BinaryOp::Equal | BinaryOp::NotEqual
                if left == right
                    && matches!(left, Type::Bool | Type::Int | Type::Float | Type::Text) =>
            {
                Some(Type::Bool)
            }
            BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual
                if left == right && matches!(left, Type::Int | Type::Float) =>
            {
                Some(Type::Bool)
            }
            BinaryOp::And | BinaryOp::Or if left == Type::Bool && right == Type::Bool => {
                Some(Type::Bool)
            }
            _ => None,
        };
        if result.is_none() {
            self.push(
                MirValidationCode::ContractShape,
                format!(
                    "operator {operator:?} is outside the total contract subset for {left:?} and {right:?}"
                ),
                span,
                path,
            );
        }
        result
    }

    fn validate_contract_unary(
        &mut self,
        operator: UnaryOp,
        operand: &Type,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let operand = self.contract_base_type(operand, span, path)?;
        match (operator, operand) {
            (UnaryOp::Negate, Type::Float) => Some(Type::Float),
            (UnaryOp::Not, Type::Bool) => Some(Type::Bool),
            (operator, operand) => {
                self.push(
                    MirValidationCode::ContractShape,
                    format!(
                        "operator {operator:?} is outside the total contract subset for {operand:?}"
                    ),
                    span,
                    path,
                );
                None
            }
        }
    }

    fn contract_base_type(&mut self, ty: &Type, span: Span, path: &str) -> Option<Type> {
        let mut current = ty.clone();
        for _ in 0..64 {
            let Type::Nominal(id, arguments) = &current else {
                return Some(current);
            };
            let Some(definition) = self.program.type_def(*id) else {
                self.invalid_type(*id, span, path);
                return None;
            };
            let TypeDefKind::Refined { base, .. } = &definition.kind else {
                return Some(current);
            };
            current = substitute_type(base, arguments);
        }
        self.push(
            MirValidationCode::ContractShape,
            "refined contract operand chain exceeds the validation limit",
            span,
            path,
        );
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_record_expr(
        &mut self,
        function: &Function,
        type_id: crate::TypeId,
        type_arguments: &[Type],
        fields: &[Expr],
        construction: ConstructionMode,
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        let Some(definition) = self.program.type_def(type_id).cloned() else {
            self.invalid_type(type_id, expression.span, format!("{path}.record_type"));
            for (index, field) in fields.iter().enumerate() {
                self.validate_expr(function, field, &format!("{path}.fields[{index}]"), depth);
            }
            return None;
        };
        let TypeDefKind::Record {
            fields: definitions,
            invariant,
        } = definition.kind
        else {
            self.push(
                MirValidationCode::RecordShape,
                "record construction references a non-record type",
                expression.span,
                path,
            );
            return None;
        };
        self.validate_nominal_instantiation(
            function,
            type_id,
            type_arguments,
            expression.span,
            &format!("{path}.type_arguments"),
            depth,
        );
        if fields.len() != definitions.len() {
            self.push(
                MirValidationCode::RecordShape,
                format!(
                    "record expects {} field value(s), received {}",
                    definitions.len(),
                    fields.len()
                ),
                expression.span,
                path,
            );
        }
        for (index, field) in fields.iter().enumerate() {
            let actual =
                self.validate_expr(function, field, &format!("{path}.fields[{index}]"), depth);
            let expected = definitions
                .get(index)
                .map(|expected| substitute_type(&expected.ty, type_arguments));
            if expected
                .as_ref()
                .is_some_and(|expected| !types_compatible(expected, &actual))
            {
                self.type_mismatch(
                    expected.as_ref().expect("checked above"),
                    &actual,
                    field.span,
                    &format!("{path}.fields[{index}]"),
                );
            }
        }
        let valid_construction = matches!(
            (invariant.is_some(), construction),
            (false, ConstructionMode::Plain)
                | (true, ConstructionMode::Proven | ConstructionMode::Runtime)
        );
        if !valid_construction {
            self.push(
                MirValidationCode::RecordShape,
                "record construction mode does not match its invariant boundary",
                expression.span,
                path,
            );
        }
        if construction == ConstructionMode::Runtime {
            let success = Type::Nominal(type_id, type_arguments.to_vec());
            self.expected_result_type(
                success,
                self.program.prelude.constraint_error,
                "constraint_error",
                expression.span,
                path,
            )
        } else {
            Some(Type::Nominal(type_id, type_arguments.to_vec()))
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_variant_expr(
        &mut self,
        function: &Function,
        type_id: crate::TypeId,
        type_arguments: &[Type],
        variant_id: VariantId,
        payload: &[Expr],
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        self.validate_nominal_instantiation(
            function,
            type_id,
            type_arguments,
            expression.span,
            &format!("{path}.type_arguments"),
            depth,
        );
        let Some(variant) = self.variant(type_id, variant_id) else {
            self.invalid_variant(type_id, variant_id, expression.span, path);
            for (index, value) in payload.iter().enumerate() {
                self.validate_expr(function, value, &format!("{path}.payload[{index}]"), depth);
            }
            return None;
        };
        if payload.len() != variant.payload.len() {
            self.push(
                MirValidationCode::VariantShape,
                format!(
                    "variant expects {} payload value(s), received {}",
                    variant.payload.len(),
                    payload.len()
                ),
                expression.span,
                path,
            );
        }
        for (index, value) in payload.iter().enumerate() {
            let actual =
                self.validate_expr(function, value, &format!("{path}.payload[{index}]"), depth);
            let expected = variant
                .payload
                .get(index)
                .map(|expected| substitute_type(expected, type_arguments));
            if expected
                .as_ref()
                .is_some_and(|expected| !types_compatible(expected, &actual))
            {
                self.type_mismatch(
                    expected.as_ref().expect("checked above"),
                    &actual,
                    value.span,
                    &format!("{path}.payload[{index}]"),
                );
            }
        }
        Some(Type::Nominal(type_id, type_arguments.to_vec()))
    }

    fn validate_nominal_instantiation(
        &mut self,
        function: &Function,
        type_id: crate::TypeId,
        type_arguments: &[Type],
        span: Span,
        path: &str,
        depth: u16,
    ) {
        let Some(definition) = self.program.type_def(type_id) else {
            return;
        };
        if type_arguments.len() != definition.type_parameters as usize {
            self.push(
                MirValidationCode::TypeMismatch,
                format!(
                    "type `{}` expects {} generic argument(s), found {}",
                    definition.name,
                    definition.type_parameters,
                    type_arguments.len()
                ),
                span,
                path,
            );
        }
        for (index, argument) in type_arguments.iter().enumerate() {
            self.validate_type(argument, span, &format!("{path}[{index}]"), depth);
            self.validate_function_type(
                function,
                argument,
                span,
                &format!("{path}[{index}]"),
                depth,
            );
            self.validate_view_placement(argument, false, span, &format!("{path}[{index}]"), depth);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_refine_expr(
        &mut self,
        function: &Function,
        type_id: crate::TypeId,
        value: &Expr,
        construction: ConstructionMode,
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        let actual = self.validate_expr(function, value, &format!("{path}.value"), depth);
        let Some(definition) = self.program.type_def(type_id) else {
            self.invalid_type(type_id, expression.span, format!("{path}.refined_type"));
            return None;
        };
        let TypeDefKind::Refined { base, .. } = &definition.kind else {
            self.push(
                MirValidationCode::ExpressionShape,
                "refinement construction references a non-refined type",
                expression.span,
                path,
            );
            return None;
        };
        if !types_compatible(base, &actual) {
            self.type_mismatch(base, &actual, value.span, &format!("{path}.value"));
        }
        match construction {
            ConstructionMode::Plain => {
                self.push(
                    MirValidationCode::ExpressionShape,
                    "refinement construction cannot use the plain record mode",
                    expression.span,
                    path,
                );
                None
            }
            ConstructionMode::Proven => Some(Type::Nominal(type_id, Vec::new())),
            ConstructionMode::Runtime => self.expected_result_type(
                Type::Nominal(type_id, Vec::new()),
                self.program.prelude.constraint_error,
                "constraint_error",
                expression.span,
                path,
            ),
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn validate_call(
        &mut self,
        function: &Function,
        target: &CallTarget,
        type_arguments: &[Type],
        arguments: &[CallArgument],
        witnesses: &[WitnessRef],
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        match target {
            CallTarget::Direct(function_id) | CallTarget::Inherent(function_id) => {
                let is_inherent = matches!(target, CallTarget::Inherent(_));
                let Some(callee) = self.program.function(*function_id).cloned() else {
                    self.invalid_function(*function_id, expression.span, format!("{path}.target"));
                    self.validate_untyped_arguments(function, arguments, path, depth);
                    return None;
                };
                if type_arguments.len() != callee.type_parameters as usize {
                    self.push(
                        MirValidationCode::CallArity,
                        format!(
                            "call supplies {} type argument(s), but `{}` expects {}",
                            type_arguments.len(),
                            callee.name,
                            callee.type_parameters
                        ),
                        expression.span,
                        format!("{path}.type_arguments"),
                    );
                }
                for (index, argument) in type_arguments.iter().enumerate() {
                    self.validate_type(
                        argument,
                        expression.span,
                        &format!("{path}.type_arguments[{index}]"),
                        depth,
                    );
                    self.validate_view_placement(
                        argument,
                        false,
                        expression.span,
                        &format!("{path}.type_arguments[{index}]"),
                        depth,
                    );
                    self.validate_function_type(
                        function,
                        argument,
                        expression.span,
                        &format!("{path}.type_arguments[{index}]"),
                        depth,
                    );
                }
                if witnesses.len() != callee.witness_params.len() {
                    self.push(
                        MirValidationCode::WitnessArity,
                        format!(
                            "call supplies {} witness argument(s), but `{}` expects {}",
                            witnesses.len(),
                            callee.name,
                            callee.witness_params.len()
                        ),
                        expression.span,
                        format!("{path}.witnesses"),
                    );
                }
                let mut resolved_witnesses = Vec::with_capacity(witnesses.len());
                for (index, witness) in witnesses.iter().enumerate() {
                    let expected = callee
                        .witness_params
                        .get(index)
                        .map(|parameter| substitute_witness_param(parameter, type_arguments))
                        .map(|parameter| {
                            self.instantiate_call_witness_param(
                                &parameter,
                                &resolved_witnesses,
                                expression.span,
                                &format!("{path}.witnesses[{index}].expected"),
                                depth,
                            )
                        });
                    resolved_witnesses.push(self.validate_witness_ref(
                        function,
                        witness,
                        expected.as_ref(),
                        expression.span,
                        &format!("{path}.witnesses[{index}]"),
                        depth,
                    ));
                }
                self.validate_call_arguments(
                    function,
                    arguments,
                    &callee,
                    type_arguments,
                    &resolved_witnesses,
                    path,
                    depth,
                );
                if is_inherent && callee.receiver.is_none() {
                    self.push(
                        MirValidationCode::ReceiverShape,
                        "an inherent call target must have a receiver",
                        expression.span,
                        format!("{path}.target"),
                    );
                }
                if !is_inherent && callee.receiver.is_some() {
                    self.push(
                        MirValidationCode::ReceiverShape,
                        "a receiver function must use CallTarget::Inherent",
                        expression.span,
                        format!("{path}.target"),
                    );
                }
                let return_ty = substitute_type(&callee.return_ty, type_arguments);
                let output = self.instantiate_call_type(
                    &return_ty,
                    &resolved_witnesses,
                    expression.span,
                    &format!("{path}.return_ty"),
                    depth,
                );
                Some(if callee.is_async {
                    Type::Task(Box::new(output))
                } else {
                    output
                })
            }
            CallTarget::StaticConcept {
                requirement,
                witness,
                dispatch_type,
            } => {
                let actual_arguments =
                    self.validate_untyped_arguments(function, arguments, path, depth);
                let Some(requirement_def) = self.program.requirement(*requirement).cloned() else {
                    self.invalid_requirement(
                        *requirement,
                        expression.span,
                        format!("{path}.target.requirement"),
                    );
                    return None;
                };
                if type_arguments.len() != requirement_def.method_type_parameters as usize {
                    self.push(
                        MirValidationCode::CallArity,
                        format!(
                            "concept call supplies {} method type argument(s), but requirement expects {}",
                            type_arguments.len(),
                            requirement_def.method_type_parameters
                        ),
                        expression.span,
                        format!("{path}.target.type_arguments"),
                    );
                }
                for (index, argument) in type_arguments.iter().enumerate() {
                    self.validate_type(
                        argument,
                        expression.span,
                        &format!("{path}.target.type_arguments[{index}]"),
                        depth,
                    );
                    self.validate_view_placement(
                        argument,
                        false,
                        expression.span,
                        &format!("{path}.target.type_arguments[{index}]"),
                        depth,
                    );
                    self.validate_function_type(
                        function,
                        argument,
                        expression.span,
                        &format!("{path}.target.type_arguments[{index}]"),
                        depth,
                    );
                }
                self.validate_type(
                    dispatch_type,
                    expression.span,
                    &format!("{path}.target.dispatch_type"),
                    depth,
                );
                self.validate_function_type(
                    function,
                    dispatch_type,
                    expression.span,
                    &format!("{path}.target.dispatch_type"),
                    depth,
                );
                self.validate_view_placement(
                    dispatch_type,
                    false,
                    expression.span,
                    &format!("{path}.target.dispatch_type"),
                    depth,
                );
                if witnesses.len() != requirement_def.witness_params.len() {
                    self.push(
                        MirValidationCode::WitnessArity,
                        format!(
                            "concept call supplies {} method proof(s), but requirement expects {}",
                            witnesses.len(),
                            requirement_def.witness_params.len()
                        ),
                        expression.span,
                        format!("{path}.witnesses"),
                    );
                }
                let expected_main = WitnessParam {
                    target: dispatch_type.clone(),
                    concept: requirement_def.concept,
                    bindings: BTreeMap::new(),
                    span: expression.span,
                };
                if requirement_def.receiver.is_some()
                    && actual_arguments.first().is_some_and(|actual| {
                        actual
                            .as_ref()
                            .is_some_and(|actual| !types_compatible(dispatch_type, actual))
                    })
                {
                    self.type_mismatch(
                        dispatch_type,
                        actual_arguments[0].as_ref().expect("checked above"),
                        expression.span,
                        &format!("{path}.target.dispatch_type"),
                    );
                }
                self.validate_receiver_argument_mode(
                    requirement_def.receiver,
                    arguments.first(),
                    expression.span,
                    path,
                );
                let resolved = self.validate_witness_ref(
                    function,
                    witness,
                    Some(&expected_main),
                    expression.span,
                    &format!("{path}.target.witness"),
                    depth,
                )?;
                if resolved.proof.concept != requirement_def.concept {
                    self.push(
                        MirValidationCode::WitnessShape,
                        "static concept call witness belongs to another concept",
                        expression.span,
                        format!("{path}.target.witness"),
                    );
                }
                if let Some(witness_id) = resolved.definition
                    && self
                        .program
                        .witness(witness_id)
                        .is_some_and(|definition| !definition.methods.contains_key(requirement))
                {
                    self.push(
                        MirValidationCode::WitnessShape,
                        format!("witness has no method for requirement #{}", requirement.0),
                        expression.span,
                        format!("{path}.target"),
                    );
                }
                let mut resolved_method_witnesses = Vec::with_capacity(witnesses.len());
                for (index, witness) in witnesses.iter().enumerate() {
                    let expected =
                        requirement_def
                            .witness_params
                            .get(index)
                            .and_then(|parameter| {
                                self.instantiate_requirement_witness_param_with(
                                    parameter,
                                    &resolved.proof.target,
                                    &resolved.proof.bindings,
                                    resolved.projection_witness,
                                    MethodTypes::Arguments(type_arguments),
                                    RequirementProofs::Resolved(&resolved_method_witnesses),
                                    &format!("{path}.witnesses[{index}]"),
                                )
                            });
                    let resolved_method = self.validate_witness_ref(
                        function,
                        witness,
                        expected.as_ref(),
                        expression.span,
                        &format!("{path}.witnesses[{index}]"),
                        depth,
                    );
                    resolved_method_witnesses.push(resolved_method);
                }
                let expected_arguments = requirement_def
                    .params
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        self.instantiate_requirement_type_with(
                            ty,
                            &resolved.proof.target,
                            &resolved.proof.bindings,
                            resolved.projection_witness,
                            MethodTypes::Arguments(type_arguments),
                            RequirementProofs::Resolved(&resolved_method_witnesses),
                            expression.span,
                            &format!("{path}.signature.params[{index}]"),
                            depth,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?;
                self.compare_call_types(
                    arguments,
                    &actual_arguments,
                    &expected_arguments,
                    expression.span,
                    path,
                );
                self.instantiate_requirement_type_with(
                    &requirement_def.return_ty,
                    &resolved.proof.target,
                    &resolved.proof.bindings,
                    resolved.projection_witness,
                    MethodTypes::Arguments(type_arguments),
                    RequirementProofs::Resolved(&resolved_method_witnesses),
                    expression.span,
                    &format!("{path}.signature.return_ty"),
                    depth,
                )
            }
            CallTarget::Dynamic { requirement } => {
                if !type_arguments.is_empty() {
                    self.unused_call_type_arguments(expression.span, path);
                }
                if !witnesses.is_empty() {
                    self.unused_call_witnesses(expression.span, path);
                }
                let argument_types =
                    self.validate_untyped_arguments(function, arguments, path, depth);
                let Some(requirement_def) = self.program.requirement(*requirement).cloned() else {
                    self.invalid_requirement(
                        *requirement,
                        expression.span,
                        format!("{path}.target.requirement"),
                    );
                    return None;
                };
                let concept = self.program.concept(requirement_def.concept).cloned();
                if concept.as_ref().is_none_or(|concept| !concept.dynamic) {
                    self.push(
                        MirValidationCode::RequirementShape,
                        "dynamic call requirement must belong to a dyn concept",
                        expression.span,
                        format!("{path}.target"),
                    );
                }
                if argument_types.is_empty() {
                    self.push(
                        MirValidationCode::CallArity,
                        "a dynamic call requires an erased interface receiver argument",
                        expression.span,
                        format!("{path}.arguments"),
                    );
                    return None;
                }
                let Some(Type::View {
                    mutable,
                    concept,
                    bindings,
                }) = argument_types[0].as_ref()
                else {
                    self.push(
                        MirValidationCode::ReceiverShape,
                        "dynamic call argument zero must have an erased concept interface type",
                        expression.span,
                        format!("{path}.arguments[0]"),
                    );
                    return None;
                };
                if *concept != requirement_def.concept {
                    self.push(
                        MirValidationCode::WitnessShape,
                        "dynamic receiver concept differs from the requirement owner",
                        expression.span,
                        format!("{path}.arguments[0]"),
                    );
                }
                if requirement_def.receiver == Some(Receiver::Mutable) && !mutable {
                    self.push(
                        MirValidationCode::ReceiverShape,
                        "a `mut self` requirement needs a mutable interface receiver",
                        expression.span,
                        format!("{path}.arguments[0]"),
                    );
                }
                self.validate_receiver_argument_mode(
                    requirement_def.receiver,
                    arguments.first(),
                    expression.span,
                    path,
                );
                let view_ty = argument_types[0].clone().expect("checked above");
                let expected_arguments = requirement_def
                    .params
                    .iter()
                    .enumerate()
                    .map(|(index, ty)| {
                        if index == 0 && requirement_def.receiver.is_some() {
                            Some(view_ty.clone())
                        } else {
                            self.instantiate_requirement_type_with(
                                ty,
                                &view_ty,
                                bindings,
                                None,
                                MethodTypes::Arguments(&[]),
                                RequirementProofs::Unavailable,
                                expression.span,
                                &format!("{path}.signature.params[{index}]"),
                                depth,
                            )
                        }
                    })
                    .collect::<Option<Vec<_>>>()?;
                self.compare_call_types(
                    arguments,
                    &argument_types,
                    &expected_arguments,
                    expression.span,
                    path,
                );
                self.instantiate_requirement_type_with(
                    &requirement_def.return_ty,
                    &view_ty,
                    bindings,
                    None,
                    MethodTypes::Arguments(&[]),
                    RequirementProofs::Unavailable,
                    expression.span,
                    &format!("{path}.signature.return_ty"),
                    depth,
                )
            }
            CallTarget::Builtin(builtin) => {
                if !type_arguments.is_empty() {
                    self.unused_call_type_arguments(expression.span, path);
                }
                if !witnesses.is_empty() {
                    self.unused_call_witnesses(expression.span, path);
                }
                self.validate_builtin(function, *builtin, arguments, expression, path, depth)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_call_arguments(
        &mut self,
        function: &Function,
        arguments: &[CallArgument],
        callee: &Function,
        type_arguments: &[Type],
        witnesses: &[Option<ResolvedWitness>],
        path: &str,
        depth: u16,
    ) {
        if arguments.len() != callee.params.len() {
            self.push(
                MirValidationCode::CallArity,
                format!(
                    "call supplies {} argument(s), but `{}` expects {}",
                    arguments.len(),
                    callee.name,
                    callee.params.len()
                ),
                callee.span,
                format!("{path}.arguments"),
            );
        }
        self.validate_receiver_argument_mode(callee.receiver, arguments.first(), callee.span, path);
        for (index, argument) in arguments.iter().enumerate() {
            if index != 0 && matches!(argument, CallArgument::InOut(_)) {
                self.push(
                    MirValidationCode::ReceiverShape,
                    "only a mutable receiver may use an inout argument",
                    callee.span,
                    format!("{path}.arguments[{index}]"),
                );
            }
            let expected_decl = callee.params.get(index);
            let expected_ty = expected_decl.map(|parameter| {
                let parameter_ty = substitute_type(&parameter.ty, type_arguments);
                self.instantiate_call_type(
                    &parameter_ty,
                    witnesses,
                    parameter.span,
                    &format!("{path}.arguments[{index}].expected"),
                    depth,
                )
            });
            let actual = self.validate_call_argument(
                function,
                argument,
                expected_decl,
                &format!("{path}.arguments[{index}]"),
                depth,
            );
            if let (Some(expected), Some(actual)) = (expected_ty, actual)
                && !types_compatible(&expected, &actual)
            {
                self.type_mismatch(
                    &expected,
                    &actual,
                    expected_decl.expect("type came from declaration").span,
                    &format!("{path}.arguments[{index}]"),
                );
            }
        }
    }

    fn instantiate_call_type(
        &mut self,
        ty: &Type,
        witnesses: &[Option<ResolvedWitness>],
        span: Span,
        path: &str,
        depth: u16,
    ) -> Type {
        if !self.enter(depth, span, path) {
            return Type::Error;
        }
        match ty {
            Type::AssociatedProjection {
                witness,
                associated,
            } => {
                let Some(Some(resolved)) = witnesses.get(*witness as usize) else {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        format!(
                            "call cannot resolve associated projection through witness #{witness}"
                        ),
                        span,
                        path,
                    );
                    return Type::Error;
                };
                if let Some(binding) = resolved.proof.bindings.get(associated) {
                    binding.clone()
                } else if let Some(witness) = resolved.projection_witness {
                    Type::AssociatedProjection {
                        witness,
                        associated: associated.clone(),
                    }
                } else {
                    self.push(
                        MirValidationCode::WitnessShape,
                        format!("call witness does not bind associated type `{associated}`"),
                        span,
                        path,
                    );
                    Type::Error
                }
            }
            Type::Nominal(id, arguments) => Type::Nominal(
                *id,
                arguments
                    .iter()
                    .enumerate()
                    .map(|(index, argument)| {
                        self.instantiate_call_type(
                            argument,
                            witnesses,
                            span,
                            &format!("{path}.arguments[{index}]"),
                            depth + 1,
                        )
                    })
                    .collect(),
            ),
            Type::View {
                mutable,
                concept,
                bindings,
            } => Type::View {
                mutable: *mutable,
                concept: *concept,
                bindings: bindings
                    .iter()
                    .map(|(name, binding)| {
                        (
                            name.clone(),
                            self.instantiate_call_type(
                                binding,
                                witnesses,
                                span,
                                &format!("{path}.bindings[{name:?}]"),
                                depth + 1,
                            ),
                        )
                    })
                    .collect(),
            },
            _ => ty.clone(),
        }
    }

    fn instantiate_call_witness_param(
        &mut self,
        parameter: &WitnessParam,
        witnesses: &[Option<ResolvedWitness>],
        span: Span,
        path: &str,
        depth: u16,
    ) -> WitnessParam {
        WitnessParam {
            target: self.instantiate_call_type(
                &parameter.target,
                witnesses,
                span,
                &format!("{path}.target"),
                depth,
            ),
            concept: parameter.concept,
            bindings: parameter
                .bindings
                .iter()
                .map(|(name, ty)| {
                    (
                        name.clone(),
                        self.instantiate_call_type(
                            ty,
                            witnesses,
                            span,
                            &format!("{path}.bindings[{name:?}]"),
                            depth,
                        ),
                    )
                })
                .collect(),
            span: parameter.span,
        }
    }

    fn compare_call_types(
        &mut self,
        arguments: &[CallArgument],
        actual: &[Option<Type>],
        expected: &[Type],
        span: Span,
        path: &str,
    ) {
        if arguments.len() != expected.len() {
            self.push(
                MirValidationCode::CallArity,
                format!(
                    "concept call supplies {} value argument(s), but requirement expects {}",
                    arguments.len(),
                    expected.len()
                ),
                span,
                format!("{path}.arguments"),
            );
        }
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            if actual
                .as_ref()
                .is_some_and(|actual| !types_compatible(expected, actual))
            {
                self.type_mismatch(
                    expected,
                    actual.as_ref().expect("checked above"),
                    span,
                    &format!("{path}.arguments[{index}]"),
                );
            }
        }
    }

    fn validate_untyped_arguments(
        &mut self,
        function: &Function,
        arguments: &[CallArgument],
        path: &str,
        depth: u16,
    ) -> Vec<Option<Type>> {
        arguments
            .iter()
            .enumerate()
            .map(|(index, argument)| {
                self.validate_call_argument(
                    function,
                    argument,
                    None,
                    &format!("{path}.arguments[{index}]"),
                    depth,
                )
            })
            .collect()
    }

    fn validate_call_argument(
        &mut self,
        function: &Function,
        argument: &CallArgument,
        expected: Option<&LocalDecl>,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        match argument {
            CallArgument::Value(expression) => {
                Some(self.validate_expr(function, expression, path, depth))
            }
            CallArgument::InOut(place) => {
                let mutable_view = Self::local_decl(function, place.local)
                    .is_some_and(|local| matches!(local.ty, Type::View { mutable: true, .. }));
                self.validate_place(
                    function,
                    place,
                    !mutable_view,
                    expected.map_or(Span::default(), |p| p.span),
                    path,
                )
            }
        }
    }

    fn validate_receiver_argument_mode(
        &mut self,
        receiver: Option<Receiver>,
        argument: Option<&CallArgument>,
        span: Span,
        path: &str,
    ) {
        let valid = matches!(
            (receiver, argument),
            (None, None | Some(CallArgument::Value(_)))
                | (Some(Receiver::Readonly), Some(CallArgument::Value(_)))
                | (Some(Receiver::Mutable), Some(CallArgument::InOut(_)))
        );
        if !valid {
            self.push(
                MirValidationCode::ReceiverShape,
                "receiver argument mode does not match readonly/mutable requirement",
                span,
                format!("{path}.arguments[0]"),
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_builtin(
        &mut self,
        function: &Function,
        builtin: Builtin,
        arguments: &[CallArgument],
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        let types = self.validate_untyped_arguments(function, arguments, path, depth);
        if !self.validate_builtin_arity_and_mode(builtin, arguments, expression.span, path) {
            return None;
        }
        if matches!(
            builtin,
            Builtin::DurationMilliseconds | Builtin::DurationAsMilliseconds
        ) {
            let result = self.validate_duration_builtin(builtin, &types);
            if result.is_none() {
                self.invalid_builtin_shape(builtin, &types, expression.span, path);
            }
            return result;
        }
        if matches!(
            builtin,
            Builtin::FileOpenRead
                | Builtin::FileCreate
                | Builtin::FileOpenReadPath
                | Builtin::FileCreatePath
                | Builtin::FileTryOpenRead
                | Builtin::FileTryCreate
                | Builtin::FileTryOpenReadPath
                | Builtin::FileTryCreatePath
                | Builtin::FileReadText
                | Builtin::FileWriteText
                | Builtin::FileTryReadText
                | Builtin::FileTryWriteText
                | Builtin::FileClose
                | Builtin::SocketConnect
                | Builtin::SocketTryConnect
                | Builtin::SocketReadText
                | Builtin::SocketWriteText
                | Builtin::SocketTryReadText
                | Builtin::SocketTryWriteText
                | Builtin::SocketClose
        ) {
            let result = self.validate_io_builtin(builtin, &types, expression.span, path);
            if result.is_none() {
                self.invalid_builtin_shape(builtin, &types, expression.span, path);
            }
            return result;
        }
        match builtin {
            Builtin::TextMapNew => {
                let map = self.program.prelude.text_map?;
                match &expression.ty {
                    Type::Nominal(actual, arguments) if *actual == map && arguments.len() == 1 => {
                        Some(expression.ty.clone())
                    }
                    _ => None,
                }
            }
            Builtin::TextMapLength
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.text_map) =>
            {
                Some(Type::Int)
            }
            Builtin::TextMapContains
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.text_map)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                Some(Type::Bool)
            }
            Builtin::TextMapGet
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.text_map)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                let value = Self::nominal_builtin_type_argument(
                    &types,
                    0,
                    self.program.prelude.text_map,
                    0,
                )?;
                self.expected_option_type(value, expression.span, path)
            }
            Builtin::TextMapInsert
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.text_map)
                    && types_compatible(&Type::Text, types[1].as_ref()?)
                    && Self::nominal_builtin_type_argument(
                        &types,
                        0,
                        self.program.prelude.text_map,
                        0,
                    )
                    .is_some_and(|value| {
                        types[2]
                            .as_ref()
                            .is_some_and(|actual| types_compatible(&value, actual))
                    }) =>
            {
                Some(types[0].clone()?)
            }
            Builtin::TextMapRemove
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.text_map)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                Some(types[0].clone()?)
            }
            Builtin::JsonParse if types_compatible(&Type::Text, types[0].as_ref()?) => {
                let json = self.expected_prelude_nominal(
                    self.program.prelude.json,
                    "json",
                    expression.span,
                    path,
                )?;
                self.expected_result_type(
                    json,
                    self.program.prelude.json_error,
                    "json_error",
                    expression.span,
                    path,
                )
            }
            Builtin::JsonFormat
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.json) =>
            {
                self.expected_result_type(
                    Type::Text,
                    self.program.prelude.json_error,
                    "json_error",
                    expression.span,
                    path,
                )
            }
            Builtin::IoErrorKind
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.io_error) =>
            {
                self.expected_prelude_nominal(
                    self.program.prelude.io_error_kind,
                    "io_error_kind",
                    expression.span,
                    path,
                )
            }
            Builtin::IoErrorMessage
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.io_error) =>
            {
                Some(Type::Text)
            }
            Builtin::LogDebug | Builtin::LogInfo | Builtin::LogWarn | Builtin::LogError
                if types_compatible(&Type::Text, types[0].as_ref()?) =>
            {
                Some(Type::Unit)
            }
            Builtin::LogWrite
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.log_level)
                    && types_compatible(&Type::Text, types[1].as_ref()?)
                    && Self::nominal_builtin_argument(&types, 2, self.program.prelude.text_map)
                    && Self::nominal_builtin_type_argument(
                        &types,
                        2,
                        self.program.prelude.text_map,
                        0,
                    )
                    .is_some_and(|value| types_compatible(&Type::Text, &value)) =>
            {
                Some(Type::Unit)
            }
            Builtin::ProcessArguments => Some(Type::List(Box::new(Type::Text))),
            Builtin::ProcessEnvironment if types_compatible(&Type::Text, types[0].as_ref()?) => {
                let Some(option) = self.program.prelude.option else {
                    self.push(
                        MirValidationCode::InvalidTypeReference,
                        "environment returns Option, but prelude.option is absent",
                        expression.span,
                        path,
                    );
                    return None;
                };
                Some(Type::Nominal(option, vec![Type::Text]))
            }
            Builtin::ParseInt if types_compatible(&Type::Text, types[0].as_ref()?) => self
                .expected_result_type(
                    Type::Int,
                    self.program.prelude.parse_int_error,
                    "parse_int_error",
                    expression.span,
                    path,
                ),
            Builtin::IsFinite if self.is_float_like(types[0].as_ref()?) => Some(Type::Bool),
            Builtin::ParseFloat if types_compatible(&Type::Text, types[0].as_ref()?) => self
                .expected_result_type(
                    Type::Float,
                    self.program.prelude.parse_float_error,
                    "parse_float_error",
                    expression.span,
                    path,
                ),
            Builtin::FormatFloat if self.is_float_like(types[0].as_ref()?) => Some(Type::Text),
            Builtin::TextLength if types_compatible(&Type::Text, types[0].as_ref()?) => {
                Some(Type::Int)
            }
            Builtin::TextGet
                if types_compatible(&Type::Text, types[0].as_ref()?)
                    && types_compatible(&Type::Int, types[1].as_ref()?) =>
            {
                self.expected_option_type(Type::Text, expression.span, path)
            }
            Builtin::TextConcat
                if types_compatible(&Type::Text, types[0].as_ref()?)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                Some(Type::Text)
            }
            Builtin::TextContains
                if types_compatible(&Type::Text, types[0].as_ref()?)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                Some(Type::Bool)
            }
            Builtin::TextEncodeUtf8 if types_compatible(&Type::Text, types[0].as_ref()?) => self
                .expected_prelude_nominal(
                    self.program.prelude.bytes,
                    "bytes",
                    expression.span,
                    path,
                ),
            Builtin::BytesLength
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.bytes) =>
            {
                Some(Type::Int)
            }
            Builtin::BytesGet
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.bytes)
                    && types_compatible(&Type::Int, types[1].as_ref()?) =>
            {
                self.expected_option_type(Type::Int, expression.span, path)
            }
            Builtin::BytesAppend
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.bytes)
                    && Self::nominal_builtin_argument(&types, 1, self.program.prelude.bytes) =>
            {
                self.program
                    .prelude
                    .bytes
                    .map(|id| Type::Nominal(id, Vec::new()))
            }
            Builtin::BytesDecodeUtf8
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.bytes) =>
            {
                self.expected_result_type(
                    Type::Text,
                    self.program.prelude.decode_text_error,
                    "decode_text_error",
                    expression.span,
                    path,
                )
            }
            Builtin::PathFromText if types_compatible(&Type::Text, types[0].as_ref()?) => {
                let path_ty = self.expected_prelude_nominal(
                    self.program.prelude.path,
                    "path",
                    expression.span,
                    path,
                )?;
                self.expected_result_type(
                    path_ty,
                    self.program.prelude.path_error,
                    "path_error",
                    expression.span,
                    path,
                )
            }
            Builtin::PathAsText
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.path) =>
            {
                Some(Type::Text)
            }
            Builtin::PathJoin
                if Self::nominal_builtin_argument(&types, 0, self.program.prelude.path)
                    && Self::nominal_builtin_argument(&types, 1, self.program.prelude.path) =>
            {
                let path_ty = self.expected_prelude_nominal(
                    self.program.prelude.path,
                    "path",
                    expression.span,
                    path,
                )?;
                self.expected_result_type(
                    path_ty,
                    self.program.prelude.path_error,
                    "path_error",
                    expression.span,
                    path,
                )
            }
            Builtin::TaskFaultCode | Builtin::TaskFaultMessage
                if self.program.prelude.task_fault.is_some_and(|task_fault| {
                    types[0]
                        .as_ref()
                        .is_some_and(|actual| nominal_is(actual, task_fault))
                }) =>
            {
                Some(Type::Text)
            }
            Builtin::ListAdd | Builtin::ListLength | Builtin::ListGet => {
                self.validate_list_builtin(builtin, arguments, &types, expression.span, path)
            }
            _ => {
                let argument = types[0].as_ref()?;
                self.push(
                    MirValidationCode::BuiltinShape,
                    format!("builtin {builtin:?} cannot accept {argument:?}"),
                    expression.span,
                    path,
                );
                None
            }
        }
    }

    fn validate_builtin_arity_and_mode(
        &mut self,
        builtin: Builtin,
        arguments: &[CallArgument],
        span: Span,
        path: &str,
    ) -> bool {
        let expected_arity = match builtin {
            Builtin::ListAdd
            | Builtin::ListGet
            | Builtin::TextGet
            | Builtin::TextConcat
            | Builtin::TextContains
            | Builtin::BytesGet
            | Builtin::BytesAppend
            | Builtin::PathJoin
            | Builtin::FileWriteText
            | Builtin::SocketConnect
            | Builtin::SocketWriteText
            | Builtin::TextMapContains
            | Builtin::TextMapGet
            | Builtin::TextMapRemove
            | Builtin::FileTryWriteText
            | Builtin::SocketTryConnect
            | Builtin::SocketTryWriteText => 2,
            Builtin::TextMapInsert | Builtin::LogWrite => 3,
            Builtin::ProcessArguments | Builtin::TextMapNew => 0,
            Builtin::IsFinite
            | Builtin::ParseFloat
            | Builtin::FormatFloat
            | Builtin::TextLength
            | Builtin::TextEncodeUtf8
            | Builtin::BytesLength
            | Builtin::BytesDecodeUtf8
            | Builtin::PathFromText
            | Builtin::PathAsText
            | Builtin::ListLength
            | Builtin::ProcessEnvironment
            | Builtin::ParseInt
            | Builtin::TaskFaultCode
            | Builtin::TaskFaultMessage
            | Builtin::DurationMilliseconds
            | Builtin::DurationAsMilliseconds
            | Builtin::FileOpenRead
            | Builtin::FileCreate
            | Builtin::FileOpenReadPath
            | Builtin::FileCreatePath
            | Builtin::FileReadText
            | Builtin::FileClose
            | Builtin::SocketReadText
            | Builtin::SocketClose
            | Builtin::TextMapLength
            | Builtin::JsonParse
            | Builtin::JsonFormat
            | Builtin::IoErrorKind
            | Builtin::IoErrorMessage
            | Builtin::FileTryOpenRead
            | Builtin::FileTryCreate
            | Builtin::FileTryOpenReadPath
            | Builtin::FileTryCreatePath
            | Builtin::FileTryReadText
            | Builtin::SocketTryReadText
            | Builtin::LogDebug
            | Builtin::LogInfo
            | Builtin::LogWarn
            | Builtin::LogError => 1,
        };
        if arguments.len() != expected_arity {
            self.push(
                MirValidationCode::CallArity,
                format!("builtin {builtin:?} expects exactly {expected_arity} argument(s)"),
                span,
                format!("{path}.arguments"),
            );
            return false;
        }
        if matches!(builtin, Builtin::FileClose | Builtin::SocketClose)
            && !matches!(arguments.first(), Some(CallArgument::InOut(_)))
        {
            self.push(
                MirValidationCode::ReceiverShape,
                format!("builtin {builtin:?} requires an inout resource place"),
                span,
                format!("{path}.arguments[0]"),
            );
        }
        true
    }

    fn validate_duration_builtin(&self, builtin: Builtin, types: &[Option<Type>]) -> Option<Type> {
        match builtin {
            Builtin::DurationMilliseconds if types_compatible(&Type::Int, types[0].as_ref()?) => {
                self.program
                    .prelude
                    .duration
                    .map(|id| Type::Nominal(id, Vec::new()))
            }
            Builtin::DurationAsMilliseconds
                if Self::nominal_builtin_argument(types, 0, self.program.prelude.duration) =>
            {
                Some(Type::Int)
            }
            _ => None,
        }
    }

    fn validate_io_builtin(
        &mut self,
        builtin: Builtin,
        types: &[Option<Type>],
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let file = self.program.prelude.file;
        let socket = self.program.prelude.socket;
        match builtin {
            Builtin::FileOpenRead | Builtin::FileCreate
                if types_compatible(&Type::Text, types[0].as_ref()?) =>
            {
                file.map(|id| Type::Task(Box::new(Type::Nominal(id, Vec::new()))))
            }
            Builtin::FileOpenReadPath | Builtin::FileCreatePath
                if Self::nominal_builtin_argument(types, 0, self.program.prelude.path) =>
            {
                file.map(|id| Type::Task(Box::new(Type::Nominal(id, Vec::new()))))
            }
            Builtin::FileTryOpenRead | Builtin::FileTryCreate
                if types_compatible(&Type::Text, types[0].as_ref()?) =>
            {
                let file = Type::Nominal(file?, Vec::new());
                self.expected_io_result_task(file, span, path)
            }
            Builtin::FileTryOpenReadPath | Builtin::FileTryCreatePath
                if Self::nominal_builtin_argument(types, 0, self.program.prelude.path) =>
            {
                let file = Type::Nominal(file?, Vec::new());
                self.expected_io_result_task(file, span, path)
            }
            Builtin::FileReadText if Self::nominal_builtin_argument(types, 0, file) => {
                Some(Type::Task(Box::new(Type::Text)))
            }
            Builtin::FileWriteText
                if Self::nominal_builtin_argument(types, 0, file)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                Some(Type::Task(Box::new(Type::Unit)))
            }
            Builtin::FileTryReadText if Self::nominal_builtin_argument(types, 0, file) => {
                self.expected_io_result_task(Type::Text, span, path)
            }
            Builtin::FileTryWriteText
                if Self::nominal_builtin_argument(types, 0, file)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                self.expected_io_result_task(Type::Unit, span, path)
            }
            Builtin::FileClose if Self::nominal_builtin_argument(types, 0, file) => {
                Some(Type::Unit)
            }
            Builtin::SocketConnect
                if types_compatible(&Type::Text, types[0].as_ref()?)
                    && types_compatible(&Type::Int, types[1].as_ref()?) =>
            {
                socket.map(|id| Type::Task(Box::new(Type::Nominal(id, Vec::new()))))
            }
            Builtin::SocketTryConnect
                if types_compatible(&Type::Text, types[0].as_ref()?)
                    && types_compatible(&Type::Int, types[1].as_ref()?) =>
            {
                let socket = Type::Nominal(socket?, Vec::new());
                self.expected_io_result_task(socket, span, path)
            }
            Builtin::SocketReadText if Self::nominal_builtin_argument(types, 0, socket) => {
                Some(Type::Task(Box::new(Type::Text)))
            }
            Builtin::SocketWriteText
                if Self::nominal_builtin_argument(types, 0, socket)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                Some(Type::Task(Box::new(Type::Unit)))
            }
            Builtin::SocketTryReadText if Self::nominal_builtin_argument(types, 0, socket) => {
                self.expected_io_result_task(Type::Text, span, path)
            }
            Builtin::SocketTryWriteText
                if Self::nominal_builtin_argument(types, 0, socket)
                    && types_compatible(&Type::Text, types[1].as_ref()?) =>
            {
                self.expected_io_result_task(Type::Unit, span, path)
            }
            Builtin::SocketClose if Self::nominal_builtin_argument(types, 0, socket) => {
                Some(Type::Unit)
            }
            _ => None,
        }
    }

    fn nominal_builtin_argument(
        types: &[Option<Type>],
        index: usize,
        expected: Option<crate::TypeId>,
    ) -> bool {
        expected.is_some_and(|id| {
            types
                .get(index)
                .and_then(Option::as_ref)
                .is_some_and(|actual| nominal_is(actual, id))
        })
    }

    fn nominal_builtin_type_argument(
        types: &[Option<Type>],
        index: usize,
        expected: Option<crate::TypeId>,
        argument: usize,
    ) -> Option<Type> {
        let expected = expected?;
        let Type::Nominal(actual, arguments) = types.get(index)?.as_ref()? else {
            return None;
        };
        (*actual == expected).then(|| arguments.get(argument).cloned())?
    }

    fn invalid_builtin_shape(
        &mut self,
        builtin: Builtin,
        types: &[Option<Type>],
        span: Span,
        path: &str,
    ) {
        let argument = types.first().and_then(Option::as_ref);
        self.push(
            MirValidationCode::BuiltinShape,
            format!("builtin {builtin:?} cannot accept {argument:?}"),
            span,
            path,
        );
    }

    fn validate_list_builtin(
        &mut self,
        builtin: Builtin,
        arguments: &[CallArgument],
        types: &[Option<Type>],
        span: Span,
        path: &str,
    ) -> Option<Type> {
        match builtin {
            Builtin::ListAdd => {
                let Type::List(element) = types[0].as_ref()? else {
                    self.push(
                        MirValidationCode::BuiltinShape,
                        "ListAdd receiver must be List",
                        span,
                        path,
                    );
                    return None;
                };
                if !matches!(arguments.first(), Some(CallArgument::InOut(_))) {
                    self.push(
                        MirValidationCode::ReceiverShape,
                        "ListAdd receiver must be passed inout",
                        span,
                        format!("{path}.arguments[0]"),
                    );
                }
                if !types_compatible(element, types[1].as_ref()?) {
                    self.type_mismatch(element, types[1].as_ref()?, span, path);
                }
                Some(Type::Unit)
            }
            Builtin::ListLength => match types[0].as_ref()? {
                Type::List(_) => Some(Type::Int),
                argument => {
                    self.push(
                        MirValidationCode::BuiltinShape,
                        format!("builtin {builtin:?} cannot accept {argument:?}"),
                        span,
                        path,
                    );
                    None
                }
            },
            Builtin::ListGet => {
                let Type::List(element) = types[0].as_ref()? else {
                    self.push(
                        MirValidationCode::BuiltinShape,
                        "ListGet receiver must be List",
                        span,
                        path,
                    );
                    return None;
                };
                if !types_compatible(&Type::Int, types[1].as_ref()?) {
                    self.type_mismatch(&Type::Int, types[1].as_ref()?, span, path);
                }
                let Some(option) = self.program.prelude.option else {
                    self.push(
                        MirValidationCode::InvalidTypeReference,
                        "ListGet returns Option, but prelude.option is absent",
                        span,
                        path,
                    );
                    return None;
                };
                Some(Type::Nominal(option, vec![element.as_ref().clone()]))
            }
            _ => unreachable!("caller filters List builtins"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_make_view(
        &mut self,
        function: &Function,
        value: &Expr,
        writeback: Option<&Place>,
        witness_ref: &WitnessRef,
        mutable: bool,
        expression: &Expr,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        let value_ty = self.validate_expr(function, value, &format!("{path}.value"), depth);
        if !expr_definitely_diverges(value, 0) {
            self.reject_obligation_loss(
                &value_ty,
                value.span,
                &format!("{path}.value"),
                "erase into a dynamic concept interface",
            );
        }
        if let Some(writeback) = writeback {
            if !matches!(&value.kind, ExprKind::Copy(source) if source == writeback) {
                self.push(
                    MirValidationCode::BorrowShape,
                    "borrowed interface adaptation must copy exactly its writeback place",
                    value.span,
                    format!("{path}.value"),
                );
            }
            let writeback_ty = self.validate_place(
                function,
                writeback,
                mutable,
                expression.span,
                &format!("{path}.writeback"),
            );
            if writeback_ty
                .as_ref()
                .is_some_and(|writeback| !types_compatible(&value_ty, writeback))
            {
                self.type_mismatch(
                    &value_ty,
                    writeback_ty.as_ref().expect("checked"),
                    expression.span,
                    path,
                );
            }
        }
        let Type::View {
            mutable: declared_mutable,
            concept,
            bindings,
        } = &expression.ty
        else {
            self.push(
                MirValidationCode::ExpressionShape,
                "internal interface adaptation must declare an erased concept interface result",
                expression.span,
                path,
            );
            return None;
        };
        if *declared_mutable != mutable {
            self.push(
                MirValidationCode::ExpressionShape,
                "internal interface adaptation mutability does not match its result type",
                expression.span,
                path,
            );
        }
        let expected = Some(WitnessParam {
            target: value_ty.clone(),
            concept: *concept,
            bindings: bindings.clone(),
            span: expression.span,
        });
        if let Some(resolved) = self.validate_witness_ref(
            function,
            witness_ref,
            expected.as_ref(),
            expression.span,
            &format!("{path}.witness"),
            0,
        ) && (resolved.proof.concept != *concept
            || resolved.proof.bindings != *bindings
            || !types_compatible(&resolved.proof.target, &value_ty))
        {
            self.push(
                MirValidationCode::WitnessShape,
                "interface type and argument do not match the supplied witness proof",
                expression.span,
                path,
            );
        }
        Some(expression.ty.clone())
    }

    fn validate_reborrow_view(
        &mut self,
        function: &Function,
        owner: &Place,
        mutable: bool,
        expression: &Expr,
        path: &str,
    ) -> Option<Type> {
        let owner_ty = self.validate_place(
            function,
            owner,
            mutable,
            expression.span,
            &format!("{path}.owner"),
        );
        if owner_ty.as_ref() != Some(&expression.ty) {
            self.push(
                MirValidationCode::ExpressionShape,
                "interface reborrow owner and result types must match",
                expression.span,
                path,
            );
        }
        let Type::View {
            mutable: declared_mutable,
            ..
        } = &expression.ty
        else {
            self.push(
                MirValidationCode::ExpressionShape,
                "interface reborrow must produce an erased interface value",
                expression.span,
                path,
            );
            return None;
        };
        if *declared_mutable != mutable {
            self.push(
                MirValidationCode::ExpressionShape,
                "interface reborrow mutability does not match its type",
                expression.span,
                path,
            );
        }
        Some(expression.ty.clone())
    }

    fn validate_match(
        &mut self,
        function: &Function,
        scrutinee: &Type,
        arms: &[MatchArm],
        expected: &Type,
        path: &str,
        depth: u16,
    ) -> Type {
        let mut joined = Type::Never;
        let mut previous_patterns = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            let arm_path = format!("{path}.arms[{index}]");
            if !self.pattern_vector_useful(
                std::slice::from_ref(scrutinee),
                &previous_patterns,
                std::slice::from_ref(&arm.pattern),
                0,
            ) {
                self.push(
                    MirValidationCode::PatternShape,
                    "match arm is unreachable because previous arms already cover it",
                    arm.value.span,
                    &arm_path,
                );
            }
            previous_patterns.push(vec![arm.pattern.clone()]);
            let mut bindings = Vec::new();
            self.validate_pattern(
                &arm.pattern,
                scrutinee,
                &mut bindings,
                &format!("{arm_path}.pattern"),
                arm.value.span,
                depth,
            );
            self.validate_match_bindings(function, arm, &bindings, &arm_path);
            let value_ty =
                self.validate_expr(function, &arm.value, &format!("{arm_path}.value"), depth);
            if !types_compatible(expected, &value_ty) {
                self.type_mismatch(expected, &value_ty, arm.value.span, &arm_path);
            }
            if !flow_types_compatible(&joined, &value_ty) {
                self.type_mismatch(&joined, &value_ty, arm.value.span, &arm_path);
            } else if joined == Type::Never {
                joined = value_ty;
            }
        }
        if arms.is_empty() {
            self.push(
                MirValidationCode::PatternShape,
                "match expression must contain at least one arm",
                Span::default(),
                path,
            );
            Type::Error
        } else if !self.patterns_exhaustive(scrutinee, arms.iter().map(|arm| &arm.pattern)) {
            self.push(
                MirValidationCode::PatternShape,
                "match patterns are not exhaustive",
                Span::default(),
                path,
            );
            joined
        } else {
            joined
        }
    }

    fn validate_match_bindings(
        &mut self,
        function: &Function,
        arm: &MatchArm,
        bindings: &[Type],
        arm_path: &str,
    ) {
        if bindings.len() != arm.bindings.len() {
            self.push(
                MirValidationCode::PatternShape,
                format!(
                    "pattern binds {} value(s), but arm lists {} binding local(s)",
                    bindings.len(),
                    arm.bindings.len()
                ),
                arm.value.span,
                format!("{arm_path}.bindings"),
            );
        }
        let mut seen = BTreeSet::new();
        for (binding_index, local_id) in arm.bindings.iter().copied().enumerate() {
            let binding_path = format!("{arm_path}.bindings[{binding_index}]");
            if !seen.insert(local_id) {
                self.push(
                    MirValidationCode::DuplicateLocal,
                    format!("match arm binds local #{} more than once", local_id.0),
                    arm.value.span,
                    &binding_path,
                );
            }
            let Some(local) = Self::local_decl(function, local_id) else {
                self.invalid_local(local_id, arm.value.span, binding_path);
                continue;
            };
            if !function
                .locals
                .iter()
                .any(|candidate| candidate.id == local_id)
            {
                self.push(
                    MirValidationCode::InvalidLocalReference,
                    "match bindings must target declared non-parameter locals",
                    local.span,
                    &binding_path,
                );
            }
            if bindings
                .get(binding_index)
                .is_some_and(|ty| !types_compatible(&local.ty, ty))
            {
                self.type_mismatch(
                    &local.ty,
                    &bindings[binding_index],
                    local.span,
                    &binding_path,
                );
            }
        }
    }

    fn validate_pattern(
        &mut self,
        pattern: &Pattern,
        expected: &Type,
        bindings: &mut Vec<Type>,
        path: &str,
        span: Span,
        depth: u16,
    ) {
        if !self.enter(depth, span, path) {
            return;
        }
        match pattern {
            Pattern::Wildcard => {}
            Pattern::Binding => bindings.push(expected.clone()),
            Pattern::Constant(constant) => {
                let actual = constant_type(constant);
                if !types_compatible(expected, &actual) {
                    self.type_mismatch(expected, &actual, span, path);
                }
            }
            Pattern::Variant {
                ty,
                variant,
                payload,
            } => {
                if !nominal_is(expected, *ty) {
                    self.push(
                        MirValidationCode::PatternShape,
                        format!(
                            "variant pattern for type #{} cannot match {expected:?}",
                            ty.0
                        ),
                        span,
                        path,
                    );
                }
                let Some(definition) = self.variant(*ty, *variant) else {
                    self.invalid_variant(*ty, *variant, span, path);
                    return;
                };
                let arguments = match expected {
                    Type::Nominal(expected_id, arguments) if expected_id == ty => arguments.clone(),
                    _ => Vec::new(),
                };
                if payload.len() != definition.payload.len() {
                    self.push(
                        MirValidationCode::PatternShape,
                        format!(
                            "variant pattern expects {} payload item(s), received {}",
                            definition.payload.len(),
                            payload.len()
                        ),
                        span,
                        path,
                    );
                }
                for (index, nested) in payload.iter().enumerate() {
                    if let Some(payload_ty) = definition.payload.get(index) {
                        let payload_ty = substitute_type(payload_ty, &arguments);
                        self.validate_pattern(
                            nested,
                            &payload_ty,
                            bindings,
                            &format!("{path}.payload[{index}]"),
                            span,
                            depth + 1,
                        );
                    }
                }
            }
        }
    }

    fn validate_contract(&mut self, contract: &Contract, env: &ContractEnv, path: &str) {
        let ty = self.validate_contract_expr(&contract.expression, env, path, 0);
        if ty
            .as_ref()
            .is_some_and(|actual| !types_compatible(&Type::Bool, actual))
        {
            self.type_mismatch(
                &Type::Bool,
                &ty.expect("checked above"),
                contract.span,
                path,
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_contract_expr(
        &mut self,
        expression: &ContractExpr,
        env: &ContractEnv,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        if !self.enter(depth, expression.span, path) {
            return None;
        }
        match &expression.kind {
            ContractExprKind::Constant(Constant::Unit) => {
                self.push(
                    MirValidationCode::ContractShape,
                    "Unit is not a contract-predicate literal",
                    expression.span,
                    path,
                );
                None
            }
            ContractExprKind::Constant(constant) => Some(constant_type(constant)),
            ContractExprKind::Value(value) => match value {
                ContractValue::SelfValue => self.contract_value(
                    env.receiver.clone(),
                    "`self` is unavailable in this contract",
                    expression.span,
                    path,
                ),
                ContractValue::Result => self.contract_value(
                    env.result.clone(),
                    "`result` is only available in an ensures contract",
                    expression.span,
                    path,
                ),
                ContractValue::Argument(index) => self.contract_value(
                    env.arguments.get(*index as usize).cloned(),
                    "contract argument index is out of bounds",
                    expression.span,
                    path,
                ),
                ContractValue::OldSelf => self.contract_old_value(
                    env.allow_old.then(|| env.receiver.clone()).flatten(),
                    "`old(self)` is only available in an ensures contract",
                    expression.span,
                    path,
                ),
                ContractValue::OldArgument(index) => self.contract_old_value(
                    env.allow_old
                        .then(|| env.arguments.get(*index as usize).cloned())
                        .flatten(),
                    "old argument is unavailable or its index is out of bounds",
                    expression.span,
                    path,
                ),
            },
            ContractExprKind::Binding(index) => self.contract_value(
                env.bindings.get(*index as usize).cloned(),
                "contract arm binding index is out of bounds",
                expression.span,
                path,
            ),
            ContractExprKind::Field(value, field) => {
                let owner =
                    self.validate_contract_expr(value, env, &format!("{path}.owner"), depth + 1)?;
                self.project_type(owner, *field, expression.span, &format!("{path}.field"))
            }
            ContractExprKind::Unary(operator, operand) => {
                let operand = self.validate_contract_expr(
                    operand,
                    env,
                    &format!("{path}.operand"),
                    depth + 1,
                )?;
                self.validate_contract_unary(*operator, &operand, expression.span, path)
            }
            ContractExprKind::Binary(operator, left, right) => {
                let left =
                    self.validate_contract_expr(left, env, &format!("{path}.left"), depth + 1);
                let right =
                    self.validate_contract_expr(right, env, &format!("{path}.right"), depth + 1);
                match (left, right) {
                    (Some(left), Some(right)) => self.validate_contract_binary(
                        *operator,
                        &left,
                        &right,
                        expression.span,
                        path,
                    ),
                    _ => None,
                }
            }
            ContractExprKind::IsFinite(value) => {
                let value =
                    self.validate_contract_expr(value, env, &format!("{path}.value"), depth + 1)?;
                if self.is_float_like(&value) {
                    Some(Type::Bool)
                } else {
                    self.push(
                        MirValidationCode::ContractShape,
                        format!("is_finite cannot accept {value:?}"),
                        expression.span,
                        path,
                    );
                    None
                }
            }
            ContractExprKind::Match { scrutinee, arms } => {
                let scrutinee = self.validate_contract_expr(
                    scrutinee,
                    env,
                    &format!("{path}.scrutinee"),
                    depth + 1,
                )?;
                self.validate_contract_arms(&scrutinee, arms, env, path, depth + 1)
            }
        }
    }

    fn validate_contract_arms(
        &mut self,
        scrutinee: &Type,
        arms: &[ContractArm],
        env: &ContractEnv,
        path: &str,
        depth: u16,
    ) -> Option<Type> {
        let mut result = None;
        let mut previous_patterns = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            let arm_path = format!("{path}.arms[{index}]");
            if !self.pattern_vector_useful(
                std::slice::from_ref(scrutinee),
                &previous_patterns,
                std::slice::from_ref(&arm.pattern),
                0,
            ) {
                self.push(
                    MirValidationCode::ContractShape,
                    "contract match arm is unreachable because previous arms already cover it",
                    arm.value.span,
                    &arm_path,
                );
            }
            previous_patterns.push(vec![arm.pattern.clone()]);
            let mut bindings = Vec::new();
            self.validate_pattern(
                &arm.pattern,
                scrutinee,
                &mut bindings,
                &format!("{arm_path}.pattern"),
                arm.value.span,
                depth,
            );
            if bindings.len() != arm.bindings.len() {
                self.push(
                    MirValidationCode::ContractShape,
                    format!(
                        "contract pattern binds {} value(s), but arm declares {} binding slot(s)",
                        bindings.len(),
                        arm.bindings.len()
                    ),
                    arm.value.span,
                    format!("{arm_path}.bindings"),
                );
            }
            for (binding_index, (actual, declared)) in
                bindings.iter().zip(&arm.bindings).enumerate()
            {
                self.validate_type(
                    declared,
                    arm.value.span,
                    &format!("{arm_path}.bindings[{binding_index}]"),
                    depth,
                );
                if !types_compatible(actual, declared) {
                    self.type_mismatch(
                        actual,
                        declared,
                        arm.value.span,
                        &format!("{arm_path}.bindings[{binding_index}]"),
                    );
                }
            }
            let mut arm_env = env.clone();
            arm_env.bindings.extend(arm.bindings.iter().cloned());
            let arm_ty = self.validate_contract_expr(
                &arm.value,
                &arm_env,
                &format!("{arm_path}.value"),
                depth,
            );
            if let (Some(expected), Some(actual)) = (&result, &arm_ty)
                && !types_compatible(expected, actual)
            {
                self.type_mismatch(expected, actual, arm.value.span, &arm_path);
            }
            if result.is_none() {
                result = arm_ty;
            }
        }
        if arms.is_empty() {
            self.push(
                MirValidationCode::ContractShape,
                "contract match must contain at least one arm",
                Span::default(),
                path,
            );
        } else if !self.patterns_exhaustive(scrutinee, arms.iter().map(|arm| &arm.pattern)) {
            self.push(
                MirValidationCode::ContractShape,
                "contract match patterns are not exhaustive",
                Span::default(),
                path,
            );
        }
        result
    }

    fn patterns_exhaustive<'pattern>(
        &self,
        expected: &Type,
        patterns: impl Iterator<Item = &'pattern Pattern>,
    ) -> bool {
        let rows = patterns
            .map(|pattern| vec![pattern.clone()])
            .collect::<Vec<_>>();
        self.pattern_matrix_exhaustive(std::slice::from_ref(expected), &rows, 0)
    }

    fn pattern_vector_useful(
        &self,
        expected: &[Type],
        rows: &[Vec<Pattern>],
        candidate: &[Pattern],
        depth: u16,
    ) -> bool {
        if expected.is_empty() || candidate.is_empty() {
            return rows.is_empty();
        }
        if depth >= MAX_VALIDATION_DEPTH {
            return true;
        }

        let tail = &expected[1..];
        let candidate_tail = &candidate[1..];
        match &candidate[0] {
            Pattern::Wildcard | Pattern::Binding => {
                self.wildcard_pattern_useful(expected, rows, candidate_tail, depth)
            }
            Pattern::Constant(constant) => {
                let specialized = specialize_constant_rows(rows, constant);
                self.pattern_vector_useful(tail, &specialized, candidate_tail, depth + 1)
            }
            Pattern::Variant {
                ty,
                variant,
                payload,
            } => {
                let Some(definition) = self.variant(*ty, *variant) else {
                    return true;
                };
                let arguments = match &expected[0] {
                    Type::Nominal(expected_id, arguments) if expected_id == ty => arguments,
                    _ => return true,
                };
                if payload.len() != definition.payload.len() {
                    return true;
                }
                let mut specialized_types = definition
                    .payload
                    .iter()
                    .map(|ty| substitute_type(ty, arguments))
                    .collect::<Vec<_>>();
                specialized_types.extend_from_slice(tail);
                let specialized = specialize_variant_rows(rows, *ty, *variant, payload.len());
                let mut specialized_candidate = payload.clone();
                specialized_candidate.extend_from_slice(candidate_tail);
                self.pattern_vector_useful(
                    &specialized_types,
                    &specialized,
                    &specialized_candidate,
                    depth + 1,
                )
            }
        }
    }

    fn wildcard_pattern_useful(
        &self,
        expected: &[Type],
        rows: &[Vec<Pattern>],
        candidate_tail: &[Pattern],
        depth: u16,
    ) -> bool {
        let tail = &expected[1..];
        match &expected[0] {
            Type::Bool => [false, true].into_iter().any(|value| {
                let specialized = specialize_constant_rows(rows, &Constant::Bool(value));
                self.pattern_vector_useful(tail, &specialized, candidate_tail, depth + 1)
            }),
            Type::Unit => {
                let specialized = specialize_constant_rows(rows, &Constant::Unit);
                self.pattern_vector_useful(tail, &specialized, candidate_tail, depth + 1)
            }
            Type::Nominal(type_id, arguments) => {
                let Some(TypeDef {
                    kind: TypeDefKind::Enum { variants },
                    ..
                }) = self.program.type_def(*type_id)
                else {
                    let default = default_pattern_rows(rows);
                    return self.pattern_vector_useful(tail, &default, candidate_tail, depth + 1);
                };
                variants.iter().any(|variant| {
                    let mut specialized_types = variant
                        .payload
                        .iter()
                        .map(|ty| substitute_type(ty, arguments))
                        .collect::<Vec<_>>();
                    let payload_arity = specialized_types.len();
                    specialized_types.extend_from_slice(tail);
                    let specialized =
                        specialize_variant_rows(rows, *type_id, variant.id, payload_arity);
                    let mut specialized_candidate = vec![Pattern::Wildcard; payload_arity];
                    specialized_candidate.extend_from_slice(candidate_tail);
                    self.pattern_vector_useful(
                        &specialized_types,
                        &specialized,
                        &specialized_candidate,
                        depth + 1,
                    )
                })
            }
            Type::Never => false,
            Type::Int
            | Type::Float
            | Type::Text
            | Type::Tuple(_)
            | Type::List(_)
            | Type::Task(_)
            | Type::TaskOutcome(_)
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::View { .. }
            | Type::Error => {
                let default = default_pattern_rows(rows);
                self.pattern_vector_useful(tail, &default, candidate_tail, depth + 1)
            }
        }
    }

    fn pattern_matrix_exhaustive(
        &self,
        expected: &[Type],
        rows: &[Vec<Pattern>],
        depth: u16,
    ) -> bool {
        if expected.is_empty() {
            return !rows.is_empty();
        }
        if rows.iter().any(|row| {
            row.len() == expected.len()
                && row
                    .iter()
                    .all(|pattern| matches!(pattern, Pattern::Wildcard | Pattern::Binding))
        }) {
            return true;
        }
        if depth >= MAX_VALIDATION_DEPTH {
            return false;
        }

        let tail = &expected[1..];
        match &expected[0] {
            Type::Bool => [false, true].into_iter().all(|value| {
                let rows = specialize_constant_rows(rows, &Constant::Bool(value));
                self.pattern_matrix_exhaustive(tail, &rows, depth + 1)
            }),
            Type::Unit => {
                let rows = specialize_constant_rows(rows, &Constant::Unit);
                self.pattern_matrix_exhaustive(tail, &rows, depth + 1)
            }
            Type::Nominal(type_id, arguments) => {
                let Some(TypeDef {
                    kind: TypeDefKind::Enum { variants },
                    ..
                }) = self.program.type_def(*type_id)
                else {
                    return false;
                };
                variants.iter().all(|variant| {
                    let mut specialized_types = variant
                        .payload
                        .iter()
                        .map(|ty| substitute_type(ty, arguments))
                        .collect::<Vec<_>>();
                    specialized_types.extend_from_slice(tail);
                    let rows =
                        specialize_variant_rows(rows, *type_id, variant.id, variant.payload.len());
                    self.pattern_matrix_exhaustive(&specialized_types, &rows, depth + 1)
                })
            }
            Type::Never => true,
            Type::Int
            | Type::Float
            | Type::Text
            | Type::Tuple(_)
            | Type::List(_)
            | Type::Task(_)
            | Type::TaskOutcome(_)
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::View { .. }
            | Type::Error => {
                let rows = default_pattern_rows(rows);
                self.pattern_matrix_exhaustive(tail, &rows, depth + 1)
            }
        }
    }

    fn contract_value(
        &mut self,
        value: Option<Type>,
        message: &str,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        if value.is_none() {
            self.push(MirValidationCode::ContractShape, message, span, path);
        }
        value
    }

    fn contract_old_value(
        &mut self,
        value: Option<Type>,
        message: &str,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let value = self.contract_value(value, message, span, path)?;
        if type_contains_view(&value) {
            self.push(
                MirValidationCode::ContractShape,
                "old(...) cannot snapshot an erased interface parameter",
                span,
                path,
            );
            None
        } else {
            Some(value)
        }
    }

    fn validate_place(
        &mut self,
        function: &Function,
        place: &Place,
        require_mutable: bool,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let Some(local) = Self::local_decl(function, place.local) else {
            self.invalid_local(place.local, span, path);
            return None;
        };
        if require_mutable && !local.mutable {
            self.push(
                MirValidationCode::ImmutablePlace,
                format!("local #{} is not mutable", place.local.0),
                span,
                path,
            );
        }
        let mut ty = local.ty.clone();
        for (index, field) in place.projection.iter().copied().enumerate() {
            ty = self.project_type(ty, field, span, &format!("{path}.projection[{index}]"))?;
        }
        Some(ty)
    }

    fn project_type(&mut self, mut ty: Type, field: u32, span: Span, path: &str) -> Option<Type> {
        for _ in 0..64 {
            let Type::Nominal(type_id, arguments) = ty else {
                self.push(
                    MirValidationCode::InvalidPlace,
                    format!("field projection targets non-record type {ty:?}"),
                    span,
                    path,
                );
                return None;
            };
            let Some(definition) = self.program.type_def(type_id) else {
                self.invalid_type(type_id, span, path);
                return None;
            };
            match &definition.kind {
                TypeDefKind::Record { fields, .. } => {
                    let Some(definition) = fields.get(field as usize) else {
                        self.push(
                            MirValidationCode::InvalidPlace,
                            format!("record field index {field} is out of bounds"),
                            span,
                            path,
                        );
                        return None;
                    };
                    return Some(substitute_type(&definition.ty, &arguments));
                }
                TypeDefKind::Refined { base, .. } => {
                    ty = substitute_type(base, &arguments);
                }
                TypeDefKind::Enum { .. } => {
                    self.push(
                        MirValidationCode::InvalidPlace,
                        "field projection targets an enum value",
                        span,
                        path,
                    );
                    return None;
                }
            }
        }
        self.push(
            MirValidationCode::InvalidPlace,
            "refined projection chain exceeds the validation limit",
            span,
            path,
        );
        None
    }

    #[allow(clippy::too_many_lines)]
    fn validate_witness_ref(
        &mut self,
        function: &Function,
        reference: &WitnessRef,
        expected: Option<&WitnessParam>,
        span: Span,
        path: &str,
        depth: u16,
    ) -> Option<ResolvedWitness> {
        if !self.enter(depth, span, path) {
            return None;
        }
        match reference {
            WitnessRef::Concrete(id) => {
                let Some(witness) = self.program.witness(*id).cloned() else {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        format!("unknown witness #{}", id.0),
                        span,
                        path,
                    );
                    return None;
                };
                if witness.type_parameters != 0 || !witness.prerequisites.is_empty() {
                    self.push(
                        MirValidationCode::WitnessArity,
                        "generic or conditional witness must be instantiated with WitnessRef::Apply",
                        span,
                        path,
                    );
                }
                let proof = WitnessParam {
                    target: witness.concrete,
                    concept: witness.concept,
                    bindings: witness.associated,
                    span,
                };
                self.check_resolved_proof(&proof, expected, span, path);
                Some(ResolvedWitness {
                    definition: Some(*id),
                    proof,
                    projection_witness: None,
                })
            }
            WitnessRef::Parameter(index) => {
                let Some(parameter) = function.witness_params.get(*index as usize).cloned() else {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        format!("unknown witness parameter #{index}"),
                        span,
                        path,
                    );
                    return None;
                };
                self.check_resolved_proof(&parameter, expected, span, path);
                Some(ResolvedWitness {
                    definition: None,
                    proof: parameter,
                    projection_witness: Some(*index),
                })
            }
            WitnessRef::Apply { witness, arguments } => {
                let Some(definition) = self.program.witness(*witness).cloned() else {
                    self.push(
                        MirValidationCode::InvalidWitnessReference,
                        format!("unknown conditional witness #{}", witness.0),
                        span,
                        path,
                    );
                    for (index, argument) in arguments.iter().enumerate() {
                        self.validate_witness_ref(
                            function,
                            argument,
                            None,
                            span,
                            &format!("{path}.arguments[{index}]"),
                            depth + 1,
                        );
                    }
                    return None;
                };
                if arguments.len() != definition.prerequisites.len() {
                    self.push(
                        MirValidationCode::WitnessArity,
                        format!(
                            "conditional witness expects {} proof argument(s), received {}",
                            definition.prerequisites.len(),
                            arguments.len()
                        ),
                        span,
                        format!("{path}.arguments"),
                    );
                }
                let mut substitutions = vec![None; definition.type_parameters as usize];
                if let Some(expected) = expected
                    && expected.concept == definition.concept
                {
                    if !unify_type_parameters(
                        &definition.concrete,
                        &expected.target,
                        &mut substitutions,
                    ) {
                        self.push(
                            MirValidationCode::WitnessShape,
                            "conditional witness head cannot unify with the expected proof target",
                            span,
                            path,
                        );
                    }
                    for (name, schema) in &definition.associated {
                        if let Some(actual) = expected.bindings.get(name)
                            && !unify_type_parameters(schema, actual, &mut substitutions)
                        {
                            self.push(
                                    MirValidationCode::WitnessShape,
                                    format!(
                                        "conditional witness associated type `{name}` cannot unify with the expected binding"
                                    ),
                                    span,
                                    format!("{path}.bindings[{name:?}]"),
                                );
                        }
                    }
                }

                let mut resolved_arguments = Vec::with_capacity(arguments.len());
                for (index, argument) in arguments.iter().enumerate() {
                    let instantiated_expected =
                        definition.prerequisites.get(index).and_then(|schema| {
                            complete_substitutions(&substitutions)
                                .map(|types| substitute_witness_param(schema, &types))
                        });
                    let resolved = self.validate_witness_ref(
                        function,
                        argument,
                        instantiated_expected.as_ref(),
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                    if let (Some(actual), Some(schema)) =
                        (resolved.as_ref(), definition.prerequisites.get(index))
                    {
                        if actual.proof.concept != schema.concept {
                            self.push(
                                MirValidationCode::WitnessShape,
                                format!(
                                    "conditional proof argument belongs to concept #{}, expected #{}",
                                    actual.proof.concept.0, schema.concept.0
                                ),
                                span,
                                format!("{path}.arguments[{index}]"),
                            );
                        }
                        if !unify_type_parameters(
                            &schema.target,
                            &actual.proof.target,
                            &mut substitutions,
                        ) {
                            self.push(
                                MirValidationCode::WitnessShape,
                                "conditional proof target does not match its prerequisite schema",
                                span,
                                format!("{path}.arguments[{index}]"),
                            );
                        }
                        for (name, binding_schema) in &schema.bindings {
                            let compatible =
                                actual.proof.bindings.get(name).is_some_and(|actual| {
                                    unify_type_parameters(
                                        binding_schema,
                                        actual,
                                        &mut substitutions,
                                    )
                                });
                            if !compatible {
                                self.push(
                                    MirValidationCode::WitnessShape,
                                    format!(
                                        "conditional proof does not satisfy associated binding `{name}`"
                                    ),
                                    span,
                                    format!("{path}.arguments[{index}].bindings[{name:?}]"),
                                );
                            }
                        }
                    }
                    resolved_arguments.push(resolved);
                }

                for (index, substitution) in substitutions.iter().enumerate() {
                    if substitution.is_none() {
                        self.push(
                            MirValidationCode::WitnessShape,
                            format!(
                                "conditional witness type parameter #{index} cannot be inferred from its proof tree or expected target"
                            ),
                            span,
                            path,
                        );
                    }
                }
                let substitutions: Vec<_> = substitutions
                    .into_iter()
                    .map(|substitution| substitution.unwrap_or(Type::Error))
                    .collect();
                for (index, (resolved, schema)) in resolved_arguments
                    .iter()
                    .zip(&definition.prerequisites)
                    .enumerate()
                {
                    if let Some(resolved) = resolved {
                        let instantiated = substitute_witness_param(schema, &substitutions);
                        self.check_resolved_proof(
                            &resolved.proof,
                            Some(&instantiated),
                            span,
                            &format!("{path}.arguments[{index}]"),
                        );
                    }
                }
                let proof = WitnessParam {
                    target: substitute_type(&definition.concrete, &substitutions),
                    concept: definition.concept,
                    bindings: definition
                        .associated
                        .iter()
                        .map(|(name, ty)| (name.clone(), substitute_type(ty, &substitutions)))
                        .collect(),
                    span,
                };
                self.check_resolved_proof(&proof, expected, span, path);
                Some(ResolvedWitness {
                    definition: Some(*witness),
                    proof,
                    projection_witness: None,
                })
            }
        }
    }

    fn check_resolved_proof(
        &mut self,
        actual: &WitnessParam,
        expected: Option<&WitnessParam>,
        span: Span,
        path: &str,
    ) {
        let Some(expected) = expected else {
            return;
        };
        if actual.concept != expected.concept
            || !types_compatible(&actual.target, &expected.target)
            || !proof_bindings_satisfy(&actual.bindings, &expected.bindings)
        {
            self.push(
                MirValidationCode::WitnessShape,
                format!(
                    "proof {:?}: concept #{} does not satisfy expected {:?}: concept #{}",
                    actual.target, actual.concept.0, expected.target, expected.concept.0
                ),
                span,
                path,
            );
        }
    }

    fn validate_type(&mut self, ty: &Type, span: Span, path: &str, depth: u16) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_type(
                        element,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Nominal(id, arguments) => {
                if let Some(definition) = self.program.type_def(*id) {
                    if arguments.len() != definition.type_parameters as usize {
                        self.push(
                            MirValidationCode::TypeMismatch,
                            format!(
                                "type `{}` expects {} generic argument(s), found {}",
                                definition.name,
                                definition.type_parameters,
                                arguments.len()
                            ),
                            span,
                            path,
                        );
                    }
                } else {
                    self.invalid_type(*id, span, path);
                }
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_type(
                        argument,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::View { bindings, .. } => {
                for (name, binding) in bindings {
                    self.validate_type(
                        binding,
                        span,
                        &format!("{path}.bindings[{name:?}]"),
                        depth + 1,
                    );
                }
            }
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => {
                self.validate_type(output, span, &format!("{path}.output"), depth + 1);
            }
            Type::Error => self.push(
                MirValidationCode::ErrorType,
                "checked MIR cannot contain the recovery-only Error type",
                span,
                path,
            ),
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. } => {}
        }
    }

    fn reject_frame_projection(&mut self, ty: &Type, span: Span, path: &str, depth: u16) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            Type::AssociatedProjection { .. } => self.push(
                MirValidationCode::TypeMismatch,
                "associated projection is only valid in a function frame",
                span,
                path,
            ),
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.reject_frame_projection(
                        element,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Nominal(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    self.reject_frame_projection(
                        argument,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => {
                self.reject_frame_projection(output, span, &format!("{path}.output"), depth + 1);
            }
            Type::View { bindings, .. } => {
                for (name, binding) in bindings {
                    self.reject_frame_projection(
                        binding,
                        span,
                        &format!("{path}.bindings[{name:?}]"),
                        depth + 1,
                    );
                }
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::Error => {}
        }
    }

    fn validate_view_placement(
        &mut self,
        ty: &Type,
        _allow_top_level: bool,
        span: Span,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, span, path) {
            return;
        }
        match ty {
            Type::View { bindings, .. } => {
                for (name, binding) in bindings {
                    self.validate_view_placement(
                        binding,
                        false,
                        span,
                        &format!("{path}.bindings[{name:?}]"),
                        depth + 1,
                    );
                }
            }
            Type::Nominal(_, arguments) => {
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_view_placement(
                        argument,
                        false,
                        span,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Tuple(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_view_placement(
                        element,
                        false,
                        span,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => {
                self.validate_view_placement(
                    output,
                    false,
                    span,
                    &format!("{path}.output"),
                    depth + 1,
                );
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }

    fn variant(&self, type_id: crate::TypeId, variant_id: VariantId) -> Option<crate::VariantDef> {
        let definition = self.program.type_def(type_id)?;
        let TypeDefKind::Enum { variants } = &definition.kind else {
            return None;
        };
        let variant = variants.get(variant_id.0 as usize)?;
        (variant.id == variant_id).then(|| variant.clone())
    }

    fn nominal_self_type(definition: &TypeDef) -> Type {
        Type::Nominal(
            definition.id,
            (0..definition.type_parameters)
                .map(Type::Parameter)
                .collect(),
        )
    }

    fn local_decl(function: &Function, id: LocalId) -> Option<&LocalDecl> {
        function
            .params
            .iter()
            .chain(&function.locals)
            .find(|local| local.id == id)
    }

    fn expected_result_type(
        &mut self,
        success: Type,
        error_id: Option<crate::TypeId>,
        error_name: &str,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let Some(result_id) = self.program.prelude.result else {
            self.push(
                MirValidationCode::InvalidTypeReference,
                "operation returns Result, but prelude.result is absent",
                span,
                path,
            );
            return None;
        };
        let Some(result) = self.program.type_def(result_id) else {
            self.invalid_type(result_id, span, "prelude.result");
            return None;
        };
        if !matches!(result.kind, TypeDefKind::Enum { .. }) || result.type_parameters != 2 {
            self.push(
                MirValidationCode::VariantShape,
                "prelude.result must be a binary generic enum",
                result.span,
                "prelude.result",
            );
            return None;
        }
        let Some(error_id) = error_id else {
            self.push(
                MirValidationCode::InvalidTypeReference,
                format!("operation requires prelude.{error_name}"),
                span,
                path,
            );
            return None;
        };
        let Some(error) = self.program.type_def(error_id) else {
            self.invalid_type(error_id, span, format!("prelude.{error_name}"));
            return None;
        };
        if error.type_parameters != 0 {
            self.push(
                MirValidationCode::TypeMismatch,
                format!("prelude.{error_name} must be a non-generic type"),
                error.span,
                format!("prelude.{error_name}"),
            );
            return None;
        }
        Some(Type::Nominal(
            result_id,
            vec![success, Type::Nominal(error_id, Vec::new())],
        ))
    }

    fn expected_io_result_task(&mut self, success: Type, span: Span, path: &str) -> Option<Type> {
        self.expected_result_type(
            success,
            self.program.prelude.io_error,
            "io_error",
            span,
            path,
        )
        .map(|result| Type::Task(Box::new(result)))
    }

    fn expected_option_type(&mut self, element: Type, span: Span, path: &str) -> Option<Type> {
        let Some(option_id) = self.program.prelude.option else {
            self.push(
                MirValidationCode::InvalidTypeReference,
                "operation returns Option, but prelude.option is absent",
                span,
                path,
            );
            return None;
        };
        let Some(option) = self.program.type_def(option_id) else {
            self.invalid_type(option_id, span, "prelude.option");
            return None;
        };
        if !matches!(option.kind, TypeDefKind::Enum { .. }) || option.type_parameters != 1 {
            self.push(
                MirValidationCode::VariantShape,
                "prelude.option must be a unary generic enum",
                option.span,
                "prelude.option",
            );
            return None;
        }
        Some(Type::Nominal(option_id, vec![element]))
    }

    fn expected_prelude_nominal(
        &mut self,
        id: Option<crate::TypeId>,
        name: &str,
        span: Span,
        path: &str,
    ) -> Option<Type> {
        let Some(id) = id else {
            self.push(
                MirValidationCode::InvalidTypeReference,
                format!("operation requires prelude.{name}"),
                span,
                path,
            );
            return None;
        };
        let Some(definition) = self.program.type_def(id) else {
            self.invalid_type(id, span, format!("prelude.{name}"));
            return None;
        };
        if definition.type_parameters != 0 {
            self.push(
                MirValidationCode::TypeMismatch,
                format!("prelude.{name} must be a non-generic type"),
                definition.span,
                format!("prelude.{name}"),
            );
            return None;
        }
        Some(Type::Nominal(id, Vec::new()))
    }

    fn task_outcome_type(&self, output: Type) -> Type {
        self.program
            .prelude
            .task_outcome
            .map_or(Type::Error, |id| Type::Nominal(id, vec![output]))
    }

    fn call_target_is_proven_synchronous(&self, target: &CallTarget) -> bool {
        match target {
            CallTarget::Direct(function) | CallTarget::Inherent(function) => self
                .program
                .function(*function)
                .is_some_and(|function| !function.is_async),
            CallTarget::StaticConcept {
                requirement,
                witness,
                ..
            } => {
                if self.program.requirement(*requirement).is_none() {
                    return false;
                }
                let witness = match witness {
                    WitnessRef::Concrete(witness) | WitnessRef::Apply { witness, .. } => {
                        Some(*witness)
                    }
                    // The current requirement schema has no async form. Every
                    // concrete proof supplied for this parameter is separately
                    // required to map to a synchronous witness method.
                    WitnessRef::Parameter(_) => None,
                };
                witness.is_none_or(|witness| {
                    self.program
                        .witness(witness)
                        .and_then(|witness| witness.methods.get(requirement))
                        .and_then(|function| self.program.function(*function))
                        .is_some_and(|function| !function.is_async)
                })
            }
            CallTarget::Dynamic { requirement } => {
                self.program.requirement(*requirement).is_some()
                    && self
                        .program
                        .witnesses
                        .iter()
                        .filter_map(|witness| witness.methods.get(requirement))
                        .all(|function| {
                            self.program
                                .function(*function)
                                .is_some_and(|function| !function.is_async)
                        })
            }
            CallTarget::Builtin(builtin) => Self::builtin_is_proven_synchronous(*builtin),
        }
    }

    const fn builtin_is_proven_synchronous(builtin: Builtin) -> bool {
        match builtin {
            Builtin::FileOpenRead
            | Builtin::FileCreate
            | Builtin::FileOpenReadPath
            | Builtin::FileCreatePath
            | Builtin::FileReadText
            | Builtin::FileWriteText
            | Builtin::SocketConnect
            | Builtin::SocketReadText
            | Builtin::SocketWriteText
            | Builtin::FileTryOpenRead
            | Builtin::FileTryCreate
            | Builtin::FileTryOpenReadPath
            | Builtin::FileTryCreatePath
            | Builtin::FileTryReadText
            | Builtin::FileTryWriteText
            | Builtin::SocketTryConnect
            | Builtin::SocketTryReadText
            | Builtin::SocketTryWriteText => false,
            Builtin::IsFinite
            | Builtin::ParseFloat
            | Builtin::FormatFloat
            | Builtin::TextLength
            | Builtin::TextGet
            | Builtin::TextConcat
            | Builtin::TextContains
            | Builtin::TextEncodeUtf8
            | Builtin::BytesLength
            | Builtin::BytesGet
            | Builtin::BytesAppend
            | Builtin::BytesDecodeUtf8
            | Builtin::PathFromText
            | Builtin::PathAsText
            | Builtin::PathJoin
            | Builtin::ListAdd
            | Builtin::ListLength
            | Builtin::ListGet
            | Builtin::ProcessArguments
            | Builtin::ProcessEnvironment
            | Builtin::ParseInt
            | Builtin::TaskFaultCode
            | Builtin::TaskFaultMessage
            | Builtin::DurationMilliseconds
            | Builtin::DurationAsMilliseconds
            | Builtin::FileClose
            | Builtin::SocketClose
            | Builtin::TextMapNew
            | Builtin::TextMapLength
            | Builtin::TextMapContains
            | Builtin::TextMapGet
            | Builtin::TextMapInsert
            | Builtin::TextMapRemove
            | Builtin::JsonParse
            | Builtin::JsonFormat
            | Builtin::IoErrorKind
            | Builtin::IoErrorMessage
            | Builtin::LogDebug
            | Builtin::LogInfo
            | Builtin::LogWarn
            | Builtin::LogError
            | Builtin::LogWrite => true,
        }
    }

    fn validate_borrowed_view_uses(&mut self, function: &Function, path: &str) {
        self.validate_borrowed_view_block(function, &function.body, &format!("{path}.body"), 0);
    }

    fn validate_borrowed_view_block(
        &mut self,
        function: &Function,
        block: &Block,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, block.span, path) {
            return;
        }
        for (index, statement) in block.statements.iter().enumerate() {
            self.validate_borrowed_view_statement(
                function,
                statement,
                &format!("{path}.statements[{index}]"),
                depth + 1,
            );
        }
        if let Some(tail) = block.tail.as_deref() {
            self.validate_borrowed_view_expr(
                function,
                tail,
                BorrowedViewPosition::Other,
                &format!("{path}.tail"),
                depth + 1,
            );
        }
    }

    fn validate_borrowed_view_statement(
        &mut self,
        function: &Function,
        statement: &Statement,
        path: &str,
        depth: u16,
    ) {
        match &statement.kind {
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. } => self.validate_borrowed_view_expr(
                function,
                value,
                BorrowedViewPosition::Other,
                &format!("{path}.value"),
                depth,
            ),
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                self.validate_borrowed_view_expr(
                    function,
                    start,
                    BorrowedViewPosition::Other,
                    &format!("{path}.start"),
                    depth,
                );
                self.validate_borrowed_view_expr(
                    function,
                    end,
                    BorrowedViewPosition::Other,
                    &format!("{path}.end"),
                    depth,
                );
                self.validate_borrowed_view_block(function, body, &format!("{path}.body"), depth);
            }
            StatementKind::Assert { condition } => self.validate_borrowed_view_expr(
                function,
                condition,
                BorrowedViewPosition::Other,
                &format!("{path}.condition"),
                depth,
            ),
            StatementKind::Defer(cleanup) => self.validate_borrowed_view_block(
                function,
                cleanup,
                &format!("{path}.cleanup"),
                depth,
            ),
            StatementKind::Evaluate(expression) => self.validate_borrowed_view_expr(
                function,
                expression,
                BorrowedViewPosition::Other,
                &format!("{path}.expression"),
                depth,
            ),
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    self.validate_borrowed_view_expr(
                        function,
                        value,
                        BorrowedViewPosition::Other,
                        &format!("{path}.value"),
                        depth,
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn validate_borrowed_view_expr(
        &mut self,
        function: &Function,
        expression: &Expr,
        position: BorrowedViewPosition,
        path: &str,
        depth: u16,
    ) {
        if !self.enter(depth, expression.span, path) {
            return;
        }
        let is_borrowed = matches!(
            &expression.kind,
            ExprKind::MakeView {
                writeback: Some(_),
                ..
            } | ExprKind::ReborrowView { .. }
        );
        if is_borrowed && position != BorrowedViewPosition::DirectCallArgument {
            self.push(
                MirValidationCode::BorrowShape,
                "borrowed interface adaptation must be the direct value expression of a synchronous call argument",
                expression.span,
                path,
            );
        }

        let other = BorrowedViewPosition::Other;
        match &expression.kind {
            ExprKind::Constant(_)
            | ExprKind::Copy(_)
            | ExprKind::Move(_)
            | ExprKind::ReborrowView { .. } => {}
            ExprKind::Tuple(elements) | ExprKind::List(elements) => {
                for (index, element) in elements.iter().enumerate() {
                    self.validate_borrowed_view_expr(
                        function,
                        element,
                        other,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                }
            }
            ExprKind::Unary(_, operand) | ExprKind::Unrefine(operand) => self
                .validate_borrowed_view_expr(
                    function,
                    operand,
                    other,
                    &format!("{path}.operand"),
                    depth + 1,
                ),
            ExprKind::Binary(_, left, right) => {
                self.validate_borrowed_view_expr(
                    function,
                    left,
                    other,
                    &format!("{path}.left"),
                    depth + 1,
                );
                self.validate_borrowed_view_expr(
                    function,
                    right,
                    other,
                    &format!("{path}.right"),
                    depth + 1,
                );
            }
            ExprKind::Block(block) => self.validate_borrowed_view_block(
                function,
                block,
                &format!("{path}.block"),
                depth + 1,
            ),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.validate_borrowed_view_expr(
                    function,
                    condition,
                    other,
                    &format!("{path}.condition"),
                    depth + 1,
                );
                self.validate_borrowed_view_block(
                    function,
                    then_branch,
                    &format!("{path}.then"),
                    depth + 1,
                );
                self.validate_borrowed_view_block(
                    function,
                    else_branch,
                    &format!("{path}.else"),
                    depth + 1,
                );
            }
            ExprKind::Match { scrutinee, arms } => {
                self.validate_borrowed_view_expr(
                    function,
                    scrutinee,
                    other,
                    &format!("{path}.scrutinee"),
                    depth + 1,
                );
                for (index, arm) in arms.iter().enumerate() {
                    self.validate_borrowed_view_expr(
                        function,
                        &arm.value,
                        other,
                        &format!("{path}.arms[{index}].value"),
                        depth + 1,
                    );
                }
            }
            ExprKind::Record { fields, .. } => {
                for (index, field) in fields.iter().enumerate() {
                    self.validate_borrowed_view_expr(
                        function,
                        field,
                        other,
                        &format!("{path}.fields[{index}]"),
                        depth + 1,
                    );
                }
            }
            ExprKind::Variant { payload, .. } => {
                for (index, value) in payload.iter().enumerate() {
                    self.validate_borrowed_view_expr(
                        function,
                        value,
                        other,
                        &format!("{path}.payload[{index}]"),
                        depth + 1,
                    );
                }
            }
            ExprKind::Refine { value, .. } | ExprKind::MakeView { value, .. } => self
                .validate_borrowed_view_expr(
                    function,
                    value,
                    other,
                    &format!("{path}.value"),
                    depth + 1,
                ),
            ExprKind::Call { arguments, .. } => {
                for (index, argument) in arguments.iter().enumerate() {
                    if let CallArgument::Value(value) = argument {
                        self.validate_borrowed_view_expr(
                            function,
                            value,
                            BorrowedViewPosition::DirectCallArgument,
                            &format!("{path}.arguments[{index}]"),
                            depth + 1,
                        );
                    }
                }
            }
            ExprKind::Await { task, .. } => self.validate_borrowed_view_expr(
                function,
                task,
                other,
                &format!("{path}.task"),
                depth + 1,
            ),
            ExprKind::Sleep { milliseconds } => self.validate_borrowed_view_expr(
                function,
                milliseconds,
                other,
                &format!("{path}.milliseconds"),
                depth + 1,
            ),
            ExprKind::WaitFd { descriptor, .. } => self.validate_borrowed_view_expr(
                function,
                descriptor,
                other,
                &format!("{path}.descriptor"),
                depth + 1,
            ),
            ExprKind::TaskJoin { arguments, .. } => {
                for (index, argument) in arguments.iter().enumerate() {
                    self.validate_borrowed_view_expr(
                        function,
                        argument,
                        other,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                }
            }
        }
    }

    fn validate_function_dataflow(&mut self, function: &Function, path: &str) {
        self.validate_borrowed_view_uses(function, path);
        if self.nesting_failed {
            return;
        }
        let local_count = function.params.len() + function.locals.len();
        let mut state = DataflowState {
            slots: vec![SlotState::Uninitialized; local_count],
            view_loans: vec![Vec::new(); local_count],
            temporary_loans: Vec::new(),
        };
        for parameter in &function.params {
            if let Some(slot) = state.slots.get_mut(parameter.id.0 as usize) {
                *slot = SlotState::Available;
            }
        }
        let mut tokens = BTreeSet::new();
        let flow = self.dataflow_block(
            function,
            &function.body,
            &mut state,
            &mut tokens,
            &format!("{path}.body"),
            0,
        );
        if !flow.loans.is_empty() {
            self.push(
                MirValidationCode::BorrowShape,
                "call-scoped interface carrier cannot escape through a function result",
                function.body.span,
                format!("{path}.body.tail"),
            );
        }
    }

    fn dataflow_block(
        &mut self,
        function: &Function,
        block: &Block,
        state: &mut DataflowState,
        tokens: &mut BTreeSet<u32>,
        path: &str,
        depth: u16,
    ) -> ExprFlow {
        if !self.enter(depth, block.span, path) {
            return ExprFlow {
                diverges: true,
                loans: Vec::new(),
            };
        }
        for (index, statement) in block.statements.iter().enumerate() {
            if self.dataflow_statement(
                function,
                statement,
                state,
                tokens,
                &format!("{path}.statements[{index}]"),
                depth + 1,
            ) {
                return ExprFlow {
                    diverges: true,
                    loans: Vec::new(),
                };
            }
        }
        block.tail.as_deref().map_or(
            ExprFlow {
                diverges: false,
                loans: Vec::new(),
            },
            |tail| {
                self.dataflow_expr(
                    function,
                    tail,
                    state,
                    tokens,
                    &format!("{path}.tail"),
                    depth + 1,
                )
            },
        )
    }

    #[allow(clippy::too_many_lines)]
    fn dataflow_statement(
        &mut self,
        function: &Function,
        statement: &Statement,
        state: &mut DataflowState,
        tokens: &mut BTreeSet<u32>,
        path: &str,
        depth: u16,
    ) -> bool {
        match &statement.kind {
            StatementKind::Let { local, value } => {
                let value = self.dataflow_expr(
                    function,
                    value,
                    state,
                    tokens,
                    &format!("{path}.value"),
                    depth,
                );
                if value.diverges {
                    return true;
                }
                if !value.loans.is_empty() {
                    self.push(
                        MirValidationCode::BorrowShape,
                        "call-scoped interface carrier cannot be stored in a local",
                        statement.span,
                        format!("{path}.value"),
                    );
                }
                let index = local.0 as usize;
                if let Some(slot) = state.slots.get(index)
                    && *slot != SlotState::Uninitialized
                {
                    self.push(
                        MirValidationCode::LocalState,
                        "Let must initialize an uninitialized local exactly once",
                        statement.span,
                        format!("{path}.local"),
                    );
                }
                Self::dataflow_store(index, value.loans, state);
                false
            }
            StatementKind::LetTuple { locals, value } => {
                let value = self.dataflow_expr(
                    function,
                    value,
                    state,
                    tokens,
                    &format!("{path}.value"),
                    depth,
                );
                if value.diverges {
                    return true;
                }
                if !value.loans.is_empty() {
                    self.push(
                        MirValidationCode::BorrowShape,
                        "call-scoped interface carrier cannot be destructured into locals",
                        statement.span,
                        format!("{path}.value"),
                    );
                }
                for (index, local) in locals.iter().enumerate() {
                    let slot = local.0 as usize;
                    if state
                        .slots
                        .get(slot)
                        .is_some_and(|slot| *slot != SlotState::Uninitialized)
                    {
                        self.push(
                            MirValidationCode::LocalState,
                            "LetTuple must initialize each local exactly once",
                            statement.span,
                            format!("{path}.locals[{index}]"),
                        );
                    }
                    Self::dataflow_store(slot, Vec::new(), state);
                }
                false
            }
            StatementKind::ForRange {
                local,
                start,
                end,
                body,
            } => {
                let start = self.dataflow_expr(
                    function,
                    start,
                    state,
                    tokens,
                    &format!("{path}.start"),
                    depth,
                );
                if start.diverges {
                    return true;
                }
                let end =
                    self.dataflow_expr(function, end, state, tokens, &format!("{path}.end"), depth);
                if end.diverges {
                    return true;
                }

                // The loop may execute zero times. Model one iteration and
                // join it with the entry state; source scoping prevents use of
                // the potentially uninitialized iteration binding afterward.
                let entry = state.clone();
                let mut iteration = state.clone();
                Self::dataflow_store(local.0 as usize, Vec::new(), &mut iteration);
                let _ = self.dataflow_block(
                    function,
                    body,
                    &mut iteration,
                    tokens,
                    &format!("{path}.body"),
                    depth + 1,
                );
                *state = join_dataflow_states(&[entry, iteration]);
                false
            }
            StatementKind::Assign { place, value } => {
                let value = self.dataflow_expr(
                    function,
                    value,
                    state,
                    tokens,
                    &format!("{path}.value"),
                    depth,
                );
                if value.diverges {
                    return true;
                }
                if !value.loans.is_empty() {
                    self.push(
                        MirValidationCode::BorrowShape,
                        "call-scoped interface carrier cannot be stored by assignment",
                        statement.span,
                        format!("{path}.value"),
                    );
                }
                let index = place.local.0 as usize;
                if place.projection.is_empty() {
                    self.reject_owner_mutation_while_borrowed(place, state, statement.span, path);
                    Self::dataflow_store(index, value.loans, state);
                } else {
                    self.require_available(place.local, state, statement.span, path);
                    self.reject_owner_mutation_while_borrowed(place, state, statement.span, path);
                }
                false
            }
            StatementKind::Assert { condition } => {
                self.dataflow_expr(
                    function,
                    condition,
                    state,
                    tokens,
                    &format!("{path}.condition"),
                    depth,
                )
                .diverges
            }
            StatementKind::Defer(cleanup) => {
                let mut cleanup_state = state.clone();
                let _ = self.dataflow_block(
                    function,
                    cleanup,
                    &mut cleanup_state,
                    tokens,
                    &format!("{path}.cleanup"),
                    depth + 1,
                );
                false
            }
            StatementKind::Evaluate(expression) => {
                self.dataflow_expr(
                    function,
                    expression,
                    state,
                    tokens,
                    &format!("{path}.expression"),
                    depth,
                )
                .diverges
            }
            StatementKind::Return(value) => {
                if let Some(value) = value {
                    let flow = self.dataflow_expr(
                        function,
                        value,
                        state,
                        tokens,
                        &format!("{path}.value"),
                        depth,
                    );
                    if !flow.loans.is_empty() {
                        self.push(
                            MirValidationCode::BorrowShape,
                            "call-scoped interface carrier cannot escape through return",
                            statement.span,
                            format!("{path}.value"),
                        );
                    }
                }
                true
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn dataflow_expr(
        &mut self,
        function: &Function,
        expression: &Expr,
        state: &mut DataflowState,
        tokens: &mut BTreeSet<u32>,
        path: &str,
        depth: u16,
    ) -> ExprFlow {
        if !self.enter(depth, expression.span, path) {
            return ExprFlow {
                diverges: true,
                loans: Vec::new(),
            };
        }
        let no_value = |diverges| ExprFlow {
            diverges,
            loans: Vec::new(),
        };
        match &expression.kind {
            ExprKind::Constant(_) => no_value(expression.ty == Type::Never),
            ExprKind::Tuple(elements) | ExprKind::List(elements) => {
                let mut loans = Vec::new();
                for (index, element) in elements.iter().enumerate() {
                    let flow = self.dataflow_expr(
                        function,
                        element,
                        state,
                        tokens,
                        &format!("{path}.elements[{index}]"),
                        depth + 1,
                    );
                    if flow.diverges {
                        return no_value(true);
                    }
                    loans = union_loans(loans, flow.loans);
                }
                ExprFlow {
                    diverges: expression.ty == Type::Never,
                    loans,
                }
            }
            ExprKind::Copy(place) => {
                self.require_available(place.local, state, expression.span, path);
                if Self::owner_has_mutable_loan(place, state) {
                    self.push(
                        MirValidationCode::BorrowShape,
                        "place cannot be read while mutable call-scoped access is active",
                        expression.span,
                        path,
                    );
                }
                ExprFlow {
                    diverges: expression.ty == Type::Never,
                    loans: Vec::new(),
                }
            }
            ExprKind::Move(place) => {
                self.require_available(place.local, state, expression.span, path);
                self.reject_owner_mutation_while_borrowed(place, state, expression.span, path);
                let index = place.local.0 as usize;
                let loans = state
                    .view_loans
                    .get_mut(index)
                    .map(std::mem::take)
                    .unwrap_or_default();
                if let Some(slot) = state.slots.get_mut(index) {
                    *slot = SlotState::Moved;
                }
                ExprFlow {
                    diverges: expression.ty == Type::Never,
                    loans,
                }
            }
            ExprKind::Unary(_, operand) | ExprKind::Unrefine(operand) => {
                let flow = self.dataflow_expr(
                    function,
                    operand,
                    state,
                    tokens,
                    &format!("{path}.operand"),
                    depth + 1,
                );
                no_value(flow.diverges || expression.ty == Type::Never)
            }
            ExprKind::Binary(_, left, right) => {
                let left = self.dataflow_expr(
                    function,
                    left,
                    state,
                    tokens,
                    &format!("{path}.left"),
                    depth + 1,
                );
                if left.diverges {
                    return no_value(true);
                }
                let right = self.dataflow_expr(
                    function,
                    right,
                    state,
                    tokens,
                    &format!("{path}.right"),
                    depth + 1,
                );
                no_value(right.diverges || expression.ty == Type::Never)
            }
            ExprKind::Block(block) => self.dataflow_block(
                function,
                block,
                state,
                tokens,
                &format!("{path}.block"),
                depth + 1,
            ),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition = self.dataflow_expr(
                    function,
                    condition,
                    state,
                    tokens,
                    &format!("{path}.condition"),
                    depth + 1,
                );
                if condition.diverges {
                    return no_value(true);
                }
                let mut then_state = state.clone();
                let mut else_state = state.clone();
                let then_flow = self.dataflow_block(
                    function,
                    then_branch,
                    &mut then_state,
                    tokens,
                    &format!("{path}.then"),
                    depth + 1,
                );
                let else_flow = self.dataflow_block(
                    function,
                    else_branch,
                    &mut else_state,
                    tokens,
                    &format!("{path}.else"),
                    depth + 1,
                );
                let continuing = [
                    (!then_flow.diverges).then_some(then_state),
                    (!else_flow.diverges).then_some(else_state),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
                if !continuing.is_empty() {
                    *state = join_dataflow_states(&continuing);
                }
                ExprFlow {
                    diverges: then_flow.diverges && else_flow.diverges,
                    loans: union_loans(then_flow.loans, else_flow.loans),
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                let scrutinee = self.dataflow_expr(
                    function,
                    scrutinee,
                    state,
                    tokens,
                    &format!("{path}.scrutinee"),
                    depth + 1,
                );
                if scrutinee.diverges {
                    return no_value(true);
                }
                let mut continuing = Vec::new();
                let mut result_loans = Vec::new();
                let mut all_diverge = !arms.is_empty();
                for (index, arm) in arms.iter().enumerate() {
                    let mut arm_state = state.clone();
                    for local in &arm.bindings {
                        Self::dataflow_store(local.0 as usize, Vec::new(), &mut arm_state);
                    }
                    let flow = self.dataflow_expr(
                        function,
                        &arm.value,
                        &mut arm_state,
                        tokens,
                        &format!("{path}.arms[{index}].value"),
                        depth + 1,
                    );
                    all_diverge &= flow.diverges;
                    result_loans = union_loans(result_loans, flow.loans);
                    if !flow.diverges {
                        continuing.push(arm_state);
                    }
                }
                if !continuing.is_empty() {
                    *state = join_dataflow_states(&continuing);
                }
                ExprFlow {
                    diverges: all_diverge,
                    loans: result_loans,
                }
            }
            ExprKind::Record { fields, .. } => {
                let mut loans = Vec::new();
                for (index, field) in fields.iter().enumerate() {
                    let flow = self.dataflow_expr(
                        function,
                        field,
                        state,
                        tokens,
                        &format!("{path}.fields[{index}]"),
                        depth + 1,
                    );
                    if flow.diverges {
                        return no_value(true);
                    }
                    loans = union_loans(loans, flow.loans);
                }
                ExprFlow {
                    diverges: expression.ty == Type::Never,
                    loans,
                }
            }
            ExprKind::Variant { payload, .. } => {
                let mut loans = Vec::new();
                for (index, value) in payload.iter().enumerate() {
                    let flow = self.dataflow_expr(
                        function,
                        value,
                        state,
                        tokens,
                        &format!("{path}.payload[{index}]"),
                        depth + 1,
                    );
                    if flow.diverges {
                        return no_value(true);
                    }
                    loans = union_loans(loans, flow.loans);
                }
                ExprFlow {
                    diverges: expression.ty == Type::Never,
                    loans,
                }
            }
            ExprKind::Refine { value, .. } => {
                let flow = self.dataflow_expr(
                    function,
                    value,
                    state,
                    tokens,
                    &format!("{path}.value"),
                    depth + 1,
                );
                ExprFlow {
                    diverges: flow.diverges || expression.ty == Type::Never,
                    loans: flow.loans,
                }
            }
            ExprKind::Call {
                target, arguments, ..
            } => {
                let target_is_synchronous = self.call_target_is_proven_synchronous(target);
                let checkpoint = state.temporary_loans.len();
                for (index, argument) in arguments.iter().enumerate() {
                    match argument {
                        CallArgument::Value(value) => {
                            let flow = self.dataflow_expr(
                                function,
                                value,
                                state,
                                tokens,
                                &format!("{path}.arguments[{index}]"),
                                depth + 1,
                            );
                            if flow.diverges {
                                state.temporary_loans.truncate(checkpoint);
                                return no_value(true);
                            }
                            if !target_is_synchronous && !flow.loans.is_empty() {
                                self.push(
                                    MirValidationCode::BorrowShape,
                                    "call-scoped interface carrier cannot enter a call whose target is not proven synchronous",
                                    value.span,
                                    format!("{path}.arguments[{index}]"),
                                );
                            }
                            state.temporary_loans.extend(flow.loans);
                        }
                        CallArgument::InOut(place) => {
                            self.require_available(place.local, state, expression.span, path);
                            if !target_is_synchronous {
                                self.push(
                                    MirValidationCode::BorrowShape,
                                    "an inout place cannot enter a call whose target is not proven synchronous",
                                    expression.span,
                                    format!("{path}.arguments[{index}]"),
                                );
                            }
                            self.reject_owner_mutation_while_borrowed(
                                place,
                                state,
                                expression.span,
                                &format!("{path}.arguments[{index}]"),
                            );
                            state.temporary_loans.push(PlaceLoan {
                                owner: place.clone(),
                                // InOut uses a two-phase access. Later arguments may take a
                                // readonly snapshot (for example `values.add(values.length())`),
                                // but Move, write, mutable borrow, and another overlapping InOut
                                // remain forbidden until the call begins.
                                mutable: false,
                            });
                        }
                    }
                }
                state.temporary_loans.truncate(checkpoint);
                no_value(expression.ty == Type::Never)
            }
            ExprKind::MakeView {
                value,
                writeback,
                mutable,
                token,
                ..
            } => {
                let flow = self.dataflow_expr(
                    function,
                    value,
                    state,
                    tokens,
                    &format!("{path}.value"),
                    depth + 1,
                );
                if flow.diverges {
                    return no_value(true);
                }
                let Some(owner) = writeback else {
                    if !tokens.insert(*token) {
                        self.push(
                            MirValidationCode::BorrowShape,
                            format!("interface value token #{token} is reused in one function"),
                            expression.span,
                            format!("{path}.token"),
                        );
                    }
                    return no_value(expression.ty == Type::Never);
                };
                self.dataflow_view_borrow(owner, *mutable, *token, expression, state, tokens, path)
            }
            ExprKind::ReborrowView {
                owner,
                mutable,
                token,
            } => {
                self.dataflow_view_borrow(owner, *mutable, *token, expression, state, tokens, path)
            }
            ExprKind::Await { task, .. } => {
                let flow = self.dataflow_expr(
                    function,
                    task,
                    state,
                    tokens,
                    &format!("{path}.task"),
                    depth + 1,
                );
                if state.view_loans.iter().any(|loans| !loans.is_empty())
                    || !state.temporary_loans.is_empty()
                {
                    self.push(
                        MirValidationCode::BorrowShape,
                        "call-scoped access cannot remain active across Await",
                        expression.span,
                        path,
                    );
                }
                no_value(flow.diverges || expression.ty == Type::Never)
            }
            ExprKind::Sleep { milliseconds } => {
                let flow = self.dataflow_expr(
                    function,
                    milliseconds,
                    state,
                    tokens,
                    &format!("{path}.milliseconds"),
                    depth + 1,
                );
                no_value(flow.diverges || expression.ty == Type::Never)
            }
            ExprKind::WaitFd { descriptor, .. } => {
                let flow = self.dataflow_expr(
                    function,
                    descriptor,
                    state,
                    tokens,
                    &format!("{path}.descriptor"),
                    depth + 1,
                );
                no_value(flow.diverges || expression.ty == Type::Never)
            }
            ExprKind::TaskJoin { arguments, .. } => {
                for (index, argument) in arguments.iter().enumerate() {
                    let flow = self.dataflow_expr(
                        function,
                        argument,
                        state,
                        tokens,
                        &format!("{path}.arguments[{index}]"),
                        depth + 1,
                    );
                    if flow.diverges {
                        return no_value(true);
                    }
                }
                no_value(expression.ty == Type::Never)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dataflow_view_borrow(
        &mut self,
        owner: &Place,
        mutable: bool,
        token: u32,
        expression: &Expr,
        state: &mut DataflowState,
        tokens: &mut BTreeSet<u32>,
        path: &str,
    ) -> ExprFlow {
        self.require_available(owner.local, state, expression.span, path);
        let conflicts = Self::active_loans(owner, state).any(|loan| mutable || loan.mutable);
        if conflicts {
            self.push(
                MirValidationCode::BorrowShape,
                "interface argument access conflicts with an already-active interface access",
                expression.span,
                path,
            );
        }
        if !tokens.insert(token) {
            self.push(
                MirValidationCode::BorrowShape,
                format!("interface access token #{token} is reused in one function"),
                expression.span,
                format!("{path}.token"),
            );
        }
        ExprFlow {
            diverges: expression.ty == Type::Never,
            loans: vec![PlaceLoan {
                owner: owner.clone(),
                mutable,
            }],
        }
    }

    fn dataflow_store(index: usize, loans: Vec<PlaceLoan>, state: &mut DataflowState) {
        if let Some(slot) = state.slots.get_mut(index) {
            *slot = SlotState::Available;
        }
        if let Some(carried) = state.view_loans.get_mut(index) {
            *carried = loans;
        }
    }

    fn require_available(&mut self, local: LocalId, state: &DataflowState, span: Span, path: &str) {
        if state.slots.get(local.0 as usize) != Some(&SlotState::Available) {
            self.push(
                MirValidationCode::LocalState,
                format!(
                    "local #{} is uninitialized, moved, or branch-dependent",
                    local.0
                ),
                span,
                path,
            );
        }
    }

    fn active_loans<'state>(
        owner: &'state Place,
        state: &'state DataflowState,
    ) -> impl Iterator<Item = &'state PlaceLoan> + 'state {
        state
            .view_loans
            .iter()
            .flat_map(|loans| loans.iter())
            .chain(state.temporary_loans.iter())
            .filter(move |loan| places_overlap(&loan.owner, owner))
    }

    fn owner_has_mutable_loan(owner: &Place, state: &DataflowState) -> bool {
        Self::active_loans(owner, state).any(|loan| loan.mutable)
    }

    fn reject_owner_mutation_while_borrowed(
        &mut self,
        owner: &Place,
        state: &DataflowState,
        span: Span,
        path: &str,
    ) {
        if Self::active_loans(owner, state).next().is_some() {
            self.push(
                MirValidationCode::BorrowShape,
                "place cannot be moved or mutated while call-scoped access is active",
                span,
                path,
            );
        }
    }

    fn is_float_like(&self, ty: &Type) -> bool {
        let mut current = ty;
        for _ in 0..64 {
            match current {
                Type::Float => return true,
                Type::Nominal(id, _) => {
                    let Some(TypeDef {
                        kind: TypeDefKind::Refined { base, .. },
                        ..
                    }) = self.program.type_def(*id)
                    else {
                        return false;
                    };
                    current = base;
                }
                _ => return false,
            }
        }
        false
    }

    fn enter(&mut self, depth: u16, span: Span, path: &str) -> bool {
        if depth <= MAX_VALIDATION_DEPTH {
            true
        } else {
            self.nesting_failed = true;
            self.push(
                MirValidationCode::NestingLimit,
                format!("MIR nesting exceeds {MAX_VALIDATION_DEPTH}"),
                span,
                path,
            );
            false
        }
    }

    fn invalid_type(&mut self, id: crate::TypeId, span: Span, path: impl Into<String>) {
        self.push(
            MirValidationCode::InvalidTypeReference,
            format!("unknown type #{}", id.0),
            span,
            path,
        );
    }

    fn invalid_function(&mut self, id: FunctionId, span: Span, path: impl Into<String>) {
        self.push(
            MirValidationCode::InvalidFunctionReference,
            format!("unknown function #{}", id.0),
            span,
            path,
        );
    }

    fn invalid_concept(&mut self, id: ConceptId, span: Span, path: impl Into<String>) {
        self.push(
            MirValidationCode::InvalidConceptReference,
            format!("unknown concept #{}", id.0),
            span,
            path,
        );
    }

    fn invalid_requirement(&mut self, id: RequirementId, span: Span, path: impl Into<String>) {
        self.push(
            MirValidationCode::InvalidRequirementReference,
            format!("unknown requirement #{}", id.0),
            span,
            path,
        );
    }

    fn invalid_variant(
        &mut self,
        ty: crate::TypeId,
        variant: VariantId,
        span: Span,
        path: impl Into<String>,
    ) {
        self.push(
            MirValidationCode::InvalidVariantReference,
            format!("unknown variant #{} for type #{}", variant.0, ty.0),
            span,
            path,
        );
    }

    fn invalid_local(&mut self, id: LocalId, span: Span, path: impl Into<String>) {
        self.push(
            MirValidationCode::InvalidLocalReference,
            format!("unknown local #{}", id.0),
            span,
            path,
        );
    }

    fn unused_call_witnesses(&mut self, span: Span, path: &str) {
        self.push(
            MirValidationCode::WitnessArity,
            "this call target obtains witnesses from its target and must not carry witness arguments",
            span,
            format!("{path}.witnesses"),
        );
    }

    fn unused_call_type_arguments(&mut self, span: Span, path: &str) {
        self.push(
            MirValidationCode::CallArity,
            "dynamic and builtin calls cannot carry static type arguments",
            span,
            format!("{path}.type_arguments"),
        );
    }

    fn type_mismatch(&mut self, expected: &Type, actual: &Type, span: Span, path: &str) {
        self.push(
            MirValidationCode::TypeMismatch,
            format!("expected {expected:?}, found {actual:?}"),
            span,
            path,
        );
    }

    fn push(
        &mut self,
        code: MirValidationCode,
        message: impl Into<String>,
        span: Span,
        path: impl Into<String>,
    ) {
        self.errors.push(MirValidationError {
            code,
            message: message.into(),
            span,
            path: path.into(),
        });
    }
}

fn constant_type(constant: &Constant) -> Type {
    match constant {
        Constant::Unit => Type::Unit,
        Constant::Bool(_) => Type::Bool,
        Constant::Int(_) => Type::Int,
        Constant::Float(_) => Type::Float,
        Constant::Text(_) => Type::Text,
    }
}

fn witness_params_equal(left: &WitnessParam, right: &WitnessParam) -> bool {
    left.target == right.target && left.concept == right.concept && left.bindings == right.bindings
}

fn substitute_witness_param(parameter: &WitnessParam, arguments: &[Type]) -> WitnessParam {
    WitnessParam {
        target: substitute_type(&parameter.target, arguments),
        concept: parameter.concept,
        bindings: parameter
            .bindings
            .iter()
            .map(|(name, ty)| (name.clone(), substitute_type(ty, arguments)))
            .collect(),
        span: parameter.span,
    }
}

fn complete_substitutions(substitutions: &[Option<Type>]) -> Option<Vec<Type>> {
    substitutions.iter().cloned().collect()
}

fn collect_type_parameters(ty: &Type, parameters: &mut BTreeSet<u32>) {
    let mut pending = vec![ty];
    while let Some(current) = pending.pop() {
        match current {
            Type::Parameter(index) => {
                parameters.insert(*index);
            }
            Type::Tuple(elements) => pending.extend(elements),
            Type::Nominal(_, arguments) => pending.extend(arguments),
            Type::View { bindings, .. } => pending.extend(bindings.values()),
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => {
                pending.push(output);
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }
}

fn type_contains_view(ty: &Type) -> bool {
    let mut pending = vec![ty];
    while let Some(current) = pending.pop() {
        match current {
            Type::View { .. } => return true,
            Type::Tuple(elements) => pending.extend(elements),
            Type::Nominal(_, arguments) => pending.extend(arguments),
            Type::Task(output) | Type::List(output) | Type::TaskOutcome(output) => {
                pending.push(output);
            }
            Type::Never
            | Type::Unit
            | Type::Bool
            | Type::Int
            | Type::Float
            | Type::Text
            | Type::Parameter(_)
            | Type::AssociatedProjection { .. }
            | Type::Error => {}
        }
    }
    false
}

fn union_loans(mut left: Vec<PlaceLoan>, right: Vec<PlaceLoan>) -> Vec<PlaceLoan> {
    for loan in right {
        if !left.contains(&loan) {
            left.push(loan);
        }
    }
    left
}

fn places_overlap(left: &Place, right: &Place) -> bool {
    left.local == right.local
        && (left.projection.starts_with(&right.projection)
            || right.projection.starts_with(&left.projection))
}

fn join_dataflow_states(states: &[DataflowState]) -> DataflowState {
    let mut joined = states[0].clone();
    for state in &states[1..] {
        for (slot, other) in joined.slots.iter_mut().zip(&state.slots) {
            if *slot != *other {
                *slot = SlotState::MaybeUnavailable;
            }
        }
        for (loans, other) in joined.view_loans.iter_mut().zip(&state.view_loans) {
            *loans = union_loans(std::mem::take(loans), other.clone());
        }
        joined.temporary_loans = union_loans(
            std::mem::take(&mut joined.temporary_loans),
            state.temporary_loans.clone(),
        );
    }
    joined
}

fn block_definitely_diverges(block: &Block, depth: u16) -> bool {
    if depth > MAX_VALIDATION_DEPTH {
        return true;
    }
    for statement in &block.statements {
        let diverges = match &statement.kind {
            StatementKind::Return(_) => true,
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expr_definitely_diverges(value, depth + 1),
            StatementKind::ForRange { start, end, .. } => {
                expr_definitely_diverges(start, depth + 1)
                    || expr_definitely_diverges(end, depth + 1)
            }
            StatementKind::Assert { condition } => expr_definitely_diverges(condition, depth + 1),
            StatementKind::Defer(_) => false,
        };
        if diverges {
            return true;
        }
    }
    block
        .tail
        .as_deref()
        .is_some_and(|tail| expr_definitely_diverges(tail, depth + 1))
}

fn cleanup_contains_forbidden_control(block: &Block, depth: u16) -> bool {
    if depth > MAX_VALIDATION_DEPTH {
        return true;
    }
    block
        .statements
        .iter()
        .any(|statement| match &statement.kind {
            StatementKind::Return(_) | StatementKind::Defer(_) => true,
            StatementKind::Let { value, .. }
            | StatementKind::LetTuple { value, .. }
            | StatementKind::Assign { value, .. }
            | StatementKind::Evaluate(value) => expr_contains_await(value, depth + 1),
            StatementKind::ForRange {
                start, end, body, ..
            } => {
                expr_contains_await(start, depth + 1)
                    || expr_contains_await(end, depth + 1)
                    || cleanup_contains_forbidden_control(body, depth + 1)
            }
            StatementKind::Assert { condition } => expr_contains_await(condition, depth + 1),
        })
        || block
            .tail
            .as_deref()
            .is_some_and(|tail| expr_contains_await(tail, depth + 1))
}

fn expr_contains_await(expression: &Expr, depth: u16) -> bool {
    if depth > MAX_VALIDATION_DEPTH {
        return true;
    }
    match &expression.kind {
        ExprKind::Await { .. } => true,
        ExprKind::Tuple(elements) | ExprKind::List(elements) => elements
            .iter()
            .any(|element| expr_contains_await(element, depth + 1)),
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::MakeView { value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        }
        | ExprKind::WaitFd {
            descriptor: value, ..
        } => {
            expr_contains_await(value, depth + 1)
        }
        ExprKind::Binary(_, left, right) => {
            expr_contains_await(left, depth + 1) || expr_contains_await(right, depth + 1)
        }
        ExprKind::Block(block) => cleanup_contains_forbidden_control(block, depth + 1),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_contains_await(condition, depth + 1)
                || cleanup_contains_forbidden_control(then_branch, depth + 1)
                || cleanup_contains_forbidden_control(else_branch, depth + 1)
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_contains_await(scrutinee, depth + 1)
                || arms
                    .iter()
                    .any(|arm| expr_contains_await(&arm.value, depth + 1))
        }
        ExprKind::Record { fields, .. } => fields
            .iter()
            .any(|field| expr_contains_await(field, depth + 1)),
        ExprKind::Variant { payload, .. } => payload
            .iter()
            .any(|value| expr_contains_await(value, depth + 1)),
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| {
            matches!(argument, CallArgument::Value(value) if expr_contains_await(value, depth + 1))
        }),
        ExprKind::TaskJoin { arguments, .. } => arguments
            .iter()
            .any(|argument| expr_contains_await(argument, depth + 1)),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

fn expr_definitely_diverges(expression: &Expr, depth: u16) -> bool {
    if depth > MAX_VALIDATION_DEPTH || expression.ty == Type::Never {
        return true;
    }
    match &expression.kind {
        ExprKind::Tuple(elements) | ExprKind::List(elements) => elements
            .iter()
            .any(|element| expr_definitely_diverges(element, depth + 1)),
        ExprKind::Unary(_, value)
        | ExprKind::Unrefine(value)
        | ExprKind::Refine { value, .. }
        | ExprKind::MakeView { value, .. }
        | ExprKind::Await { task: value, .. }
        | ExprKind::Sleep {
            milliseconds: value,
        }
        | ExprKind::WaitFd {
            descriptor: value, ..
        } => {
            expr_definitely_diverges(value, depth + 1)
        }
        ExprKind::Binary(_, left, right) => {
            expr_definitely_diverges(left, depth + 1)
                || expr_definitely_diverges(right, depth + 1)
        }
        ExprKind::Block(block) => block_definitely_diverges(block, depth + 1),
        ExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            expr_definitely_diverges(condition, depth + 1)
                || (block_definitely_diverges(then_branch, depth + 1)
                    && block_definitely_diverges(else_branch, depth + 1))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_definitely_diverges(scrutinee, depth + 1)
                || (!arms.is_empty()
                    && arms
                        .iter()
                        .all(|arm| expr_definitely_diverges(&arm.value, depth + 1)))
        }
        ExprKind::Record { fields, .. } => fields
            .iter()
            .any(|field| expr_definitely_diverges(field, depth + 1)),
        ExprKind::Variant { payload, .. } => payload
            .iter()
            .any(|value| expr_definitely_diverges(value, depth + 1)),
        ExprKind::Call { arguments, .. } => arguments.iter().any(|argument| {
            matches!(argument, CallArgument::Value(value) if expr_definitely_diverges(value, depth + 1))
        }),
        ExprKind::TaskJoin { arguments, .. } => arguments
            .iter()
            .any(|argument| expr_definitely_diverges(argument, depth + 1)),
        ExprKind::Constant(_)
        | ExprKind::Copy(_)
        | ExprKind::Move(_)
        | ExprKind::ReborrowView { .. } => false,
    }
}

/// Unifies a witness-definition type schema with a call-site proof type while
/// collecting substitutions for the definition's alpha-normalized parameters.
/// This is deliberately nominal; it does not inherit the validator's broad
/// `Type::Parameter` compatibility shortcut.
fn unify_type_parameters(schema: &Type, actual: &Type, substitutions: &mut [Option<Type>]) -> bool {
    match schema {
        Type::Parameter(index) => {
            let Some(slot) = substitutions.get_mut(*index as usize) else {
                return false;
            };
            if let Some(previous) = slot {
                previous == actual
            } else {
                *slot = Some(actual.clone());
                true
            }
        }
        Type::Tuple(schema_elements) => {
            let Type::Tuple(actual_elements) = actual else {
                return false;
            };
            schema_elements.len() == actual_elements.len()
                && schema_elements
                    .iter()
                    .zip(actual_elements)
                    .all(|(schema, actual)| unify_type_parameters(schema, actual, substitutions))
        }
        Type::Task(schema_output) => {
            let Type::Task(actual_output) = actual else {
                return false;
            };
            unify_type_parameters(schema_output, actual_output, substitutions)
        }
        Type::Nominal(schema_id, schema_arguments) => {
            let Type::Nominal(actual_id, actual_arguments) = actual else {
                return false;
            };
            schema_id == actual_id
                && schema_arguments.len() == actual_arguments.len()
                && schema_arguments
                    .iter()
                    .zip(actual_arguments)
                    .all(|(schema, actual)| unify_type_parameters(schema, actual, substitutions))
        }
        Type::View {
            mutable: schema_mutable,
            concept: schema_concept,
            bindings: schema_bindings,
        } => {
            let Type::View {
                mutable: actual_mutable,
                concept: actual_concept,
                bindings: actual_bindings,
            } = actual
            else {
                return false;
            };
            schema_mutable == actual_mutable
                && schema_concept == actual_concept
                && schema_bindings.len() == actual_bindings.len()
                && schema_bindings.iter().all(|(name, schema)| {
                    actual_bindings
                        .get(name)
                        .is_some_and(|actual| unify_type_parameters(schema, actual, substitutions))
                })
        }
        _ => schema == actual,
    }
}

fn requirement_type_contains_self(ty: &RequirementType) -> bool {
    match ty {
        RequirementType::SelfType => true,
        RequirementType::Tuple(elements) => elements.iter().any(requirement_type_contains_self),
        RequirementType::Nominal(_, arguments) => {
            arguments.iter().any(requirement_type_contains_self)
        }
        RequirementType::View { bindings, .. } => {
            bindings.values().any(requirement_type_contains_self)
        }
        RequirementType::Unit
        | RequirementType::Bool
        | RequirementType::Int
        | RequirementType::Float
        | RequirementType::Text
        | RequirementType::Associated(_)
        | RequirementType::AssociatedProjection { .. }
        | RequirementType::MethodParameter(_) => false,
    }
}

fn default_pattern_rows(rows: &[Vec<Pattern>]) -> Vec<Vec<Pattern>> {
    rows.iter()
        .filter(|row| matches!(row.first(), Some(Pattern::Wildcard | Pattern::Binding)))
        .map(|row| row.iter().skip(1).cloned().collect())
        .collect()
}

fn specialize_constant_rows(rows: &[Vec<Pattern>], expected: &Constant) -> Vec<Vec<Pattern>> {
    rows.iter()
        .filter_map(|row| match row.first() {
            Some(Pattern::Wildcard | Pattern::Binding) => {
                Some(row.iter().skip(1).cloned().collect())
            }
            Some(Pattern::Constant(actual)) if actual == expected => {
                Some(row.iter().skip(1).cloned().collect())
            }
            _ => None,
        })
        .collect()
}

fn specialize_variant_rows(
    rows: &[Vec<Pattern>],
    expected_type: crate::TypeId,
    expected_variant: VariantId,
    payload_arity: usize,
) -> Vec<Vec<Pattern>> {
    rows.iter()
        .filter_map(|row| match row.first() {
            Some(Pattern::Wildcard | Pattern::Binding) => {
                let mut specialized = vec![Pattern::Wildcard; payload_arity];
                specialized.extend(row.iter().skip(1).cloned());
                Some(specialized)
            }
            Some(Pattern::Variant {
                ty,
                variant,
                payload,
            }) if *ty == expected_type
                && *variant == expected_variant
                && payload.len() == payload_arity =>
            {
                let mut specialized = payload.clone();
                specialized.extend(row.iter().skip(1).cloned());
                Some(specialized)
            }
            _ => None,
        })
        .collect()
}

fn is_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float)
}

fn nominal_is(ty: &Type, expected: crate::TypeId) -> bool {
    matches!(ty, Type::Nominal(actual, _) if *actual == expected)
}

fn types_compatible(expected: &Type, actual: &Type) -> bool {
    let mut pending = vec![(expected, actual)];
    while let Some((expected, actual)) = pending.pop() {
        match (expected, actual) {
            (_, Type::Never | Type::Error) | (Type::Error, _) => {}
            (Type::Tuple(left), Type::Tuple(right)) => {
                if left.len() != right.len() {
                    return false;
                }
                pending.extend(left.iter().zip(right));
            }
            (Type::Task(left), Type::Task(right)) => pending.push((left, right)),
            (Type::Nominal(left_id, left_args), Type::Nominal(right_id, right_args)) => {
                if left_id != right_id || left_args.len() != right_args.len() {
                    return false;
                }
                pending.extend(left_args.iter().zip(right_args));
            }
            (
                Type::View {
                    mutable: left_mutable,
                    concept: left_concept,
                    bindings: left_bindings,
                },
                Type::View {
                    mutable: right_mutable,
                    concept: right_concept,
                    bindings: right_bindings,
                },
            ) => {
                if left_mutable != right_mutable
                    || left_concept != right_concept
                    || left_bindings.len() != right_bindings.len()
                {
                    return false;
                }
                for (name, left) in left_bindings {
                    let Some(right) = right_bindings.get(name) else {
                        return false;
                    };
                    pending.push((left, right));
                }
            }
            _ if expected == actual => {}
            _ => return false,
        }
    }
    true
}

fn type_schemas_overlap(left: &Type, right: &Type) -> bool {
    let mut pending = vec![(left, right)];
    while let Some((left, right)) = pending.pop() {
        match (left, right) {
            (Type::Parameter(_), _) | (_, Type::Parameter(_)) => {}
            (Type::Tuple(left), Type::Tuple(right)) => {
                if left.len() != right.len() {
                    return false;
                }
                pending.extend(left.iter().zip(right));
            }
            (Type::Task(left), Type::Task(right)) => pending.push((left, right)),
            (Type::Nominal(left_id, left_args), Type::Nominal(right_id, right_args)) => {
                if left_id != right_id || left_args.len() != right_args.len() {
                    return false;
                }
                pending.extend(left_args.iter().zip(right_args));
            }
            (
                Type::View {
                    mutable: left_mutable,
                    concept: left_concept,
                    bindings: left_bindings,
                },
                Type::View {
                    mutable: right_mutable,
                    concept: right_concept,
                    bindings: right_bindings,
                },
            ) => {
                if left_mutable != right_mutable
                    || left_concept != right_concept
                    || left_bindings.len() != right_bindings.len()
                {
                    return false;
                }
                for (name, left) in left_bindings {
                    let Some(right) = right_bindings.get(name) else {
                        return false;
                    };
                    pending.push((left, right));
                }
            }
            _ if left == right => {}
            _ => return false,
        }
    }
    true
}

fn is_strict_type_subterm(candidate: &Type, owner: &Type) -> bool {
    let mut pending = Vec::new();
    match owner {
        Type::Tuple(elements) => pending.extend(elements),
        Type::Nominal(_, arguments) => pending.extend(arguments),
        Type::View { bindings, .. } => pending.extend(bindings.values()),
        Type::Task(output) => pending.push(output.as_ref()),
        _ => {}
    }
    while let Some(current) = pending.pop() {
        if current == candidate {
            return true;
        }
        match current {
            Type::Tuple(elements) => pending.extend(elements),
            Type::Nominal(_, arguments) => pending.extend(arguments),
            Type::View { bindings, .. } => pending.extend(bindings.values()),
            Type::Task(output) => pending.push(output.as_ref()),
            _ => {}
        }
    }
    false
}

fn flow_types_compatible(left: &Type, right: &Type) -> bool {
    *left == Type::Never || *right == Type::Never || types_compatible(left, right)
}

fn proof_bindings_satisfy(
    actual: &BTreeMap<String, Type>,
    expected: &BTreeMap<String, Type>,
) -> bool {
    expected.iter().all(|(name, expected)| {
        actual
            .get(name)
            .is_some_and(|actual| types_compatible(expected, actual))
    })
}

fn substitute_type(ty: &Type, arguments: &[Type]) -> Type {
    match ty {
        Type::Parameter(index) => arguments
            .get(*index as usize)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Tuple(elements) => Type::Tuple(
            elements
                .iter()
                .map(|element| substitute_type(element, arguments))
                .collect(),
        ),
        Type::List(element) => Type::List(Box::new(substitute_type(element, arguments))),
        Type::Task(output) => Type::Task(Box::new(substitute_type(output, arguments))),
        Type::TaskOutcome(output) => {
            Type::TaskOutcome(Box::new(substitute_type(output, arguments)))
        }
        Type::Nominal(id, nested) => Type::Nominal(
            *id,
            nested
                .iter()
                .map(|nested| substitute_type(nested, arguments))
                .collect(),
        ),
        Type::View {
            mutable,
            concept,
            bindings,
        } => Type::View {
            mutable: *mutable,
            concept: *concept,
            bindings: bindings
                .iter()
                .map(|(name, ty)| (name.clone(), substitute_type(ty, arguments)))
                .collect(),
        },
        _ => ty.clone(),
    }
}
